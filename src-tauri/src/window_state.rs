//! 用户配置数据库：`%APPDATA%/<identifier>/settings.json`。
//!
//! 只放**用户能改的配置**，眼下是两节：`window`（窗口坐标与是否置顶）与
//! `audio`（用哪只麦克风听）。随包发布的默认值不放这里 ——
//! `config/audio.json`（Rust 侧的检测参数）与 `themes/<主题>/theme.json`（表现参数）
//! 都是只读、可入库的**配置**，而这里放的是**用户状态**，两者生命周期不同。
//! 放在用户目录而不是安装目录，是因为装到 Program Files 时安装目录根本不可写。
//!
//! 写文件的三条规矩：
//!
//!  * **只覆盖自己认识的那几个字段，其余一个不动。** 写之前先把文件原样读进来当文档，
//!    只改自己那一节再写回去。这样用户手写的键、用户自己加的节，以及以后版本新增的内容，
//!    都不会被一次写入抹掉（用户完全可能在程序运行期间改它）。两条写路径
//!    （[`save_window`] / [`save_audio`]）共用 [`update`]，规矩只有一份实现。
//!  * 窗口**不存尺寸**：窗口尺寸由 `theme.json` 决定，让用户状态去覆盖主题参数
//!    会变成两处真相。
//!  * 「没配过」与「配成不监听」必须分得开：判据是 `audio` **这一节在不在**，
//!    不是里面的字段值。见 [`SavedAudio`]。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, Runtime};

/// 写文件这一段的互斥锁。
///
/// 从有「麦克风选择」起，写这份文件的不止退出那一处了（用户点一下选择框就写一次），
/// 于是两个线程可能同时「读 → 改 → 写」。不加锁有两个后果，第二个更糟：
///
///  * **丢更新**：A 读到文档，B 写入窗口位置，A 再把手里那份旧文档写回去 —— 位置白存了；
///  * **误备份**：`std::fs::write` 是先截断再写，并发读到半截文件时它不是合法 JSON，
///    于是会被当成「用户手改坏了」而改名成 `.bad` 再重建 —— 用户手写的键就真没了。
///    这正是本模块存在的意义所在，不能让一次并发读取把它破掉。
///
/// 锁只护住这一个函数里的「读 → 改 → 写」，不护别的（写文件很慢，不该顺手把设备也锁上）。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 拿写锁。`Mutex` 中毒说明上一次写入 panic 了，但**文件本身是可以继续用的**
/// （panic 只会发生在写之前的内存操作里），没理由为此让应用再也存不了配置，
/// 所以取回内部值继续走。
fn lock_writes() -> std::sync::MutexGuard<'static, ()> {
    WRITE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

const FILE_NAME: &str = "settings.json";
/// 旧版本只记窗口状态的文件名。读到它就迁移，写完新文件后删掉它，保证「只留一份」。
const LEGACY_FILE_NAME: &str = "window.json";
/// 手改坏了（不是合法 JSON 对象）时，原文件先改名留一份，别让用户写的东西无声消失。
const BACKUP_SUFFIX: &str = ".bad";
/// 早先版本会在新建文件时写一句自动生成的说明，现在不要了（2026-10-16 用户要求去掉）。
/// 老文件里可能还留着它，所以写入时顺手删掉 —— 这个键是本程序自己写的，由本程序收回去是合适的。
///
/// 删之前**逐字比对**：用户完全可能自己写一个同名的 `_说明`（这文件本来就是给他改的），
/// 那不是我们的东西，一个字都不能动。
const LEGACY_NOTE_KEY: &str = "_说明";
const OLD_DOC_NOTE: &str = "这是本程序的用户配置数据库：只放用户能改的配置。眼下只有 window 一节（角色窗口的坐标与是否置顶），程序退出时会更新这三个字段，本文件里其余内容一律原样保留 —— 你可以自己加键，也可以把这段说明删掉。随包发布的默认值不在这个文件里：检测参数在 config/audio.json、表现参数在 themes/<主题>/theme.json。";

/// 存档里的 `window` 一节：左上角坐标（物理像素）+ 是否置顶。
///
/// 两个字段放在同一节里，因为它们同属「用户上次把窗口摆成什么样」，
/// 生命周期完全一致；分成两处只会多出一份可能只写了一半的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub x: i32,
    pub y: i32,
    /// 用 `default` 是为了让「只有坐标」的旧存档继续能读
    /// （这个字段是后加的，早期版本写出的文件里没有它）。
    #[serde(default)]
    pub always_on_top: bool,
}

