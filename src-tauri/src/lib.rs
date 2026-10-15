//! 直播宠物 · 奇魔猪
//!
//! 拆成 lib + bin 两个 target 而不是只留一个 bin，是为了让阶段 0 的命令行原型
//! （`examples/audio_probe.rs`）能直接复用正式的音频流水线：
//! 如果音频代码只存在于 bin target 里，example 是引用不到它的，
//! 那样阶段 0 就会退化成「另写一份原型」——正好是 DEV.md §7 想避免的重复实现。

pub mod audio;
pub mod config;
pub mod events;
pub mod paths;

mod app;
mod commands;
mod menu;
mod tray;
mod window_state;

/// 启动桌面应用（托盘 + 透明窗口 + 音频线程）
pub fn run() {
    app::run();
}
