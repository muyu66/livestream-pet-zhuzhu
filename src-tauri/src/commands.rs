//! 前端可调用的命令。

use crate::app::{self, AppState};
use crate::audio::capture::{self, DeviceInfo};
use crate::events::AudioChoice;
use crate::menu::{self, MenuEntry};
use crate::window_state::DeviceChoice;
use std::sync::atomic::Ordering;
use tauri::{
    AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

/// 调试窗口的 label。前端、能力文件（capabilities/debug.json）都按它对齐。
pub const DEBUG_WINDOW: &str = "debug";

/// 麦克风选择框的 label。前端、能力文件（capabilities/mic-picker.json）都按它对齐。
pub const PICKER_WINDOW: &str = "mic-picker";

const PICKER_WINDOW_TITLE: &str = "选择麦克风";

/// 选择框的尺寸。比调试窗口小得多：里面就是几行设备加一条电平。
///
/// 高度留得比内容多：设备列表最长时（插了一堆虚拟声卡）会滚动，
/// 而滚动条在 420 的高度里只露两三行，很难看出下面还有东西。
const PICKER_WINDOW_SIZE: (f64, f64) = (360.0, 460.0);

const DEBUG_WINDOW_TITLE: &str = "直播宠物 · 调试";

/// 调试窗口的默认尺寸 / 最小尺寸。
///
/// 比角色窗口（160×160）大得多是有意的：它要同时放下动作按钮、标准化事件按钮、
/// 电平表和识别日志。之前它是角色窗口里的浮层，只能挤在 72% 的高度里 ——
/// 既遮住角色又看不全，这正是改造它的原因。
const DEBUG_WINDOW_SIZE: (f64, f64) = (460.0, 660.0);
const DEBUG_WINDOW_MIN_SIZE: (f64, f64) = (360.0, 320.0);

/// 退出应用。
///
/// 角色右键菜单走这里，托盘菜单直接调用同一套清理逻辑（`app::shutdown`），
/// 保证两条退出路径的行为完全一致：停麦克风 → 结束推理线程 → 关闭应用。
#[tauri::command]
pub fn exit_app(app: AppHandle) {
    log::info!("角色菜单：退出");
    crate::app::shutdown(&app);
    app.exit(0);
}

/// 切换置顶，返回切换后的状态。
///
/// 具体逻辑在 `app::toggle_always_on_top`，因为托盘菜单也要走同一条路径。
#[tauri::command]
pub fn toggle_always_on_top(app: AppHandle) -> Result<bool, String> {
    crate::app::toggle_always_on_top(&app)
}

/// 取当前菜单项。
///
/// 前端自绘菜单不自己硬编码列表，而是每次拿到这份由 Rust 生成的菜单，
/// 这样「两份菜单共享同一份定义」（DEV.md §3.3）是靠结构保证的，
/// 而不是靠两边手动保持一致。
#[tauri::command]
pub fn menu_items(app: AppHandle) -> Vec<MenuEntry> {
    let always_on_top = app
        .state::<AppState>()
        .always_on_top
        .load(Ordering::Relaxed);
    menu::entries(always_on_top)
}

/// 开关调试信息推送。
///
/// 前端打开调试面板时调用。返回切换后的状态，方便前端在日志里核对
/// —— 面板打开了但没数据、且这里返回 false，就说明是开关没送到，
/// 而不是音频管线没输出。
#[tauri::command]
pub fn set_debug(app: AppHandle, enabled: bool) -> bool {
    app.state::<AppState>()
        .debug
        .store(enabled, Ordering::Relaxed);
    log::info!("调试信息推送已{}", if enabled { "开启" } else { "关闭" });
    enabled
}

/// 开关调试窗口。返回调用之后的窗口状态（true = 现在开着）。
///
/// 调试界面做成独立窗口而不是角色窗口里的浮层：角色窗口就是角色大小
/// （160×160），浮层放不下完整内容，还会正好盖在角色身上。
/// 独立窗口可以随便拉大，角色窗口也不必为它让位。
///
/// **这里必须是 `async fn`，不能是同步的。**（踩过的坑，别改回去）
/// 建窗口这条链最终走到 tauri-runtime-wry 的 `Context::send_user_message`：
/// 在主线程上它**内联**执行 handler，而 handler 里 `create_window` 会用
/// webview2-com 的 `wait_with_pump`（一个阻塞的 GetMessageA 循环）等 WebView2
/// 的回调 —— 我们此刻正处在 IPC 消息的处理过程中，等于在自己等自己，
/// 主线程就此死锁。
/// 症状极具迷惑性：webview 已经建出来但停在 `about:blank`（一片纯白），
/// 紧接着**所有** IPC 都不再响应 —— 按 D 关不掉窗口、右键菜单退出也没反应，
/// 看起来像「程序卡住了」，实际是主线程再也没回到消息循环。
/// 写成 `async` 之后命令跑在 tokio 线程上，`send_user_message` 走 `proxy.send_event`
/// 把建窗口请求投递给事件循环，由事件循环在顶层处理，这条链才走得通。
///
/// 实测确认方式：在 `build()` 前后各打一条日志，日志停在 build() 之前；
/// 再用 WebView2 的 `--remote-debugging-port` 连进去看，页面 URL 是 about:blank。
#[tauri::command]
pub async fn toggle_debug_window(app: AppHandle) -> bool {
    if let Some(window) = app.get_webview_window(DEBUG_WINDOW) {
        if let Err(err) = window.close() {
            log::warn!("关闭调试窗口失败: {err}");
            // 关不掉就还是「开着」，如实返回，别让前端以为已经关了
            return true;
        }
        log::info!("调试窗口已关闭");
        return false;
    }

    match open_debug_window(&app) {
        Ok(_) => {
            log::info!("调试窗口已打开");
            true
        }
        Err(err) => {
            log::error!("打开调试窗口失败: {err}");
            false
        }
    }
}

/// 列出可选的输入设备（选择框的列表用）。
///
/// `async` 不是为了建窗，而是为了**别在主线程上枚举音频设备** ——
/// 枚举要走 COM/WASAPI，几十毫秒起步，卡的是整个界面的消息循环。
#[tauri::command]
pub async fn list_input_devices() -> Result<Vec<DeviceInfo>, String> {
    capture::list_input_devices().map_err(|err| format!("{err:#}"))
}

/// 当前用的是哪只麦克风、现在什么状态。选择框打开时先要一份。
#[tauri::command]
pub fn audio_state(app: AppHandle) -> AudioChoice {
    app::current_audio(&app)
}

/// 选定某只麦克风（`None` = 不监听）。点一行就调它，返回调用之后的状态。
///
/// `async`：里面要停掉旧采集线程，最长等 3 秒，不能占着主线程。
#[tauri::command]
pub async fn choose_audio_device(app: AppHandle, device: Option<DeviceChoice>) -> AudioChoice {
    match &device {
        Some(device) => log::info!("选择麦克风: {}（{}）", device.name, device.id),
        None => log::info!("选择麦克风: 不监听"),
    }
    app::choose_audio(&app, device);
    app::current_audio(&app)
}

/// 开关实时电平推送。选择框挂载时开、卸载时关。
///
/// 与 `set_debug` 是同一个形状，但**刻意是另一个开关**：
/// 两个窗口各推各的数据，谁也关不掉对方的（见 `AppState::meter` 的注释）。
#[tauri::command]
pub fn set_meter(app: AppHandle, enabled: bool) -> bool {
    app.state::<AppState>()
        .meter
        .store(enabled, Ordering::Relaxed);
    log::debug!("实时电平推送已{}", if enabled { "开启" } else { "关闭" });
    enabled
}

/// 打开选择框。幂等：已经开着就把它拿到前面来。
///
/// **`async fn` 的原因与 `toggle_debug_window` 完全一样**（见那里的长注释）：
/// 建窗必须离开主线程，否则会自锁成白屏。
#[tauri::command]
pub async fn open_mic_picker(app: AppHandle) {
    app::spawn_mic_picker(&app);
}

/// 关闭选择框。**关窗的语义在 Rust 这边**（见 `app::settle_picker_closed`）：
/// 手上没有可用麦克风时，把它关掉就等于选了「不监听」。
///
/// 所以前端必须走这条命令，不能自己 `getCurrentWindow().close()` —— 那样会绕开记录。
#[tauri::command]
pub async fn close_mic_picker(app: AppHandle) {
    let Some(window) = app.get_webview_window(PICKER_WINDOW) else {
        return;
    };
    if let Err(err) = window.close() {
        log::warn!("关闭麦克风选择框失败: {err}");
    }
}

/// 建出选择框窗口。**只给 `app::spawn_mic_picker` 用**，不要从命令里直接调。
pub fn open_mic_picker_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let window = WebviewWindowBuilder::new(
        app,
        PICKER_WINDOW,
        WebviewUrl::App("mic-picker.html".into()),
    )
    .title(PICKER_WINDOW_TITLE)
    .inner_size(PICKER_WINDOW_SIZE.0, PICKER_WINDOW_SIZE.1)
    // 不可缩放：内容是一列定长的行，能拉大只会拉出一片空白。
    // 与调试窗口（那个要放日志，必须能拉）刚好相反。
    .resizable(false)
    // 置顶：这个框会弹在全屏的游戏/OBS 上面，不置顶就等于没弹 ——
    // 用户只会发现「角色不反应了」，却看不到让他选麦克风的那个框。
    .always_on_top(true)
    .center()
    .build()?;

    // 窗口关掉了就把电平推送关掉，理由与调试窗口那条一样：
    // 前端只有在挂载/卸载时才调 set_meter，而 Alt+F4、应用退出这些路径它未必赶得上。
    // 顺带处理「关掉没选」的语义（见 `settle_picker_closed`）。
    let handle = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            handle.state::<AppState>().meter.store(false, Ordering::Relaxed);
            app::settle_picker_closed(&handle);
        }
    });

    Ok(window)
}

fn open_debug_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let window = WebviewWindowBuilder::new(app, DEBUG_WINDOW, WebviewUrl::App("debug.html".into()))
        .title(DEBUG_WINDOW_TITLE)
        .inner_size(DEBUG_WINDOW_SIZE.0, DEBUG_WINDOW_SIZE.1)
        .min_inner_size(DEBUG_WINDOW_MIN_SIZE.0, DEBUG_WINDOW_MIN_SIZE.1)
        .resizable(true)
        .center()
        .build()?;

    // 诊断推送的开关必须跟着窗口的生死走：前端只有在挂载/卸载时才调 set_debug，
    // 而窗口被 Alt+F4 关掉、或应用整体退出时前端未必来得及调到（甚至根本没跑），
    // 那样音频线程会一直往一条没人听的通道上推数据。
    let handle = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            handle
                .state::<AppState>()
                .debug
                .store(false, Ordering::Relaxed);
            log::info!("调试窗口已销毁，诊断推送随之关闭");
        }
    });

    Ok(window)
}
