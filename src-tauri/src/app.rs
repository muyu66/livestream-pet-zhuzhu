//! Tauri 应用装配：托盘、状态、窗口位置、音频线程生命周期、退出清理。

use crate::audio::{AudioWorker, DebugFn, EmitFn, StatusFn, WorkerStatus};
use crate::commands;
use crate::config::AudioConfig;
use crate::events::{
    AudioChoice, AudioStatus, PetDebug, PetEvent, PetEventKind, PET_ALWAYS_ON_TOP_CHANNEL,
    PET_AUDIO_CHANNEL, PET_DEBUG_CHANNEL, PET_EVENT_CHANNEL, PET_LEVEL_CHANNEL,
    PET_SHUTDOWN_CHANNEL,
};
use crate::paths;
use crate::tray;
use crate::window_state::{self, DeviceChoice};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, RunEvent};

/// 退出时等待音频线程收尾的上限。
/// 给足一次推理的时间，又不至于让应用退不掉。
const AUDIO_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(3000);

/// 主窗口的 label，与 tauri.conf.json 里保持一致
pub const MAIN_WINDOW: &str = "main";

/// 全局状态。音频线程的句柄放在这里，退出时取出来优雅停止。
pub struct AppState {
    pub worker: Mutex<Option<AudioWorker>>,
    /// 「用的是哪只麦克风、现在在不在听」。选择框、调试面板、角色窗口都读它。
    pub audio: Mutex<AudioChoice>,
    /// 每换一次采集线程就 +1，用来分辨一条报告还算不算数。
    ///
    /// 换麦克风时旧线程可能过一两秒才把它那条「设备没了」报上来，
    /// 那时用户已经选好新设备了 —— 没有这个计数，那份迟到的报告会把新状态覆盖成失败，
    /// 表现就是「刚选完设备，选择框自己又弹出来说你选的不能用」。
    /// 规则：报告前先比代数，对不上就丢掉。
    pub audio_generation: AtomicU64,
    /// 是否正在建选择框窗口。
    ///
    /// 建窗是异步投递给事件循环的，从「决定要建」到「窗口真的存在」之间有一段空档；
    /// 这期间再点一次「选择麦克风」会发现窗口还不存在，于是又建一次 ——
    /// 第二次会以 `WindowLabelAlreadyExists` 失败并在日志里留下一条 error。
    /// 这个标记把那段空档堵上。
    pub picker_opening: AtomicBool,
    /// 选择框是否打开。打开时才推实时电平 —— 没人看着的时候白走 IPC，
    /// 而且电平是 10 Hz 的高频数据，比别的诊断信息更值得省。
    ///
    /// **不能复用 `debug`**：那个开关归调试窗口管（见 `App.vue` 里记的那个坑），
    /// 两个窗口互相开关对方的数据流必然出怪事。
    pub meter: AtomicBool,
    /// 当前是否置顶。托盘菜单和前端自绘菜单都要读它来决定文案是
    /// 「置顶窗口」还是「取消置顶」，所以状态必须只有这一份。
    pub always_on_top: AtomicBool,
    /// 调试面板是否打开。打开时才把诊断信息推给前端 ——
    /// 正常运行时这些数据没人看，白走一趟 IPC 不划算。
    pub debug: AtomicBool,
}

