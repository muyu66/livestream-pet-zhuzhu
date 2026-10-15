//! 音频工作线程（DEV.md §5.2 的处理流水线 + §4.4「不得影响音频采集或推理任务」）。
//!
//! 麦克风采集（cpal） → 重采样 → VAD → SenseVoice → 稳定处理 → 标准化事件
//!
//! 关键设计：
//!  * 设备、音频流、VAD、识别器**全部由这条线程持有**。cpal 的 `Stream` 在部分后端
//!    不是 `Send`，而且流应当在创建设备的同一条线程上析构，所以不做跨线程传递。
//!  * 事件出口是一个 `Arc<dyn Fn>` 而不是 `AppHandle`，这样阶段 0 的命令行原型
//!    可以直接复用同一套流水线，无需伪造一个 Tauri 环境（DEV.md §7.8 解耦）。
//!  * 模型加载放在线程内部完成，主线程只负责启动，界面不会被几秒的加载卡住。

use super::capture::{self, AudioChunk};
use super::emotion::Stabilizer;
use super::{asr, vad};
use crate::config::AudioConfig;
use crate::events::{DebugLevel, DebugTranscript, PetDebug, PetEvent, PetEventKind};
use anyhow::{Context, Result};
use crossbeam_channel::{bounded, RecvTimeoutError};
use sherpa_onnx::LinearResampler;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 事件出口。用 Arc<dyn Fn> 解耦，避免音频层依赖 Tauri。
pub type EmitFn = Arc<dyn Fn(PetEvent) + Send + Sync + 'static>;

/// 诊断信息出口。同样解耦：阶段 0 的音频原型也该能用上同一套。
/// 是否真的推给前端由装配层决定（调试面板没开时直接丢掉）。
pub type DebugFn = Arc<dyn Fn(PetDebug) + Send + Sync + 'static>;

/// 采集状态出口。比 `DebugFn` 稀疏得多：一条线程一辈子也就报一两次。
///
/// 为什么不能靠 `DebugFn` 代替：那个出口是「调试面板开着才有」的可选数据，
/// 而「麦克风到底开起来没有」是必须送到装配层的 —— 它决定要不要弹选择框。
pub type StatusFn = Arc<dyn Fn(WorkerStatus) + Send + Sync + 'static>;

/// 一条采集线程的结局，只报关键的那两件事。
///
/// 注意这里**没有**「被要求停止」这一项：用户换麦克风时旧线程必然被停，
/// 那不是故障，不该报出去（报了会让选择框刚选完就弹回一个「失败」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStatus {
    /// 采集真的跑起来了 —— `play()` 已返回，系统开始投递音频。
    /// 只有走到这一步才算「这只麦克风能用」。
    Ready { device_name: String },
    /// 没开起来，或者开着开着断了。字符串是给用户看的说明（含完整因果链）。
    Failed(String),
}

/// 主循环里等待音频块的最长时间。既决定停止响应的粒度，
/// 也决定静音超时检查的轮询频率。
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 电平上报间隔。50 ms 一次足够跟手，又不会把 IPC 灌满 ——
/// 调试面板要的是「有没有声音」，不需要波形级刷新率。
const LEVEL_REPORT_INTERVAL: Duration = Duration::from_millis(100);

/// 「采集流死了」的第二道判据：连续这么久一个音频块都没收到。
///
/// 第一道判据是驱动报 `DeviceNotAvailable`（见 `capture::CaptureStream::device_lost`），
/// 大多数情况够用；但设备被别的程序独占、或驱动悄悄停掉回调时可能一声不吭。
///
/// **安静不等于没数据**：WASAPI 在完全静音时也照常按周期投递缓冲区
/// （`examples/audio_probe.rs` 的 `--level-meter` 正是靠这点观察到「设备在出全 0 的静音」），
/// 所以「收不到块」才是设备死了，「电平是 0」不是。2 秒 ≈ 200 个缓冲区周期，余量足够。
const CHUNK_TIMEOUT: Duration = Duration::from_millis(2000);