/// 用户选中的输入设备。
///
/// `id` 是 cpal 的 `DeviceId` 字符串（形如 `wasapi:{0.0.1.00000000}.{guid}`），
/// **匹配用它，不用名字** —— 同型号麦克风插两只时名字完全一样（见 `audio::capture` 的注释）。
/// `name` 只用于显示：设备拔掉之后，得能对用户说清「你上次选的是哪一只」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceChoice {
    pub id: String,
    pub name: String,
}

/// 存档里的 `audio` 一节。
///
/// `input_device` 为 `None` 就是用户选了「不监听」——语气分析整套不启用。
/// 注意「没配过」与「不监听」必须是两回事，判据是**这一节在不在**：
///
///  * 整个 `audio` 节不存在（`Settings::audio` 为 `None`）→ 没配过 → 启动时弹选择框；
///  * `audio` 在、`input_device` 是 `null` → 明确选了不监听 → 不弹。
///
/// 写成 `{}`（节在、字段缺）按「不监听」读，与写 `null` 等价。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAudio {
    #[serde(default)]
    pub input_device: Option<DeviceChoice>,
}

/// 读取时用的整体形状。
///
/// 写回**不走这个结构体**（那会把用户手写的键丢掉），而是把原文件当 JSON 文档打补丁，
/// 见 [`update`]。所以这里不需要为未知字段做任何保留。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub window: Option<SavedWindow>,
    #[serde(default)]
    pub audio: Option<SavedAudio>,
}

fn file_path<R: Runtime>(app: &AppHandle<R>, name: &str) -> Option<PathBuf> {
    let dir = match app.path().app_config_dir() {
        Ok(dir) => dir,
        Err(err) => {
            log::warn!("拿不到用户配置目录，窗口位置将不会被记忆: {err}");
            return None;
        }
    };
    Some(dir.join(name))
}

/// 读一个文本文件。不存在（或读不动）都返回 `None`，让调用方走默认路径。
fn read_text(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            log::warn!("读取 {} 失败，忽略存档: {err}", path.display());
            None
        }
    }
}

/// 读取整份用户配置。文件不存在、内容损坏时返回空的 `Settings`（走默认）。
///
/// `settings.json` 存在就以它为准，不再回头看老存档 —— 规则只有一条，不用猜谁更新。
/// 调用方自己决定「`audio` 没配过」和「配成不监听」分别怎么办，这里只管如实读出来。
pub fn load<R: Runtime>(app: &AppHandle<R>) -> Settings {
    let Some(path) = file_path(app, FILE_NAME) else {
        return Settings::default();
    };

    if let Some(text) = read_text(&path) {
        return match serde_json::from_str::<Settings>(&text) {
            Ok(settings) => settings,
            Err(err) => {
                // 内容坏了不该让应用起不来，退回默认即可
                log::warn!("{} 解析失败，忽略存档: {err}", path.display());
                Settings::default()
            }
        };
    }

    // 还没有 settings.json：读一次老版本的 window.json，读到就迁移。
    // **迁移必须当场落盘**，不能等退出时顺手写：写这份文件的不止退出那一处，
    // 用户在麦克风选择框里点一下也会写 —— 那一写就让 settings.json 存在了，
    // 而本函数只在它**不存在**时才回头看老存档，这份窗口位置就永远找不回来了。
    let Some(legacy) = file_path(app, LEGACY_FILE_NAME) else {
        return Settings::default();
    };
    let Some(saved) = read_text(&legacy).and_then(|text| serde_json::from_str::<SavedWindow>(&text).ok())
    else {
        return Settings::default();
    };

    log::info!("从老存档 {} 迁移窗口状态", legacy.display());
    if update(app, |root| {
        write_window(root, saved.x, saved.y, saved.always_on_top)
    })
    .is_some()
    {
        // 写成了才删老档：没写成的话留着它，下次启动还能再迁一次。
        remove_legacy(app);
    }

    Settings {
        window: Some(saved),
        audio: None,
    }
}

