//! 窗口状态记忆：退出时记住，下次启动还原。
//!
//! 存到**系统用户配置目录**（`%APPDATA%/<identifier>/window.json`），
//! 而不是随包的 `config/`。区别在于生命周期：
//! `config/audio.json` 是随程序发布的默认配置（只读、可入库），
//! 而窗口位置与置顶是**用户状态**，安装目录在 Program Files 下时根本不可写。
//!
//! 只存位置与置顶，不存尺寸：窗口尺寸由 `theme.json` 决定，
//! 让用户状态去覆盖主题参数会变成两处真相。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, Runtime};

const FILE_NAME: &str = "window.json";

/// 存档的窗口状态：左上角坐标（物理像素）+ 是否置顶。
///
/// 两个字段放在同一个文件里，因为它们同属「用户上次把窗口摆成什么样」，
/// 生命周期完全一致；分成两个文件只会多出一份可能只写了一半的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub x: i32,
    pub y: i32,
    /// 用 `default` 是为了让「只有坐标」的旧存档继续能读
    /// （这个字段是后加的，早期版本写出的文件里没有它）。
    #[serde(default)]
    pub always_on_top: bool,
}

fn file_path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    let dir = match app.path().app_config_dir() {
        Ok(dir) => dir,
        Err(err) => {
            log::warn!("拿不到用户配置目录，窗口位置将不会被记忆: {err}");
            return None;
        }
    };
    Some(dir.join(FILE_NAME))
}

/// 读取上次的窗口状态。文件不存在或内容损坏时返回 `None`（走默认）。
pub fn load<R: Runtime>(app: &AppHandle<R>) -> Option<SavedWindow> {
    let path = file_path(app)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            log::warn!("读取 {} 失败，忽略存档: {err}", path.display());
            return None;
        }
    };

    match serde_json::from_str::<SavedWindow>(&text) {
        Ok(saved) => Some(saved),
        Err(err) => {
            // 内容坏了不该让应用起不来，退回居中即可
            log::warn!("{} 解析失败，忽略存档: {err}", path.display());
            None
        }
    }
}

/// 保存窗口状态。失败只记日志：记不住是小事，不该影响退出。
pub fn save<R: Runtime>(
    app: &AppHandle<R>,
    position: PhysicalPosition<i32>,
    always_on_top: bool,
) {
    let Some(path) = file_path(app) else {
        return;
    };

    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            log::warn!("创建 {} 失败，窗口状态未保存: {err}", dir.display());
            return;
        }
    }

    let saved = SavedWindow {
        x: position.x,
        y: position.y,
        always_on_top,
    };

    match serde_json::to_string_pretty(&saved) {
        Ok(text) => match std::fs::write(&path, text) {
            Ok(()) => log::info!(
                "已记住窗口状态: ({}, {}), 置顶 {}",
                position.x,
                position.y,
                if always_on_top { "开" } else { "关" }
            ),
            Err(err) => log::warn!("写入 {} 失败: {err}", path.display()),
        },
        Err(err) => log::warn!("序列化窗口状态失败: {err}"),
    }
}

/// 一块显示器工作区（已排除任务栏）。
///
/// 单独定义是为了让几何计算能脱离 Tauri 单测 —— 这段逻辑出错最致命
/// （窗口落到屏幕外就再也看不见了），恰恰最需要测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkArea {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl WorkArea {
    fn right(&self) -> i32 {
        self.x + self.width
    }

    fn bottom(&self) -> i32 {
        self.y + self.height
    }
}

/// 把窗口左上角夹进某块工作区，保证**整个窗口**都留在屏幕内。
///
/// 返回 `None` 表示没有任何显示器放得下它（显示器被拔掉、分辨率变小等），
/// 调用方应退回配置里的居中显示。
///
/// 为什么是「夹住」而不是「丢弃」：早先是只要左上角落在工作区内就照搬存档，
/// 结果窗口可以大半挂在屏幕外。桌宠被拖到贴边再退出，下次回来就只剩一角 ——
/// 用户找不着，也不知道该往哪儿找。夹一下最多把贴边的宠物挪回几像素，
/// 比让它半消失划算得多。
fn clamp_into(areas: &[WorkArea], w: i32, h: i32, x: i32, y: i32) -> Option<(i32, i32)> {
    // 优先选已经能完整容纳窗口的那块，避免多显示器下把窗口挪到隔壁屏幕
    if areas
        .iter()
        .any(|a| x >= a.x && y >= a.y && x + w <= a.right() && y + h <= a.bottom())
    {
        return Some((x, y));
    }

    // 否则挑一块装得下窗口的，夹进去（取离原坐标最近的一种夹法）
    areas
        .iter()
        .filter(|a| a.width >= w && a.height >= h)
        .map(|a| (x.clamp(a.x, a.right() - w), y.clamp(a.y, a.bottom() - h)))
        .min_by_key(|(cx, cy)| (i64::from(cx - x)).pow(2) + (i64::from(cy - y)).pow(2))
}

