//! 资源路径解析。
//!
//! DEV.md §2.2 要求模型不嵌进主 EXE、单独部署，所以程序必须能在两种布局下找到资源：
//!
//!  开发期（cargo run / tauri dev）:  <项目根>/models
//!  发布期（安装后）:                <EXE 同级>/models
//!
//! 另外允许用环境变量覆盖，便于测试和阶段 4 的部署验证。

use std::path::{Path, PathBuf};

const MODELS_DIR_ENV: &str = "LIVESTREAM_PET_MODELS_DIR";
const CONFIG_DIR_ENV: &str = "LIVESTREAM_PET_CONFIG_DIR";

/// 模型目录。找不到时返回开发期默认位置，让上层给出明确的「缺文件」错误，
/// 而不是在这里 panic —— 缺模型时应当能弹出可读的提示。
pub fn models_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(MODELS_DIR_ENV) {
        return PathBuf::from(dir);
    }

    if let Some(dir) = exe_sibling("models") {
        if dir.is_dir() {
            return dir;
        }
    }

    // 开发期：CARGO_MANIFEST_DIR 是 src-tauri/，模型放在项目根的 models/
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("models")
}

/// 运行期配置文件目录（audio.json）
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV) {
        return PathBuf::from(dir);
    }

    if let Some(dir) = exe_sibling("config") {
        if dir.is_dir() {
            return dir;
        }
    }

    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config")
}

/// EXE 所在目录下的子路径（发布布局）
fn exe_sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join(name))
}

/// 把配置里的相对路径解析成绝对路径；已经是绝对路径就原样返回。
pub fn resolve(base: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    }
}
