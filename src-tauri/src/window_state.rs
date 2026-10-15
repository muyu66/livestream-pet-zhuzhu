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

/// 决定这个存档位置还原到哪儿：**能不动就不动，只有真的跟所有显示器都对不上了才夹回来**。
///
/// 1. 窗口与某块工作区有交集 → 照搬存档。
/// 2. 跟所有工作区都没有交集 → 存档来自一块已经被拔掉的显示器（或分辨率变小了），
///    这时才夹进最近一块装得下的工作区。
///
/// 返回 `None` 表示没有任何一块工作区装得下窗口，调用方退回居中显示。
///
/// **为什么允许窗口挂到屏幕外**（这条规则是被实际使用推翻后改的，别再改回去）：
/// 用户会把角色摆到屏幕边角、甚至大半压在任务栏上 —— 那是他自己放的位置，不该被程序搬走。
/// 原来要求「整个窗口都得在工作区内」，实际用下来有两个后果：角色永远离屏幕边缘差一圈余量；
/// 更糟的是夹过的位置退出时会被当成新位置存回去，**每启动一次就被往里推一次**，
/// 用户再怎么拖都留不住。窗口与角色框合并成等大之后（DEV.md §3.1）这条仍要留着：
/// 「窗口正好贴着工作区边界」必须是合法的，否则夹一下就会把角色从角上推开。
///
/// 放宽之后也不会把角色弄丢：拖拽过程中抓取点始终在屏幕内（光标的坐标被系统限制在
/// 桌面上），所以一次拖拽结束时窗口必然还压着屏幕，第 一 条分支总能命中。
fn resolve_placement(areas: &[WorkArea], w: i32, h: i32, x: i32, y: i32) -> Option<(i32, i32)> {
    let overlaps =
        |a: &WorkArea| x < a.right() && y < a.bottom() && x + w > a.x && y + h > a.y;

    if areas.iter().any(overlaps) {
        return Some((x, y));
    }

    // 挑一块装得下窗口的，夹进去（取离原坐标最近的一种夹法）
    areas
        .iter()
        .filter(|a| a.width >= w && a.height >= h)
        .map(|a| (x.clamp(a.x, a.right() - w), y.clamp(a.y, a.bottom() - h)))
        .min_by_key(|(cx, cy)| (i64::from(cx - x)).pow(2) + (i64::from(cy - y)).pow(2))
}

/// 算出一个可用的还原位置。
///
/// 坐标来自工作区而不是整块屏幕，所以贴底时角色是坐在任务栏上沿的。
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

    match resolve_placement(&areas, size.width as i32, size.height as i32, saved.x, saved.y) {
        Some((x, y)) => {
            if (x, y) != (saved.x, saved.y) {
                log::info!(
                    "存档位置 ({}, {}) 与当前所有显示器都没有交集（显示器被拔掉或分辨率变了？），已夹到 ({x}, {y})",
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

    // 窗口与角色框等大（见 DEV.md §3.1），这两个数都取当前实际的 160
    const W: i32 = 160;
    const H: i32 = 160;

    #[test]
    fn accepted_position_is_left_alone() {
        assert_eq!(resolve_placement(&areas(), W, H, 800, 400), Some((800, 400)));
    }

    /// 贴边挂出去是**故意的**，必须原样保留。
    ///
    /// 这几条原来断言的是「往里夹」—— 实际用下来，那个行为让角色永远离边缘差一圈余量，
    /// 而且夹过的位置退出时又被存回去，每启动一次往里推一次。
    #[test]
    fn position_hanging_off_the_edge_is_kept() {
        // 窗口正好卡在工作区右下角：角色与窗口等大，所以角色也正好贴住角
        assert_eq!(resolve_placement(&areas(), W, H, 1760, 872), Some((1760, 872)));
        // 大半压在任务栏上：也是用户自己拖过去的，不搬
        assert_eq!(resolve_placement(&areas(), W, H, 100, 1000), Some((100, 1000)));
        // 只剩 1px 搭在工作区上：拖拽结束时抓取点必然还在屏幕内，就是这种边界
        assert_eq!(resolve_placement(&areas(), W, H, 1919, 400), Some((1919, 400)));
    }

    #[test]
    fn position_on_a_removed_monitor_is_brought_back() {
        // 老坐标落在已拔掉的第二块屏上；现存这块装得下，就夹回来右边缘贴齐
        assert_eq!(resolve_placement(&areas(), W, H, 3000, 200), Some((1760, 200)));
    }

    /// 与所有工作区都没有交集（连一个像素都没搭上）才值得救：
    /// 到了这一步只可能是显示器没了或分辨率变了，不是用户拖出来的。
    #[test]
    fn position_completely_off_screen_is_brought_back() {
        // 左边完全在屏幕外：x + w == 0（正好擦着左边界，一个像素都没搭上）
        assert_eq!(resolve_placement(&areas(), W, H, -160, 200), Some((0, 200)));
        assert_eq!(resolve_placement(&areas(), W, H, 1920, 200), Some((1760, 200)));
    }

    /// 窗口比屏幕还大：只要还压在工作区上就照搬 —— 这时交给调用方居中只会更糟。
    #[test]
    fn window_larger_than_any_monitor_still_overlapping_is_kept() {
        assert_eq!(resolve_placement(&areas(), 2000, 1200, 0, 0), Some((0, 0)));
    }

    /// 装不下、又完全在屏幕外 → 放弃还原，让调用方居中
    #[test]
    fn window_that_no_monitor_can_hold_is_rejected() {
        assert_eq!(resolve_placement(&areas(), 2000, 1200, 5000, 0), None);
    }

    #[test]
    fn position_on_the_second_monitor_is_kept() {
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
        assert_eq!(resolve_placement(&two, W, H, -900, 100), Some((-900, 100)));
    }

    /// 夹取必须考虑窗口尺寸，不能只夹左上角那个点：
    /// 宽度快占满整块屏时，x 的可选范围本来就很窄。
    #[test]
    fn clamping_accounts_for_window_size() {
        assert_eq!(
            resolve_placement(&areas(), 1900, H, 5000, 100),
            Some((20, 100))
        );
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
