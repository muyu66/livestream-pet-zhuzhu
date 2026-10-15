//! 前端可调用的命令。

use crate::app::AppState;
use crate::menu::{self, MenuEntry};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Manager};

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