/// 算出一个可用的还原位置：夹进当前存在的某个显示器工作区。
///
/// 用的是工作区而不是整块屏幕，所以不会把窗口还原到任务栏底下。
pub fn resolve_restore_position<R: Runtime>(
    app: &AppHandle<R>,
    saved: SavedWindow,
    size: PhysicalSize<u32>,
) -> Option<PhysicalPosition<i32>> {
    let monitors = match app.available_monitors() {
        Ok(monitors) => monitors,
        Err(err) => {
            log::warn!("枚举显示器失败，放弃还原位置: {err}");
            return None;
        }
    };

    let areas: Vec<WorkArea> = monitors
        .iter()
        .map(|m| {
            let area = m.work_area();
            WorkArea {
                x: area.position.x,
                y: area.position.y,
                width: area.size.width as i32,
                height: area.size.height as i32,
            }
        })
        .collect();

    if areas.is_empty() {
        log::warn!("没有检测到任何显示器，放弃还原位置");
        return None;
    }

    match clamp_into(&areas, size.width as i32, size.height as i32, saved.x, saved.y) {
        Some((x, y)) => {
            if (x, y) != (saved.x, saved.y) {
                log::info!(
                    "存档位置 ({}, {}) 会被屏幕边缘裁掉，已夹到 ({x}, {y})",
                    saved.x,
                    saved.y
                );
            }
            Some(PhysicalPosition::new(x, y))
        }
        None => {
            log::warn!("没有显示器容得下窗口，放弃还原位置");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_json() {
        let saved = SavedWindow {
            x: -1200,
            y: 340,
            always_on_top: true,
        };
        let text = serde_json::to_string(&saved).unwrap();
        assert_eq!(serde_json::from_str::<SavedWindow>(&text).unwrap(), saved);
    }

    #[test]
    fn broken_json_is_rejected_rather_than_panicking() {
        // 存档损坏时必须能干净地失败（调用方会退回默认），不能 panic
        assert!(serde_json::from_str::<SavedWindow>("{ not json").is_err());
        // 多出来的字段不影响读取（前向兼容：以后加字段不会让老版本读不了新存档）
        let with_extra = r#"{"x":1,"y":2,"future":true}"#;
        assert_eq!(
            serde_json::from_str::<SavedWindow>(with_extra).unwrap(),
            SavedWindow {
                x: 1,
                y: 2,
                always_on_top: false
            }
        );
    }

    fn areas() -> Vec<WorkArea> {
        // 1920x1080，底部留出任务栏 —— 与实际机器一致
        vec![WorkArea {
            x: 0,
            y: 0,
            width: 1920,
            height: 1032,
        }]
    }

    const W: i32 = 320;
    const H: i32 = 320;

    #[test]
    fn accepted_position_is_left_alone() {
        assert_eq!(clamp_into(&areas(), W, H, 800, 400), Some((800, 400)));
    }

    /// 这条是实际踩到的：窗口右边缘超出屏幕 17px，
    /// 用户下次回来只看到被切掉一角的宠物。
    #[test]
    fn position_hanging_off_the_right_edge_is_pulled_back() {
        assert_eq!(clamp_into(&areas(), W, H, 1617, 708), Some((1600, 708)));
    }

    #[test]
    fn position_overlapping_the_taskbar_is_lifted() {
        assert_eq!(clamp_into(&areas(), W, H, 100, 1000), Some((100, 712)));
    }

    #[test]
    fn position_on_a_removed_monitor_is_brought_back() {
        // 老坐标落在已拔掉的第二块屏上；现存这块装得下，就夹回来
        assert_eq!(clamp_into(&areas(), W, H, 3000, 200), Some((1600, 200)));
    }

    #[test]
    fn window_larger_than_any_monitor_is_rejected() {
        assert_eq!(clamp_into(&areas(), 2000, 1200, 0, 0), None);
    }

    #[test]
    fn multi_monitor_prefers_the_screen_already_containing_the_window() {
        let two = vec![
            WorkArea {
                x: 0,
                y: 0,
                width: 1920,
                height: 1032,
            },
            // 第二块屏在主屏左侧（负坐标是 Windows 上的常见排布）
            WorkArea {
                x: -1280,
                y: 0,
                width: 1280,
                height: 984,
            },
        ];
        // 窗口本来就在左屏内，不该被挪到主屏
        assert_eq!(clamp_into(&two, W, H, -900, 100), Some((-900, 100)));
    }

    /// 夹取必须考虑窗口尺寸，不能只夹左上角那个点：
    /// 宽度快占满整块屏时，x 的可选范围本来就很窄。
    #[test]
    fn clamping_accounts_for_window_size() {
        assert_eq!(clamp_into(&areas(), 1900, H, 100, 100), Some((20, 100)));
    }

    /// 置顶字段是后加的：早期版本写出的存档只有坐标。
    /// 这种文件必须还能读（当成「不置顶」），否则老用户升级后位置记忆会整个失效。
    #[test]
    fn legacy_archive_without_topmost_still_loads() {
        let legacy = r#"{"x":1540,"y":712}"#;
        assert_eq!(
            serde_json::from_str::<SavedWindow>(legacy).unwrap(),
            SavedWindow {
                x: 1540,
                y: 712,
                always_on_top: false
            }
        );
    }
}