/// 保存窗口状态。失败只记日志：记不住是小事，不该影响退出。
pub fn save_window<R: Runtime>(
    app: &AppHandle<R>,
    position: PhysicalPosition<i32>,
    always_on_top: bool,
) {
    if update(app, |root| write_window(root, position.x, position.y, always_on_top)).is_none() {
        return;
    }
    log::info!(
        "已记住窗口状态: ({}, {}), 置顶 {}",
        position.x,
        position.y,
        if always_on_top { "开" } else { "关" }
    );
    remove_legacy(app);
}

/// 保存用户选的输入设备。`None` = 不监听。
///
/// 与窗口状态不同，这一条**当场就写**：用户在选择框里点一下就该算数，
/// 万一程序之后崩了或被强杀，下次启动还得知道他选的是哪只。
pub fn save_audio<R: Runtime>(app: &AppHandle<R>, choice: Option<&DeviceChoice>) {
    if update(app, |root| write_audio(root, choice)).is_none() {
        return;
    }
    match choice {
        Some(choice) => log::info!("已记住麦克风: {}（{}）", choice.name, choice.id),
        None => log::info!("已记住「不监听」"),
    }
}

/// 读 → 改 → 写。两条写路径共用它，规矩只写一遍：
/// 坏文件先备份再重建、只动自己那节、其余内容原样保留。
///
/// 整个过程在 [`WRITE_LOCK`] 里，因为现在会有两个线程同时来写
/// （见那里的注释：不加锁会丢更新，更会误把好文件备份成 `.bad`）。
///
/// 成功返回文件路径（调用方据此记日志/删老档），失败返回 `None`。
fn update<R: Runtime>(
    app: &AppHandle<R>,
    edit: impl FnOnce(&mut Map<String, Value>),
) -> Option<PathBuf> {
    let _guard = lock_writes();
    let path = file_path(app, FILE_NAME)?;

    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            log::warn!("创建 {} 失败，配置未保存: {err}", dir.display());
            return None;
        }
    }

    let mut existing = read_text(&path);
    if let Some(text) = existing.as_deref() {
        if !is_json_object(text) {
            // 手改坏了。先把原文件留一份再覆盖：这份文件是给人改的，
            // 「打不开于是内容没了」比「打不开所以没记住」糟糕得多。
            if let Err(err) = backup(&path) {
                log::warn!("备份坏掉的 {} 失败（仍会写入新内容）: {err}", path.display());
            }
            existing = None;
        }
    }

    let doc = patched(existing.as_deref(), edit);
    let text = match serde_json::to_string_pretty(&doc) {
        Ok(text) => text,
        Err(err) => {
            log::warn!("序列化用户配置失败: {err}");
            return None;
        }
    };

    match std::fs::write(&path, text) {
        Ok(()) => Some(path),
        Err(err) => {
            log::warn!("写入 {} 失败: {err}", path.display());
            None
        }
    }
}

/// 新文档 = 已有文档 + `edit` 改掉的东西。纯函数，好在单测里验「用户加的键还在不在」。
///
/// `existing` 不是合法 JSON 对象时按「空文档」处理（调用方已经先备份过原文件）。
fn patched(existing: Option<&str>, edit: impl FnOnce(&mut Map<String, Value>)) -> Value {
    let usable = existing
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .filter(Value::is_object);

    let mut doc = usable.unwrap_or_else(|| Value::Object(Map::new()));
    let root = doc
        .as_object_mut()
        .expect("上面已经确保过根是一个 JSON 对象");

    // 老版本自动生成的那句说明不要了，顺手清掉（逐字比对，理由见 LEGACY_NOTE_KEY）。
    if root.get(LEGACY_NOTE_KEY).and_then(Value::as_str) == Some(OLD_DOC_NOTE) {
        root.remove(LEGACY_NOTE_KEY);
    }
    edit(root);

    doc
}

/// 覆盖 `window` 一节的三个键。
fn write_window(root: &mut Map<String, Value>, x: i32, y: i32, always_on_top: bool) {
    let window = section(root, "window");
    window.insert("x".to_owned(), Value::from(x));
    window.insert("y".to_owned(), Value::from(y));
    window.insert("always_on_top".to_owned(), Value::Bool(always_on_top));
}

/// 覆盖 `audio` 一节。`None` 写成 `null`（= 不监听），这一节本身在不在才是「配没配过」。
fn write_audio(root: &mut Map<String, Value>, choice: Option<&DeviceChoice>) {
    let audio = section(root, "audio");
    let value = match choice {
        // `DeviceChoice` 只有两个字符串字段，序列化不会失败；真失败了也宁可写 null，
        // 那至少是「不监听」这个合法状态，而不是半个写坏的对象。
        Some(choice) => serde_json::to_value(choice).unwrap_or(Value::Null),
        None => Value::Null,
    };
    audio.insert("input_device".to_owned(), value);
}