pub fn run() {
    // 默认 info 级别；排查识别问题时可以用 RUST_LOG=debug 拿到中间解码日志
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let app = tauri::Builder::default()
        .manage(AppState {
            worker: Mutex::new(None),
            audio: Mutex::new(AudioChoice::off()),
            audio_generation: AtomicU64::new(0),
            picker_opening: AtomicBool::new(false),
            meter: AtomicBool::new(false),
            always_on_top: AtomicBool::new(false),
            debug: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            commands::exit_app,
            commands::toggle_always_on_top,
            commands::menu_items,
            commands::set_debug,
            commands::toggle_debug_window,
            commands::list_input_devices,
            commands::audio_state,
            commands::choose_audio_device,
            commands::set_meter,
            commands::open_mic_picker,
            commands::close_mic_picker
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // 用户配置只读一次，两节分给各自的装配步骤
            let settings = window_state::load(&handle);

            // 先定位再显示：配置里窗口是 visible:false 的
            restore_window(&handle, settings.window);

            // 托盘是主要的常驻入口；失败不致命，但必须留下明确日志
            if let Err(err) = tray::build(&handle) {
                log::error!("创建系统托盘失败: {err}");
            }

            // 音频放在最后：它可能弹选择框，而弹框要求主窗口已经存在（否则弹在空屏上）
            apply_audio(&handle, settings.audio);

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("创建 Tauri 应用失败");

    app.run(|handle, event| {
        // 兜底：任何退出路径（托盘、右键菜单、Alt+F4、系统注销）
        // 都必须先记住窗口状态、再停掉麦克风与推理线程，不留后台任务。
        //
        // 顺序有讲究：位置与置顶要在窗口还活着的时候读，所以放在 shutdown 之前。
        if let RunEvent::ExitRequested { .. } = event {
            remember_window_state(handle);
            shutdown(handle);
        }
    });
}

/// 应用上次记住的窗口状态（位置 + 置顶），然后显示窗口。
///
/// 窗口在 `tauri.conf.json` 里是 `visible: false`：
/// 定义在配置里的窗口会在 `setup` 之前就创建出来，若一开始就可见，
/// 用户会看到它先在屏幕中央闪一下再跳到目标位置。
/// 先隐藏、定位、再显示，就完全没有这个过程 —— 从用户视角依旧是「启动即显示」。
fn restore_window(handle: &AppHandle, saved: Option<window_state::SavedWindow>) {
    let Some(window) = handle.get_webview_window(MAIN_WINDOW) else {
        log::error!("找不到主窗口 {MAIN_WINDOW}，无法恢复窗口状态");
        return;
    };

    if let Some(saved) = saved {
        match window.outer_size() {
            Ok(size) => match window_state::resolve_restore_position(handle, saved, size) {
                Some(pos) => match window.set_position(pos) {
                    Ok(()) => log::info!("已恢复上次的窗口位置: ({}, {})", pos.x, pos.y),
                    Err(err) => log::warn!("恢复窗口位置失败，改为居中: {err}"),
                },
                None => log::warn!(
                    "上次的位置 ({}, {}) 无法还原到任何显示器上，改为居中显示",
                    saved.x,
                    saved.y
                ),
            },
            // 拿不到尺寸就没法判断会不会被裁掉，宁可不动位置
            Err(err) => log::warn!("读取窗口尺寸失败，改为居中显示: {err}"),
        }
    } else {
        log::info!("没有窗口状态存档，按配置居中显示");
    }

    if let Err(err) = window.show() {
        log::error!("显示主窗口失败: {err}");
    }

    if let Some(saved) = saved {
        // 置顶与位置相互独立：位置还原失败不代表置顶也要放弃。
        //
        // 顺序上必须放在 `show()` **之后**：z-order 是发给窗口管理器的请求，
        // 窗口还藏着的时候下这条命令，随后的 `ShowWindow` 有可能把它覆盖掉 ——
        // 表现出来就是「窗口开着、却被别的窗口压住，点一下任务栏才露出来」。
        //
        // 也不能复用手写的 `toggle_always_on_top`：那个函数是「取反」，
        // 用它在启动时套用存档会把状态翻成相反的。
        if saved.always_on_top {
            if let Err(err) = window.set_always_on_top(true) {
                log::warn!("恢复置顶状态失败: {err}");
            } else {
                // 写回状态：托盘菜单紧接着就要建（tray::build 读的就是这个值），
                // 漏了这一步会出现「窗口已经置顶、托盘菜单却还写着『置顶窗口』」。
                handle
                    .state::<AppState>()
                    .always_on_top
                    .store(true, Ordering::Relaxed);
                log::info!("已恢复置顶状态: 开");
            }
        } else {
            log::info!("上次退出时未置顶，保持不置顶");
        }
    }
}

/// 把当前窗口状态写进存档。窗口已经销毁时安静跳过。
fn remember_window_state(handle: &AppHandle) {
    let Some(window) = handle.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    match window.outer_position() {
        Ok(pos) => {
            let always_on_top = handle
                .state::<AppState>()
                .always_on_top
                .load(Ordering::Relaxed);
            window_state::save_window(handle, pos, always_on_top)
        }
        Err(err) => log::warn!("读取窗口位置失败，本次不保存: {err}"),
    }
}

/// 切换置顶。返回切换后的状态。
///
/// 托盘菜单与前端右键菜单都走这一个函数，保证两条路径行为一致：
/// 改窗口 → 改状态 → 刷新托盘菜单文案 → 通知前端。
pub fn toggle_always_on_top(handle: &AppHandle) -> Result<bool, String> {
    let window = handle
        .get_webview_window(MAIN_WINDOW)
        .ok_or_else(|| format!("找不到主窗口 {MAIN_WINDOW}"))?;

    let state = handle.state::<AppState>();
    let next = !state.always_on_top.load(Ordering::Relaxed);

    window
        .set_always_on_top(next)
        .map_err(|err| format!("设置置顶失败: {err}"))?;
    state.always_on_top.store(next, Ordering::Relaxed);

    // 托盘菜单的文案要跟着变，否则会出现「菜单写着置顶、窗口却已经置顶」
    if let Err(err) = tray::refresh_menu(handle) {
        // 只影响文案刷新，不影响置顶本身，所以不往上抛
        log::warn!("刷新托盘菜单失败: {err}");
    }

    // 前端自绘菜单靠这个事件同步文案。前端没起来时 emit 会失败，属正常情况。
    if let Err(err) = handle.emit(PET_ALWAYS_ON_TOP_CHANNEL, next) {
        log::debug!("通知前端置顶状态失败（前端可能尚未就绪）: {err}");
    }

    log::info!("置顶已{}", if next { "开启" } else { "关闭" });
    Ok(next)
}


/// 按用户配置决定音频怎么起。三条路，规则只有这三条（别再往里加）：
///
///  * **整节不存在** = 从来没配过 → 不建线程，弹选择框让人现在挑；
///  * **`input_device` 是 `null`** = 明确选了不监听 → 语气分析整套不启用，也不弹框；
///  * **有设备 id** → 按它启动。设备是不是真的还在不在这一层判断：
///    真正的解析、打开都在工作线程上做（cpal 的规矩，也避免在启动时于主线程枚举设备），
///    连不上时线程会报 `Failed`，那时再弹框 —— 与「用着用着掉线」走同一条路。
fn apply_audio(handle: &AppHandle, saved: Option<window_state::SavedAudio>) {
    match saved {
        None => {
            log::info!("没有配置过麦克风，弹出选择框让用户选择");
            set_audio(handle, AudioChoice::need_choice());
            spawn_mic_picker(handle);
        }
        Some(audio) if audio.input_device.is_none() => {
            log::info!("用户选择了不监听：语气分析不启用");
            set_audio(handle, AudioChoice::off());
        }
        Some(audio) => {
            let device = audio.input_device.expect("上面这个分支就是「有设备」");
            log::info!("已恢复麦克风: {}（{}）", device.name, device.id);
            start_audio(handle, device);
        }
    }
}

/// 启动本地语音分析线程，听 `device` 这台设备。
///
/// 注意这里**不会**因为麦克风或模型有问题而失败：
/// 采集与模型加载都在工作线程内部进行，出错时只记录日志（并通过状态出口报回装配层），
/// 保证「没有麦克风也能启动角色」——桌宠的动画不应该被音频问题拖死。
fn start_audio(handle: &AppHandle, device: DeviceChoice) {
    let config = AudioConfig::load();
    let models_dir = paths::models_dir();
    log::info!("模型目录: {}", models_dir.display());

    // 这一代线程的编号，下面的状态出口靠它分辨报告是不是自己的
    let generation = handle
        .state::<AppState>()
        .audio_generation
        .fetch_add(1, Ordering::SeqCst)
        + 1;

    set_audio(
        handle,
        AudioChoice {
            device: Some(device.clone()),
            status: AudioStatus::Connecting,
        },
    );

    let emit_handle = handle.clone();
    let emit: EmitFn = Arc::new(move |event: PetEvent| {
        if let Err(err) = emit_handle.emit(PET_EVENT_CHANNEL, event) {
            log::warn!("向前端发送事件失败: {err}");
        }
    });

    // 诊断出口。开关判断放在这里而不是音频线程里：
    // 音频层不需要知道前端有没有开调试面板（或麦克风选择框）。
    let debug_handle = handle.clone();
    let debug: DebugFn = Arc::new(move |info: PetDebug| {
        let state = debug_handle.state::<AppState>();
        let debug_on = state.debug.load(Ordering::Relaxed);
        let meter_on = state.meter.load(Ordering::Relaxed);
        if !debug_on && !meter_on {
            return;
        }

        // 电平走自己那条通道：选择框只想知道「有没有声音」，
        // 不该顺带收到识别文本（那是调试面板的东西）。
        //
        // 发出去的**是整个 `PetDebug::Level`**，不是里面那个 `DebugLevel`：
        // 前者带上 `{"type":"level"}` 这个标签，前端才能用与调试通道同一个
        // `isDebugLevel` 去校验。只发内层结构的话它是个没有标签的裸对象，
        // 前端会一条不剩地丢掉 —— 电平条永远不动，而且没有任何报错。
        if let PetDebug::Level(level) = &info {
            if meter_on {
                if let Err(err) = debug_handle.emit(PET_LEVEL_CHANNEL, PetDebug::Level(*level)) {
                    log::debug!("向前端发送电平失败: {err}");
                }
            }
        }

        if !debug_on {
            return;
        }
        if let Err(err) = debug_handle.emit(PET_DEBUG_CHANNEL, info) {
            log::debug!("向前端发送诊断信息失败: {err}");
        }
    });

    // 状态出口。它跑在 `pet-audio` 线程上，所以只做两件轻活：
    // 改状态、发事件。**绝不能在这里去 stop 那条线程**（那就是让它 join 自己）。
    // 需要停线程 / 建窗口的事统统丢给另一条异步任务。
    let status_handle = handle.clone();
    let status_device = device.clone();
    let status: StatusFn = Arc::new(move |report| {
        let state = status_handle.state::<AppState>();
        if state.audio_generation.load(Ordering::SeqCst) != generation {
            // 上一代线程的迟到报告（用户已经换过设备了），丢掉
            log::debug!("忽略上一代音频线程的状态报告");
            return;
        }

        match report {
            WorkerStatus::Ready { device_name } => {
                log::info!("正在聆听: {device_name}");
                set_audio(
                    &status_handle,
                    AudioChoice {
                        device: Some(status_device.clone()),
                        status: AudioStatus::Listening,
                    },
                );
            }
            WorkerStatus::Failed(reason) => {
                log::error!("麦克风没能用上: {reason}");
                set_audio(
                    &status_handle,
                    AudioChoice {
                        device: Some(status_device.clone()),
                        status: AudioStatus::Failed {
                            reason: reason.clone(),
                        },
                    },
                );

                // 用不了就立刻让人重选 —— 启动时连不上、用到一半掉线，都走这里。
                // 收线程的事放异步任务里做（此刻正在这条线程上）。
                let handle = status_handle.clone();
                tauri::async_runtime::spawn(async move {
                    stop_worker(&handle);
                    spawn_mic_picker(&handle);
                });
            }
        }
    });

    let worker = AudioWorker::spawn(
        emit,
        debug,
        status,
        config,
        models_dir,
        Some(device.id.clone()),
    );

    // 换进槽位。取出来的旧线程要**出了锁再停**：`stop` 最长等 3 秒，
    // 攥着锁等会把退出流程（它要的是同一把锁）一起拖住。
    let state = handle.state::<AppState>();
    let previous = match state.worker.lock() {
        Ok(mut slot) => slot.replace(worker),
        Err(err) => {
            log::error!("音频状态锁已损坏，音频线程将不被管理: {err}");
            return;
        }
    }; // 锁在这里就还回去了

    if let Some(mut previous) = previous {
        // 正常路径不会走到：调用方都是先 `stop_worker` 再 `start_audio`。
        log::warn!("已存在音频线程，先停止旧的");
        previous.stop(AUDIO_SHUTDOWN_TIMEOUT);
    }
}

/// 停掉当前采集线程并让它作废。可以重复调用（没有线程时直接返回）。
///
/// 顺序上有个坑（原来 `start_audio` 里就有，只因当时只调用一次才没暴露）：
/// `stop` 会阻塞最长三秒，**不能**攥着 `worker` 锁等它 ——
/// 主线程在退出时正是要拿这把锁，会被拖住三秒；再有一个等待者就是死锁。
fn stop_worker(handle: &AppHandle) {
    let state = handle.state::<AppState>();
    // 先作废代数：还在路上的报告从此一律丢掉，免得它们盖住接下来的新状态
    state.audio_generation.fetch_add(1, Ordering::SeqCst);

    let worker = {
        let mut slot = match state.worker.lock() {
            Ok(slot) => slot,
            Err(err) => {
                log::error!("音频状态锁已损坏，无法停止音频线程: {err}");
                return;
            }
        };
        slot.take()
    }; // 锁在这里就还回去了，下面的等待不占着它

    if let Some(mut worker) = worker {
        if !worker.stop(AUDIO_SHUTDOWN_TIMEOUT) {
            // 超时不是设备的问题（多数是模型还在加载，那条线程会在加载完后自己退出），
            // 所以这里**不能**当成「这台设备坏了」往上报。
            log::warn!("音频线程未在超时内退出，它会在收到停止位后自行结束");
        }
    }
}

/// 用户选定了某只麦克风（或选了不监听）。写盘 → 换线程 → 通知前端。
///
/// 先写盘：用户点一下就该算数，之后哪怕连接失败、程序被强杀，他的选择也已经记住了。
pub fn choose_audio(handle: &AppHandle, choice: Option<DeviceChoice>) {
    window_state::save_audio(handle, choice.as_ref());

    // 先停旧的再开新的：同一台设备上两条采集流会打架（WASAPI 独占模式下第二条直接开不起来）
    stop_worker(handle);

    match choice {
        Some(device) => start_audio(handle, device),
        None => {
            // 不监听：这里没有工作线程了，也就再没有任何东西会发 idle ——
            // 角色会停在最后一个动作上（比如举着的开心表情），得自己补一条。
            emit_idle(handle);
            set_audio(handle, AudioChoice::off());
        }
    }
}

/// 把当前音频选择取出来给前端。
pub fn current_audio(handle: &AppHandle) -> AudioChoice {
    let state = handle.state::<AppState>();
    // guard 先绑成独立变量：直接写 `match state.audio.lock()` 时，
    // 那个临时的 `Result` 会活到语句结束，比 `state` 还长，借用检查过不去（E0597）。
    let locked = state.audio.lock();
    match locked {
        Ok(current) => current.clone(),
        Err(err) => {
            log::error!("音频选择状态锁已损坏: {err}");
            AudioChoice::off()
        }
    }
}

/// 更新音频选择并推给前端。**所有**改状态的地方都走它，
/// 免得出现「后端状态变了、前端还显示旧的」。
pub fn set_audio(handle: &AppHandle, choice: AudioChoice) {
    let state = handle.state::<AppState>();
    {
        let locked = state.audio.lock();
        match locked {
            Ok(mut current) => *current = choice.clone(),
            Err(err) => {
                log::error!("音频选择状态锁已损坏，状态未更新: {err}");
                return;
            }
        }
    }

    if let Err(err) = handle.emit(PET_AUDIO_CHANNEL, choice) {
        log::warn!("向前端发送音频状态失败: {err}");
    }
}

/// 让角色回到待机。没有采集线程时它也不会自己动了，得手动补。
fn emit_idle(handle: &AppHandle) {
    if let Err(err) = handle.emit(PET_EVENT_CHANNEL, PetEvent::new(PetEventKind::Idle)) {
        log::warn!("向前端发送待机事件失败: {err}");
    }
}

/// 弹出麦克风选择框。已经开着就把它拿到前面来，绝不建第二个。
///
/// 菜单、托盘、启动时自动弹、掉线后自动弹，全部走这一个函数 ——
/// 「什么时候该弹」的判据散在四处，但「怎么弹」只有一份实现。
pub fn spawn_mic_picker(handle: &AppHandle) {
    if let Some(window) = handle.get_webview_window(commands::PICKER_WINDOW) {
        // show / set_focus 走事件循环的非阻塞分支，从任何线程调都安全
        if let Err(err) = window.show() {
            log::warn!("显示麦克风选择框失败: {err}");
        }
        if let Err(err) = window.set_focus() {
            log::warn!("聚焦麦克风选择框失败: {err}");
        }
        return;
    }

    let state = handle.state::<AppState>();
    if state.picker_opening.swap(true, Ordering::SeqCst) {
        // 上一条建窗请求还没落地，这一次直接放过（见 `picker_opening` 的注释）
        return;
    }

    log::info!("打开麦克风选择框");
    let handle = handle.clone();
    // **建窗必须离开主线程**：主线程上建窗会走进 `wait_with_pump` 自锁，
    // 症状是白屏 + 所有 IPC 无响应（`commands.rs` 里记了完整的排查过程）。
    tauri::async_runtime::spawn(async move {
        if let Err(err) = commands::open_mic_picker_window(&handle) {
            log::error!("打开麦克风选择框失败: {err}");
        }
        handle.state::<AppState>().picker_opening.store(false, Ordering::SeqCst);
    });
}

/// 选择框关掉了。**若此刻手上没有能用的麦克风**（状态是「等你挑」或「试过不行」），
/// 关掉就算选「不监听」：不记的话每次启动都会再弹一次，太吵。
///
/// 反过来的情况必须什么都不做 —— 正听着某只麦克风、开框看一眼再关掉，
/// 不该把麦克风给关了。所以判据是状态，不是「窗口关掉了」这件事本身。
///
/// 代价要知道：设备被拔掉那一次「关掉不选」，下次启动不会再去把它找回来，
/// 用户得重选一次。这是刻意的取舍，不是漏了。
pub fn settle_picker_closed(handle: &AppHandle) {
    let handle = handle.clone();
    // 收线程最长要等几秒，不能占着窗口事件回调（主线程）干等
    tauri::async_runtime::spawn(async move {
        let current = current_audio(&handle);
        if !matches!(
            current.status,
            AudioStatus::NeedChoice | AudioStatus::Failed { .. }
        ) {
            return;
        }
        log::info!("关闭选择框时没有选设备，按「不监听」记下");
        choose_audio(&handle, None);
    });
}

/// 完整停止后台任务。可以重复调用：第二次取到 `None` 时直接返回。
///
/// 顺序很重要 —— 先通知前端停掉动画定时器，再等音频线程退出。
pub fn shutdown(handle: &AppHandle) {
    let state = handle.state::<AppState>();
    let Ok(mut slot) = state.worker.lock() else {
        log::error!("音频状态锁已损坏，跳过清理");
        return;
    };

    let Some(mut worker) = slot.take() else {
        return;
    };
    drop(slot); // 先释放锁，避免 stop 期间阻塞其他访问

    if handle.emit(PET_SHUTDOWN_CHANNEL, ()).is_ok() {
        log::debug!("已通知前端清理动画任务");
    }

    let clean = worker.stop(AUDIO_SHUTDOWN_TIMEOUT);
    if clean {
        log::info!("音频线程已正常结束，麦克风已释放");
    } else {
        log::warn!("音频线程未在超时内结束，继续退出");
    }
}