pub struct AudioWorker {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

/// 一次运行的结局。与「出错」分开是因为**被要求停止不是故障**：
/// 用户换麦克风时旧线程必然被停掉，那不是要说给用户听的事。
enum Outcome {
    /// 正常结束，或被要求停止（含在模型加载期间就被叫停）
    Stopped,
    Failed(anyhow::Error),
}

impl AudioWorker {
    /// 启动工作线程。立即返回，模型加载在后台进行。
    ///
    /// `device_id` 是要听的那台设备（cpal 的 `DeviceId` 字符串，存在用户配置里）；
    /// `None` 表示系统默认输入设备，只有命令行原型会这么用。
    pub fn spawn(
        emit: EmitFn,
        debug: DebugFn,
        status: StatusFn,
        config: AudioConfig,
        models_dir: PathBuf,
        device_id: Option<String>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();

        let join = std::thread::Builder::new()
            .name("pet-audio".into())
            .spawn(move || {
                match run(
                    &emit,
                    &debug,
                    &status,
                    &stop_for_thread,
                    &config,
                    &models_dir,
                    device_id.as_deref(),
                ) {
                    Outcome::Stopped => {}
                    Outcome::Failed(err) => {
                        // 用 {:#} 展开 anyhow 的因果链，方便定位是缺模型还是缺麦克风
                        log::error!("音频线程结束: {err:#}");
                        status(WorkerStatus::Failed(format!("{err:#}")));
                        // 让前端回到待机，而不是停在最后一个动作上
                        emit(PetEvent::new(PetEventKind::Idle));
                    }
                }
            })
            .expect("创建音频线程失败");

        Self {
            stop,
            join: Some(join),
        }
    }