/// 取一节（不存在就建），并保证它是对象 —— 用户把它写成别的类型时换回对象，别 panic。
fn section<'a>(root: &'a mut Map<String, Value>, name: &str) -> &'a mut Map<String, Value> {
    let value = root.entry(name).or_insert_with(|| Value::Object(Map::new()));
    if !value.is_object() {
        log::warn!("settings.json 里的 {name} 不是对象，已替换成对象");
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().expect("上面刚换成了对象")
}

/// 是不是一个合法的 JSON **对象**。数组、字符串、坏 JSON 都算「读不出配置」。
fn is_json_object(text: &str) -> bool {
    matches!(serde_json::from_str::<Value>(text), Ok(Value::Object(_)))
}

/// 把文件改名成 `<名字>.bad`。同名备份先删掉，Windows 上 rename 不会覆盖已存在的文件。
fn backup(path: &Path) -> std::io::Result<()> {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let target = path.with_file_name(format!("{file_name}{BACKUP_SUFFIX}"));
    if target.exists() {
        std::fs::remove_file(&target)?;
    }
    std::fs::rename(path, &target)?;
    log::warn!("{} 不是合法 JSON，已备份为 {}", path.display(), target.display());
    Ok(())
}

/// 迁移完成后删掉老存档，保证用户目录里只有一份状态文件。
fn remove_legacy<R: Runtime>(app: &AppHandle<R>) {
    let Some(legacy) = file_path(app, LEGACY_FILE_NAME) else {
        return;
    };
    if !legacy.exists() {
        return;
    }
    match std::fs::remove_file(&legacy) {
        Ok(()) => log::info!("已删除老存档 {}（内容已并入 {FILE_NAME}）", legacy.display()),
        Err(err) => log::warn!("删除老存档 {} 失败: {err}", legacy.display()),
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

        let choice = DeviceChoice {
            id: "wasapi:{0.0.1.00000000}.{guid}".to_owned(),
            name: "耳机 (花再 Auro Clip)".to_owned(),
        };
        let text = serde_json::to_string(&choice).unwrap();
        assert_eq!(serde_json::from_str::<DeviceChoice>(&text).unwrap(), choice);
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

    /// 整份文档的形状：`window` 一节里放那三个键。
    #[test]
    fn window_section_is_read_from_the_document() {
        let doc = r#"{"window":{"x":-1200,"y":340,"always_on_top":true}}"#;
        assert_eq!(
            serde_json::from_str::<Settings>(doc).unwrap().window,
            Some(SavedWindow {
                x: -1200,
                y: 340,
                always_on_top: true
            })
        );
    }

    /// 没有 `window` 一节（用户自己删了、或文件是别的工具写的）→ 当作「没有存档」，退回居中。
    #[test]
    fn document_without_window_section_reads_as_no_archive() {
        let doc = r#"{"别的设置":1}"#;
        assert_eq!(serde_json::from_str::<Settings>(doc).unwrap().window, None);
    }

    /// **这条是麦克风那部分的核心**：`audio` 一节在、`input_device` 是 null = 用户选了不监听。
    #[test]
    fn null_input_device_means_off() {
        let settings = serde_json::from_str::<Settings>(r#"{"audio":{"input_device":null}}"#).unwrap();
        let audio = settings.audio.expect("audio 这一节在");
        assert_eq!(audio.input_device, None);
    }

    /// **和上一条必须分得开**：整个 `audio` 节不存在 = 没配过 → 启动时要弹选择框。
    /// 两者的区别全在「节在不在」，所以这条测的是 `Settings::audio`，不是里面的字段。
    #[test]
    fn absent_audio_section_means_unconfigured() {
        let settings = serde_json::from_str::<Settings>(r#"{"window":{"x":1,"y":2}}"#).unwrap();
        assert!(settings.audio.is_none());
    }

    /// 选中的设备能原样读回来（id 与 name 都要）。
    #[test]
    fn audio_section_roundtrips() {
        let doc = r#"{"audio":{"input_device":{"id":"wasapi:{a}","name":"USB 麦"}}}"#;
        let audio = serde_json::from_str::<Settings>(doc).unwrap().audio.unwrap();
        let choice = audio.input_device.expect("选了设备");
        assert_eq!(choice.id, "wasapi:{a}");
        assert_eq!(choice.name, "USB 麦");
    }

    /// **这条是「配置数据库」的核心**：写回只动自己那两节，
    /// 用户手写的键（顶层、以及我们自己那节里的）一个都不能丢。
    #[test]
    fn patch_keeps_keys_it_does_not_own() {
        let before = r#"{
          "我的说明": "我自己写的说明",
          "window": { "x": 5, "y": 6, "always_on_top": false, "我的备注": "别删" },
          "audio": { "input_device": { "id": "wasapi:{a}", "name": "旧的" }, "我的": 1 },
          "我自己加的节": { "随便": [1, 2, 3] }
        }"#;
        let choice = DeviceChoice {
            id: "wasapi:{b}".to_owned(),
            name: "新的".to_owned(),
        };

        let after = patched(Some(before), |root| {
            write_window(root, 100, 200, true);
            write_audio(root, Some(&choice));
        });

        assert_eq!(after["window"]["x"], 100);
        assert_eq!(after["window"]["y"], 200);
        assert_eq!(after["window"]["always_on_top"], true);
        assert_eq!(after["audio"]["input_device"]["id"], "wasapi:{b}");
        // 用户的东西原样在
        assert_eq!(after["我的说明"], "我自己写的说明");
        assert_eq!(after["window"]["我的备注"], "别删");
        assert_eq!(after["audio"]["我的"], 1);
        assert_eq!(after["我自己加的节"]["随便"][2], 3);
    }

    /// 第一次写（文件还不存在）：两节分别建出来，**不写任何说明键** —— 那句说明已经取消了。
    #[test]
    fn patch_creates_the_document_from_nothing() {
        let after = patched(None, |root| write_window(root, 1, 2, false));
        assert_eq!(after["window"]["x"], 1);
        assert_eq!(after["window"]["y"], 2);
        assert_eq!(after["window"]["always_on_top"], false);
        assert!(after.get(LEGACY_NOTE_KEY).is_none());

        // 不监听：这一节在、值是 null（与「没配过」的区别就在这里）
        let after = patched(Some(r#"{"window":{"x":1,"y":2}}"#), |root| {
            write_audio(root, None)
        });
        assert!(after["audio"].is_object());
        assert_eq!(after["audio"]["input_device"], Value::Null);
    }

    /// 我们以前自动写进去的那句说明，用户要求去掉 → 写入时逐个字比对后删掉。
    #[test]
    fn patch_removes_the_note_we_used_to_write() {
        let before = format!(r#"{{"_说明":"{OLD_DOC_NOTE}","window":{{"x":9,"y":9}}}}"#);
        let after = patched(Some(&before), |root| write_window(root, 0, 0, false));
        assert!(after.get(LEGACY_NOTE_KEY).is_none());
        assert_eq!(after["window"]["x"], 0);
    }

    /// 但用户自己写的 `_说明` 不是我们的东西，一个字都不许动
    /// —— 这份文件是给他改的，同名的键要么是他的，要么是我们那句（已删）。
    #[test]
    fn patch_keeps_a_hand_written_note() {
        let after = patched(Some(r#"{"_说明":"我自己写的","window":{"x":9}}"#), |root| {
            write_window(root, 0, 0, false)
        });
        assert_eq!(after[LEGACY_NOTE_KEY], "我自己写的");
    }

    /// `window` 被写成了别的类型（手滑写成数字、数组、字符串）→ 换成对象继续写，不 panic。
    #[test]
    fn patch_replaces_a_window_of_the_wrong_type() {
        let after = patched(Some(r#"{"window":42,"其他":true}"#), |root| {
            write_window(root, 7, 8, true)
        });
        assert_eq!(after["window"]["x"], 7);
        assert_eq!(after["其他"], true);
    }

    /// 只有合法的 JSON **对象**才算能打补丁；数组、字符串、坏 JSON 都要走「备份后再写」那条路。
    #[test]
    fn only_json_objects_are_patchable() {
        assert!(is_json_object(r#"{"a":1}"#));
        assert!(is_json_object("{}"));
        assert!(!is_json_object("{ not json"));
        assert!(!is_json_object("[1,2]"));
        assert!(!is_json_object(r#""只是个字符串""#));
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
