//! 本地语音情绪分析（DEV.md §5）。
//!
//! 数据流：
//!
//! ```text
//! 麦克风采集(cpal) → 重采样 → VAD(Silero) → SenseVoice 情绪/事件 → 稳定处理 → 标准化事件
//!      capture.rs        worker.rs   vad.rs         asr.rs          emotion.rs    events.rs
//! ```
//!
//! 这一层完全不认识 Tauri，也不认识前端：它只产出一串 `PetEvent`，
//! 通过 `EmitFn` 交给上层。因此阶段 0 的命令行原型（`examples/audio_probe.rs`）
//! 能和正式应用共用同一套流水线。

pub mod capture;
pub mod emotion;
pub mod worker;

mod asr;
mod vad;

pub use worker::{AudioWorker, DebugFn, EmitFn, StatusFn, WorkerStatus};
