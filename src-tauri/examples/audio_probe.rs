//! 阶段 0：音频技术验证原型（DEV.md §6「阶段 0」）。
//!
//! 不集成桌面 UI，只验证一件事：**这条本地语音路线在本机真的能跑**。
//! 具体要回答：
//!   * 能否正常打开和释放麦克风
//!   * 能否识别基本语音与静音
//!   * 能否实际输出情绪及声音事件结果（而不是只看模型介绍）
//!   * 本机资源占用、推理延迟、模型部署依赖
//!
//! 它与正式应用**共用同一套流水线**（`pet::audio`），所以这里跑通的结果
//! 对阶段 3 的集成是有效的，不是另写一份原型。
//!
//! 用法：
//!   cargo run --example audio_probe
//!   cargo run --example audio_probe -- --list-devices
//!   cargo run --example audio_probe -- --device "wasapi:{0.0.1...}.{guid}"
//!   cargo run --example audio_probe -- --models-dir D:/models
//!   cargo run --example audio_probe -- --seconds 90 --level-meter
//!
//! `--device <id>` 用 `--list-devices` 打出来的那串 id 指定麦克风（省略则用系统默认，
//! 这是**只有这个原型会做的事**：正式应用永远按用户选定的设备打开）。
//! 它也是验证「存档里的 id 能不能把那台设备找回来」的最短路径 ——
//! 与正式应用走的是同一个 `build_input_stream`。
//!
//! `--level-meter` 会另开一路采集专门打印输入电平。它回答的是排查时最先要问的
//! 那个问题：**麦克风到底有没有在出信号**。当「说了话却识别不到」时，
//! 电平表能立刻把「设备没工作」和「识别没命中」区分开。
//!
//! 建议用 `RUST_LOG=debug` 打开中间解码日志，可以同时观察识别延迟与稳定性。

use livestream_pet_zhuzhu::audio::capture;
use livestream_pet_zhuzhu::audio::{AudioWorker, WorkerStatus};
use livestream_pet_zhuzhu::config::AudioConfig;
use livestream_pet_zhuzhu::events::{PetDebug, PetEvent};
use livestream_pet_zhuzhu::paths;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 运行多久后自动结束。设 0 表示一直跑到 Ctrl+C。
const DEFAULT_DURATION_SECS: u64 = 60;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--list-devices") {
        list_devices();
        return;
    }

    let models_dir = match arg_value(&args, "--models-dir") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => paths::models_dir(),
    };
    let duration = arg_value(&args, "--seconds")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DURATION_SECS);
    let with_level_meter = args.iter().any(|a| a == "--level-meter");
    let device_id = arg_value(&args, "--device");

    let config = AudioConfig::load();

    println!("==========================================================");
    println!(" 直播宠物 · 阶段 0 音频验证原型");
    println!("==========================================================");
    println!("模型目录   : {}", models_dir.display());
    println!(
        "麦克风     : {}",
        device_id.as_deref().unwrap_or("（系统默认）")
    );
    println!("采样率     : {} Hz", config.sample_rate);
    println!("静音超时   : {} ms", config.stability.idle_timeout_ms);
    println!(
        "中间解码   : 每 {} ms / 最近 {:.1}s",
        config.recognizer.interim_interval_ms, config.recognizer.interim_window_secs
    );
    println!("运行时长   : {duration} 秒（0 表示直到 Ctrl+C）");
    println!("----------------------------------------------------------");
    println!("请对着麦克风说话、笑、用不同语气说话，观察下面的识别结果。");
    println!();

    // 事件出口：这里只是打日志，正式应用里换成 Tauri 事件通道。
    // 同一个 EmitFn 签名，说明阶段 3 的集成只替换这一行。
    let started_at = Instant::now();
    let event_count = Arc::new(AtomicU64::new(0));
    let counter = event_count.clone();
    let emit = Arc::new(move |event: PetEvent| {
        let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
        let elapsed = started_at.elapsed();
        println!(
            "[事件 #{n:>4}] +{:<7} {:?}",
            format!("{:.2}s", elapsed.as_secs_f32()),
            event.kind
        );
    });

    // 用 ctrlc 而不是自己读 stdin：需要的是一个「不阻塞主循环的退出信号」，
    // 阻塞式读 stdin 会让计时逻辑失效。
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_handler = stop.clone();
    if let Err(err) = ctrlc::set_handler(move || {
        stop_for_handler.store(true, Ordering::SeqCst);
    }) {
        eprintln!("注册 Ctrl+C 处理失败（仍可用 --seconds 控制时长）: {err}");
    }

    // 诊断出口：正式应用里这些数据走 Tauri 推给调试面板，这里直接打屏。
    // 电平按 1 Hz 打，避免刷屏；识别结果（含中间解码）全打，
    // 中间解码正是「模型到底听到了什么」的现场。
    let last_level = Arc::new(std::sync::Mutex::new(Instant::now()));
    let level_gate = last_level.clone();
    let debug = Arc::new(move |info: PetDebug| match info {
        PetDebug::Level(level) => {
            let mut last = match level_gate.lock() {
                Ok(last) => last,
                Err(_) => return,
            };
            if last.elapsed() < Duration::from_secs(1) {
                return;
            }
            *last = Instant::now();
            println!(
                "[电平] 峰值 {:>7.2} dBFS | 有效值 {:>7.2} dBFS | 丢弃 {} 块 | 驱动错误 {} 次",
                dbfs(level.peak),
                dbfs(level.rms),
                level.dropped,
                level.capture_errors
            );
        }
        PetDebug::Transcript(t) => println!(
            "[{}] 音频 {:.2}s | 推理 {} ms | {} | {} | {} | {}",
            if t.segment { "语音段" } else { "中间  " },
            t.audio_secs,
            t.elapsed_ms,
            t.lang,
            t.emotion,
            t.event,
            t.text
        ),
    });

    // 状态出口：这里也是打日志。正式应用里它决定「麦克风到底开起来没有」，
    // 以及要不要把选择框弹出来。
    let status = Arc::new(|status: WorkerStatus| match status {
        WorkerStatus::Ready { device_name } => println!("[状态] 正在聆听: {device_name}"),
        WorkerStatus::Failed(reason) => eprintln!("[状态] 没听成: {reason}"),
    });

    let mut worker = AudioWorker::spawn(emit, debug, status, config, models_dir, device_id);
    println!("（模型加载中，请稍候…）");

    let level_meter = if with_level_meter {
        Some(spawn_level_meter(stop.clone()))
    } else {
        None
    };

    let deadline = if duration > 0 {
        Some(Instant::now() + Duration::from_secs(duration))
    } else {
        None
    };

    loop {
        if stop.load(Ordering::SeqCst) {
            println!("\n收到 Ctrl+C，准备退出…");
            break;
        }
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                println!("\n到达设定时长，准备退出…");
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // 先置停止位，让电平表线程也一起收工（超时退出这条路径不会经过 Ctrl+C 处理器）
    stop.store(true, Ordering::SeqCst);

    println!("\n正在停止采集并释放麦克风…");
    let stopped = worker.stop(Duration::from_secs(5));
    if let Some(meter) = level_meter {
        let _ = meter.join();
    }
    println!(
        "{}  共产生 {} 个标准化事件",
        if stopped {
            "音频线程已正常退出。"
        } else {
            "音频线程超时未退出（需要关注）。"
        },
        event_count.load(Ordering::Relaxed)
    );
}

