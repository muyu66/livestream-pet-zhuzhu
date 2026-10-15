//! 系统托盘（DEV.md §3.2）。
//!
//! 菜单项不在这个文件里写死，而是来自 `crate::menu` —— 那是托盘菜单
//! 与角色右键菜单共用的唯一一份定义（DEV.md §3.3）。
//!
//! 菜单必须在 Rust 里构建而不是写进 tauri.conf.json：Tauri 2 的 `TrayIconConfig`
//! 只有 iconPath / tooltip 之类的字段，没有声明菜单的能力。

use crate::app::AppState;
use crate::menu;
use std::sync::atomic::Ordering;
use tauri::menu::{IsMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

const TRAY_ID: &str = "pet-tray";

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let always_on_top = current_always_on_top(app);
    let menu = build_menu(app, always_on_top)?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("直播宠物 · 奇魔猪")
        .menu(&menu)
        // 左键不弹菜单，只有右键弹（文档要求「右键托盘图标时显示菜单」）
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            menu::MENU_TOGGLE_TOPMOST => {
                if let Err(err) = crate::app::toggle_always_on_top(app) {
                    log::error!("切换置顶失败: {err}");
                }
            }
            menu::MENU_QUIT => {
                log::info!("托盘菜单：退出");
                crate::app::shutdown(app);
                app.exit(0);
            }
            other => log::warn!("收到未知的托盘菜单项: {other}"),
        });

    // 用应用图标作为托盘图标，避免额外维护一份资源
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    } else {
        log::warn!("没有默认窗口图标，托盘图标将为空");
    }

    builder.build(app)?;
    log::info!("系统托盘已创建");
    Ok(())
}

/// 置顶状态变了之后重建托盘菜单。
///
/// 直接整份重建、再 `set_menu` 换掉，而不是保存 `MenuItem` 句柄去 `set_text`：
/// 后者要把带运行时泛型的句柄塞进全局状态，换来的只是少建两个菜单项，
/// 不划算。菜单只有两项，重建的开销可以忽略。
pub fn refresh_menu(app: &AppHandle) -> tauri::Result<()> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        // 托盘可能没建起来（build 失败时只记了日志），这不是错误
        return Ok(());
    };

    let menu = build_menu(app, current_always_on_top(app))?;
    tray.set_menu(Some(menu))
}

fn current_always_on_top(app: &AppHandle) -> bool {
    app.state::<AppState>().always_on_top.load(Ordering::Relaxed)
}

fn build_menu(app: &AppHandle, always_on_top: bool) -> tauri::Result<Menu<Wry>> {
    let entries = menu::entries(always_on_top);

    let mut items: Vec<MenuItem<Wry>> = Vec::with_capacity(entries.len());
    for entry in &entries {
        items.push(MenuItem::with_id(
            app,
            &entry.id,
            &entry.label,
            true,
            None::<&str>,
        )?);
    }

    let refs: Vec<&dyn IsMenuItem<Wry>> = items
        .iter()
        .map(|item| item as &dyn IsMenuItem<Wry>)
        .collect();

    Menu::with_items(app, &refs)
}
