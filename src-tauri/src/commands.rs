//! 前端可调用的命令。

use crate::app::AppState;
use crate::menu::{self, MenuEntry};
use std::sync::atomic::Ordering;
use tauri::{
    AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

/// 调试窗口的 label。前端、能力文件（capabilities/debug.json）都按它对齐。
pub const DEBUG_WINDOW: &str = "debug";

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