/// 另开一路采集，每秒打印一次输入峰值电平（dBFS）。
///
/// 单独一条线程、单独一个流：它不参与正式流水线，也不影响 worker 那边的测量。
/// 采集流的创建与析构都在这条线程上完成 —— 这是 cpal 的要求（见 `capture` 的文档）。
///
/// 注意：同一個 WASAPI 设备上开两路采集时，偶发 `buffer underrun or overrun`
/// 是正常的，属于本诊断工具引入的现象。正式应用只有一路采集，不会出现 ——
/// 这个差异已实测确认，别把它当成生产路径的问题。
fn spawn_level_meter(stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("probe-level".into())
        .spawn(move || {
            let (tx, rx) = crossbeam_channel::bounded::<capture::AudioChunk>(8);
            // 电平表是另一路诊断，不跟着 --device 走：它要回答的是「系统默认那只麦
            // 到底有没有在出信号」，与正式采集用哪只无关。
            let stream = match capture::build_input_stream(tx, None) {
                Ok(stream) => stream,
                Err(err) => {
                    eprintln!("[电平表] 打开采集失败: {err:#}");
                    return;
                }
            };
            if let Err(err) = stream.play() {
                eprintln!("[电平表] 启动采集失败: {err}");
                return;
            }
            println!(
                "[电平表] 已启用（{}，{} Hz, {} 声道, {:?}）：每秒打印一次输入峰值。",
                stream.device_name, stream.sample_rate, stream.channels, stream.sample_format
            );

            let mut peak = 0f32;
            let mut since = Instant::now();
            while !stop.load(Ordering::SeqCst) {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(chunk) => {
                        for &sample in &chunk {
                            let abs = sample.abs();
                            if abs > peak {
                                peak = abs;
                            }
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }

                if since.elapsed() >= Duration::from_secs(1) {
                    let dbfs = if peak > 0.0 {
                        20.0 * peak.log10()
                    } else {
                        f32::NEG_INFINITY
                    };
                    // -60 dBFS 以下基本等同于「设备在出全 0 的静音」
                    let hint = if dbfs < -60.0 {
                        "  ← 几乎无声"
                    } else if dbfs > -3.0 {
                        "  ← 接近削顶"
                    } else {
                        ""
                    };
                    println!("[电平表] 峰值 {dbfs:.1} dBFS{hint}");
                    peak = 0.0;
                    since = Instant::now();
                }
            }

            drop(stream); // 在这条线程上释放设备
        })
        .expect("创建电平表线程失败")
}

fn list_devices() {
    println!("可用的输入设备：");
    match capture::list_input_devices() {
        Ok(devices) if devices.is_empty() => {
            println!("  （没有找到任何输入设备）");
        }
        Ok(devices) => {
            for device in devices {
                println!(
                    "  {}{}\n      id: {}",
                    if device.is_default { "* " } else { "  " },
                    device.name,
                    device.id
                );
            }
            println!("\n(* 表示默认设备)");
            println!("把某台的 id 抄给 --device 就能只测它（正式应用按同一个 id 记住用户的选择）。");
        }
        Err(err) => {
            eprintln!("枚举输入设备失败: {err:#}");
            std::process::exit(1);
        }
    }
}

/// 线性幅度 → dBFS。
///
/// 用 dB 而不是直接打 0.0037 这种数字：人眼对小数幅度没有直觉，
/// 而「-48 dBFS 基本等于静音、-20 dBFS 是正常说话」是可以直接判断的。
/// 0 要单独处理，否则 log10(0) 得到 -inf。
fn dbfs(amplitude: f32) -> f32 {
    if amplitude <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * amplitude.log10()
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == flag)?;
    args.get(idx + 1).cloned()
}
