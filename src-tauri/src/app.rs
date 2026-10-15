//! Tauri 应用装配：托盘、状态、窗口位置、音频线程生命周期、退出清理。

use crate::audio::{AudioWorker, DebugFn, EmitFn};
use crate::commands;
use crate::config::AudioConfig;
use crate::events::{
    PetDebug, PetEvent, PET_ALWAYS_ON_TOP_CHANNEL, PET_DEBUG_CHANNEL, PET_EVENT_CHANNEL,
    PET_SHUTDOWN_CHANNEL,
};
use crate::paths;
use crate::tray;
use crate::window_state;
use std::sync::atomic::{AtomicBool, Ordering};
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
            always_on_top: AtomicBool::new(false),
            debug: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            commands::exit_app,
            commands::toggle_always_on_top,
            commands::menu_items,
            commands::set_debug,
            commands::toggle_debug_window
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // 先定位再显示：配置里窗口是 visible:false 的
            restore_window(&handle);

            // 托盘是主要的常驻入口；失败不致命，但必须留下明确日志
            if let Err(err) = tray::build(&handle) {
                log::error!("创建系统托盘失败: {err}");
            }

            start_audio(&handle);

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
fn restore_window(handle: &AppHandle) {
    let Some(window) = handle.get_webview_window(MAIN_WINDOW) else {
        log::error!("找不到主窗口 {MAIN_WINDOW}，无法恢复窗口状态");
        return;
    };

    let saved = window_state::load(handle);

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
            window_state::save(handle, pos, always_on_top)
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


/// 启动本地语音分析线程。
///
/// 注意这里**不会**因为麦克风或模型有问题而失败：
/// 采集与模型加载都在工作线程内部进行，出错时只记录日志，
/// 保证「没有麦克风也能启动角色」——桌宠的动画不应该被音频问题拖死。
fn start_audio(handle: &AppHandle) {
    let config = AudioConfig::load();
    let models_dir = paths::models_dir();
    log::info!("模型目录: {}", models_dir.display());

    let emit_handle = handle.clone();
    let emit: EmitFn = Arc::new(move |event: PetEvent| {
        if let Err(err) = emit_handle.emit(PET_EVENT_CHANNEL, event) {
            log::warn!("向前端发送事件失败: {err}");
        }
    });

    // 诊断出口。开关判断放在这里而不是音频线程里：
    // 音频层不需要知道前端有没有开调试面板。
    let debug_handle = handle.clone();
    let debug: DebugFn = Arc::new(move |info: PetDebug| {
        if !debug_handle.state::<AppState>().debug.load(Ordering::Relaxed) {
            return;
        }
        if let Err(err) = debug_handle.emit(PET_DEBUG_CHANNEL, info) {
            log::debug!("向前端发送诊断信息失败: {err}");
        }
    });

    let worker = AudioWorker::spawn(emit, debug, config, models_dir);

    let state = handle.state::<AppState>();
    // 先把 guard 绑成独立变量再赋值：若直接写 `match state.worker.lock() { .. }`，
    // 这个临时 `Result` 会活到整个 block 结束 —— 也就是 `state` 析构之后，
    // 借用检查会直接报 E0597。
    let mut slot = match state.worker.lock() {
        Ok(slot) => slot,
        Err(err) => {
            log::error!("音频状态锁已损坏，音频线程将不被管理: {err}");
            return;
        }
    };

    // 正常路径只会走到这里一次；万一被调用两次，把旧 worker 换出来显式停掉，
    // 而不是让它留在后台继续吃麦克风。
    if let Some(mut previous) = slot.replace(worker) {
        log::warn!("已存在音频线程，先停止旧的");
        previous.stop(AUDIO_SHUTDOWN_TIMEOUT);
    }
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