    /// 请求停止并等待线程退出（DEV.md §3.2 要求退出时完整停掉采集、处理与推理）。
    ///
    /// 带超时：万一驱动把音频流析构卡住，也不能让整个应用退不掉。
    /// 超时后如实返回 false，由调用方决定如何提示。
    pub fn stop(&mut self, timeout: Duration) -> bool {
        self.stop.store(true, Ordering::Relaxed);

        let Some(join) = self.join.take() else {
            return true;
        };

        let (tx, rx) = bounded::<()>(1);
        std::thread::Builder::new()
            .name("pet-audio-join".into())
            .spawn(move || {
                let _ = join.join();
                let _ = tx.send(());
            })
            .expect("创建回收线程失败");

        match rx.recv_timeout(timeout) {
            Ok(()) => true,
            Err(_) => {
                log::warn!("音频线程未在 {:?} 内退出，放弃等待", timeout);
                false
            }
        }
    }
}

impl Drop for AudioWorker {
    fn drop(&mut self) {
        // 兜底：调用方忘了 stop 也不会留下跑不完的线程
        if self.join.is_some() {
            self.stop(Duration::from_secs(2));
        }
    }
}

/// 流水线主体。
fn run(
    emit: &EmitFn,
    debug: &DebugFn,
    status: &StatusFn,
    stop: &Arc<AtomicBool>,
    config: &AudioConfig,
    models_dir: &Path,
    device_id: Option<&str>,
) -> Outcome {
    match run_inner(emit, debug, status, stop, config, models_dir, device_id) {
        Ok(()) => Outcome::Stopped,
        Err(err) => Outcome::Failed(err),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_inner(
    emit: &EmitFn,
    debug: &DebugFn,
    status: &StatusFn,
    stop: &Arc<AtomicBool>,
    config: &AudioConfig,
    models_dir: &Path,
    device_id: Option<&str>,
) -> Result<()> {
    // ---- 0. 存档里那台设备还在不在。放在模型加载**之前**问，
    //      否则「麦克风被拔了」这件事要等一秒多才报出来，那期间的界面像死了一样。
    //      只问不建流：真正的解析与打开在下面那条线程上做（cpal 的规矩）。
    if let Some(id) = device_id {
        if !capture::device_is_available(id) {
            anyhow::bail!("存档的麦克风现在找不到了（可能被拔掉、换了 USB 口或改名了）: {id}");
        }
    }

    // ---- 1. 加载模型。失败就尽早报出来，而不是等采集跑起来之后才发现。
    log::info!("正在加载本地语音模型…");
    let load_started = Instant::now();
    let vad = vad::Vad::new(&config.vad, config.sample_rate, models_dir)?;
    let recognizer = asr::Recognizer::new(&config.recognizer, config.sample_rate, models_dir)?;
    log::info!("模型加载完成，总耗时 {:.0} ms", load_started.elapsed().as_millis());

    // 模型加载是不打断的（几秒的 C++ 推理初始化，没有中途放弃的接口），
    // 所以在这之后补一次停止检查：用户在这期间点了别的设备时，
    // 这条线程应该安静退场，而不是把麦克风开起来再关掉 ——
    // 那会与新的那条流短暂地同时占着设备。
    if stop.load(Ordering::Relaxed) {
        log::info!("加载模型期间收到停止信号，不再打开麦克风");
        return Ok(());
    }

    let mut stabilizer = Stabilizer::new(config.stability.clone(), Instant::now());

    // ---- 2. 打开麦克风。流由本线程持有，退出时在这里析构。
    let (tx, rx) = bounded::<AudioChunk>(capture::CAPTURE_CHANNEL_CAPACITY);
    let capture = capture::build_input_stream(tx, device_id)?;
    log::info!(
        "麦克风: {}（{} Hz, {} 声道, {:?}）",
        capture.device_name,
        capture.sample_rate,
        capture.channels,
        capture.sample_format
    );

    let resampler = if capture.sample_rate as i32 != config.sample_rate {
        log::info!(
            "重采样 {} Hz → {} Hz",
            capture.sample_rate,
            config.sample_rate
        );
        Some(
            LinearResampler::create(capture.sample_rate as i32, config.sample_rate)
                .context("创建重采样器失败")?,
        )
    } else {
        None
    };

    capture.play()?;
    log::info!("开始聆听（静音 {} ms 后进入待机）", config.stability.idle_timeout_ms);

    // 到这一步才算「这只麦克风能用」：设备打开了、流也真的跑起来了。
    // 装配层收到它才会把状态置成「正在聆听」，也才不会弹选择框。
    status(WorkerStatus::Ready {
        device_name: capture.device_name.clone(),
    });

    let mut session = Session::new(
        config,
        vad.window_size(),
        capture.dropped_chunks.clone(),
        capture.capture_errors.clone(),
    );

    // ---- 3. 主循环
    //
    // 掉线是**记下来再跳出循环**，不是当场 `bail!`：收尾那段要把「驱动错误通知」
    // 与「未接住的音频块」的次数报出来，掉线时恰恰最需要这两个数
    // （它们能区分「设备没了」和「我们处理不过来」）。直接 return 会把它们丢掉。
    let mut failure: Option<anyhow::Error> = None;
    let mut last_chunk = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            log::info!("收到停止信号，结束音频采集");
            break;
        }

        // 掉线判定。两条判据缺一不可，理由见各自的定义处（device_lost / CHUNK_TIMEOUT）。
        if capture.device_lost.load(Ordering::Relaxed) {
            failure = Some(anyhow::anyhow!(
                "麦克风「{}」在采集过程中断开",
                capture.device_name
            ));
            break;
        }
        if last_chunk.elapsed() >= CHUNK_TIMEOUT {
            failure = Some(anyhow::anyhow!(
                "麦克风「{}」超过 {} 秒没有送来任何音频，按断开处理",
                capture.device_name,
                CHUNK_TIMEOUT.as_secs()
            ));
            break;
        }

        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(chunk) => {
                last_chunk = Instant::now();
                let samples = match &resampler {
                    Some(r) => r.resample(&chunk, false),
                    None => chunk,
                };
                session.push_audio(&samples);
                session.process_audio(&vad, &recognizer, &mut stabilizer, emit, debug);
                session.report_level(debug);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                // **通道断开不等于「正常停止」**，这是踩过的坑（2026-10-15 实测）：
                // 用户拔掉麦克风时，设备层先报 DeviceNotAvailable 置上 `device_lost`，
                // 紧接着 cpal 把数据回调连同它持有的 `tx` 一起析构 —— 于是
                // `recv_timeout` 立刻返回 `Disconnected`，**赶在循环下一次回头检查
                // `device_lost` 之前**把控制流抢走了。
                //
                // 这里原来直接 break（`failure` 保持 `None`），于是一次掉线被当成
                // 「用户让我们停」上报，装配层收不到 `Failed`：不记错误、不弹选择框，
                // 状态永远停在「正在聆听」，界面上只剩一个不再跳的电平条。
                //
                // 判据是 `stop`：正常停止一定会先置它，那时循环顶上就 break 了，
                // 根本走不到这里。所以走到这里要么是设备没了，要么是采集侧自己散了 ——
                // 两种都不该被当成正常。
                if stop.load(Ordering::Relaxed) {
                    log::info!("音频采集通道已关闭（正在停止）");
                    break;
                }
                failure = Some(anyhow::anyhow!(
                    "麦克风「{}」的采集通道意外断开（设备可能已经不可用）",
                    capture.device_name
                ));
                break;
            }
        }

        let now = Instant::now();
        if let Some(kind) = stabilizer.poll_idle(now) {
            emit(PetEvent::new(kind));
        }
    }

    // ---- 4. 收尾：显式析构音频流，确保设备被释放
    //
    // 两个计数要分开说，它们含义完全不同：
    //  * capture_errors 是设备层（WASAPI）自己报的通知，多与设备状态、系统负载有关；
    //  * dropped_chunks 是**我们真的没接住**的音频，才是流水线的问题。
    // 混在一起报会让人误判。
    let errors = capture.capture_errors.load(Ordering::Relaxed);
    let dropped = capture.dropped_chunks.load(Ordering::Relaxed);
    drop(capture);
    if errors > 0 {
        log::info!("采集期间收到 {errors} 次驱动错误通知（不影响本次运行的可用性）");
    }
    if dropped > 0 {
        log::warn!("本次运行共丢弃 {dropped} 个音频块（处理跟不上采集）");
    }
    log::info!("麦克风已释放");

    match failure {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// 一次采集会话的缓冲与节流状态。
struct Session {
    /// 已送进 VAD 但还没凑够一个窗口的余量
    pending: Vec<f32>,
    /// 最近若干秒的音频，供中间解码使用
    recent: Vec<f32>,
    recent_cap: usize,

    window_size: usize,
    /// VAD 是否已经报告过本段语音开始
    speech_active: bool,
    /// 本段语音已累计的采样点数，用来判断够不够长做中间解码
    speech_samples: usize,
    /// 上次中间解码的时刻
    last_interim: Option<Instant>,
    interim_interval: Duration,
    /// 开始做中间解码所需的本段语音时长（秒）
    interim_min_speech: f32,
    /// 样本率，仅用于把样本数换算成秒数做日志
    sample_rate: f32,

    // ---- 诊断电平。放在 Session 里而不是采集回调里：
    //      回调跑在驱动的实时线程上，多算一点都可能引起丢块（DEV.md §5.2）。
    level_peak: f32,
    level_sq_sum: f64,
    level_samples: usize,
    last_level_report: Option<Instant>,
    /// 借采集流的计数用一下，让调试面板也能看到「我们漏了多少音频」
    dropped: Arc<AtomicU64>,
    capture_errors: Arc<AtomicU64>,
}

impl Session {
    fn new(
        config: &AudioConfig,
        window_size: usize,
        dropped: Arc<AtomicU64>,
        capture_errors: Arc<AtomicU64>,
    ) -> Self {
        let rate = config.sample_rate.max(1) as f32;
        Self {
            pending: Vec::new(),
            recent: Vec::new(),
            recent_cap: (config.recognizer.interim_window_secs.max(0.5) * rate) as usize,
            window_size,
            speech_active: false,
            speech_samples: 0,
            last_interim: None,
            interim_interval: Duration::from_millis(config.recognizer.interim_interval_ms),
            interim_min_speech: config.recognizer.interim_min_speech_secs,
            sample_rate: rate,
            level_peak: 0.0,
            level_sq_sum: 0.0,
            level_samples: 0,
            last_level_report: None,
            dropped,
            capture_errors,
        }
    }

    fn push_audio(&mut self, samples: &[f32]) {
        self.pending.extend_from_slice(samples);
        self.recent.extend_from_slice(samples);
        if self.recent.len() > self.recent_cap {
            let excess = self.recent.len() - self.recent_cap;
            self.recent.drain(..excess);
        }

        // 只统计语音期间的样本：`recent` 里含本段之前的静音，
        // 拿它算「这段说了多久」会把静音也算进去
        if self.speech_active {
            self.speech_samples += samples.len();
        }

        for &s in samples {
            let a = s.abs();
            if a > self.level_peak {
                self.level_peak = a;
            }
            self.level_sq_sum += f64::from(s) * f64::from(s);
        }
        self.level_samples += samples.len();
    }

    /// 按固定间隔上报一次电平，然后清零重新累计。
    ///
    /// 报的是**这段时间内的峰值**而不是瞬时值：识别要能成功，
    /// 靠的是整段音频的幅度，瞬时采样经常正好落在过零点上。
    fn report_level(&mut self, debug: &DebugFn) {
        let due = match self.last_level_report {
            None => true,
            Some(last) => last.elapsed() >= LEVEL_REPORT_INTERVAL,
        };
        if !due {
            return;
        }
        self.last_level_report = Some(Instant::now());

        if self.level_samples == 0 {
            return;
        }
        let rms = (self.level_sq_sum / self.level_samples as f64).sqrt() as f32;
        let level = DebugLevel {
            peak: self.level_peak,
            rms,
            dropped: self.dropped.load(Ordering::Relaxed),
            capture_errors: self.capture_errors.load(Ordering::Relaxed),
        };

        self.level_peak = 0.0;
        self.level_sq_sum = 0.0;
        self.level_samples = 0;

        debug(PetDebug::Level(level));
    }

    fn process_audio(
        &mut self,
        vad: &vad::Vad,
        recognizer: &asr::Recognizer,
        stabilizer: &mut Stabilizer,
        emit: &EmitFn,
        debug: &DebugFn,
    ) {
        // ---- 按固定窗口喂给 VAD（Silero 要求 512 采样点）
        let mut consumed = 0usize;
        while self.pending.len() - consumed >= self.window_size {
            vad.accept(&self.pending[consumed..consumed + self.window_size]);
            consumed += self.window_size;
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }

        // ---- 语音开始
        if !self.speech_active && vad.detected() {
            self.speech_active = true;
            self.speech_samples = 0;
            self.last_interim = None;
            for kind in stabilizer.on_voice_start(Instant::now()) {
                emit(PetEvent::new(kind));
            }
        }

        // ---- 中间解码：SenseVoice 是离线模型，不这样做的话长句子要等到
        //      语音段切分（最长 max_speech_duration）才有情绪反馈。
        //
        //      两个门槛缺一不可：间隔（多久解一次）+ 本段已说的时长（够长才值得解）。
        //      只看间隔的话，短句会在语音刚被确认时就解一次 —— 那时 `recent` 里
        //      基本还是上一句之后的静音，解出来是空文本；而且短句的「最近 N 秒」
        //      窗口就等于整段音频，紧接着的语音段解码会给出完全一样的结果，
        //      同一次说话白解两遍。详见 config.rs 的 `interim_min_speech_secs`。
        if self.speech_active && !self.interim_interval.is_zero() {
            let speech_secs = self.speech_samples as f32 / self.sample_rate;
            let due = match self.last_interim {
                None => true,
                Some(last) => last.elapsed() >= self.interim_interval,
            };
            if due && speech_secs >= self.interim_min_speech && !self.recent.is_empty() {
                self.last_interim = Some(Instant::now());
                if let Some(t) = recognizer.transcribe(&self.recent) {
                    log::trace!(
                        "[中间] {} ms | {} | {} | {} | {}",
                        t.elapsed_ms,
                        t.lang_tag,
                        t.emotion_tag,
                        t.event_tag,
                        t.text
                    );
                    debug(PetDebug::Transcript(DebugTranscript {
                        segment: false,
                        text: t.text.clone(),
                        lang: t.lang_tag.clone(),
                        emotion: t.emotion_tag.clone(),
                        event: t.event_tag.clone(),
                        audio_secs: self.recent.len() as f32 / self.sample_rate,
                        elapsed_ms: t.elapsed_ms as u64,
                    }));
                    for kind in stabilizer.on_transcript(&t.emotion_tag, &t.event_tag, Instant::now())
                    {
                        emit(PetEvent::new(kind));
                    }
                }
            }
        }

        // ---- 语音段结束：做一次权威解码
        while !vad.is_empty() {
            let Some(segment) = vad.take_segment() else {
                break;
            };

            let started = Instant::now();
            match recognizer.transcribe(&segment) {
                Some(t) => {
                    log::info!(
                        "[语音段] 音频 {:.2}s | 推理 {} ms | {} | {} | {} | {}",
                        segment.len() as f32 / self.sample_rate,
                        t.elapsed_ms,
                        t.lang_tag,
                        t.emotion_tag,
                        t.event_tag,
                        t.text
                    );
                    for kind in stabilizer.on_transcript(&t.emotion_tag, &t.event_tag, Instant::now())
                    {
                        emit(PetEvent::new(kind));
                    }
                    debug(PetDebug::Transcript(DebugTranscript {
                        segment: true,
                        text: t.text.clone(),
                        lang: t.lang_tag.clone(),
                        emotion: t.emotion_tag.clone(),
                        event: t.event_tag.clone(),
                        audio_secs: segment.len() as f32 / self.sample_rate,
                        elapsed_ms: t.elapsed_ms as u64,
                    }));
                }
                None => log::warn!(
                "语音段解码没有返回结果（{:.2}s）",
                segment.len() as f32 / self.sample_rate
            ),
            }
            log::debug!("语音段处理总耗时 {} ms", started.elapsed().as_millis());

            self.speech_active = false;
            self.speech_samples = 0;
            self.last_interim = None;
            self.recent.clear();
            self.pending.clear();

            for kind in stabilizer.on_voice_end(Instant::now()) {
                emit(PetEvent::new(kind));
            }
        }
    }
}
