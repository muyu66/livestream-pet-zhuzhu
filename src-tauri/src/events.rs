//! 标准化角色事件（DEV.md §5.4）。
//!
//! 这是 Rust 侧与前端之间的唯一契约：后端只把「已经过稳定处理」的粗粒度事件推给
//! 前端，前端永远不接触原始音频，也不接触模型输出。
//!
//! 刻意没有 `confidence` 字段：SenseVoice 不返回可用置信度，DEV.md §5.6 明确禁止
//! 凭空生成。稳定性由「连续窗口一致性 + 最短持续时间 + 冷却」保证，见 `audio::emotion::Stabilizer`。

use crate::window_state::DeviceChoice;
use serde::Serialize;

/// 推送给前端的事件通道名。前端 `src/types/events.ts` 中的常量必须与此一致。
pub const PET_EVENT_CHANNEL: &str = "pet://event";

/// 退出前通知前端清理动画定时器的通道名。
pub const PET_SHUTDOWN_CHANNEL: &str = "pet://shutdown";

/// 置顶状态变化的通道名，负载是一个 bool。
///
/// 托盘菜单切了置顶之后，前端自绘菜单的文案也得跟着变，
/// 否则会出现「菜单写着置顶、但窗口已经是置顶」的不一致。
pub const PET_ALWAYS_ON_TOP_CHANNEL: &str = "pet://always-on-top";

/// 诊断信息通道的通道名。**不属于上面那份契约**。
///
/// `pet://event` 上跑的只有「已稳定」的粗粒度事件，前端据此驱动动画；
/// 那条链路刻意丢掉了原文、语言、原始情绪 token、耗时这些中间信息。
/// 排查「为什么没识别出来」时缺的正是这些，所以单开一条通道，
/// 只在调试面板打开时推送，免得正常运行也白白走 IPC。
pub const PET_DEBUG_CHANNEL: &str = "pet://debug";

/// 「用的是哪只麦克风、现在到底在不在听」的通道名。
///
/// 与 `pet://debug` 分开是有意的：那条通道是排查用的可选数据流（调试面板开着才有），
/// 而这一条是**状态** —— 选择框靠它决定显示「正在聆听」还是「连接失败」，
/// 角色窗口靠它决定要不要提示。它不该因为调试面板没开就收不到。
pub const PET_AUDIO_CHANNEL: &str = "pet://audio";

/// 选择框要看的实时电平。
///
/// **为什么另开一条通道，而不是复用 `pet://debug`**：两条流各自的闸门不同
/// （调试面板 vs 选择框），复用一条会让「关掉其中一个窗口」这件事互相干扰
/// —— `App.vue` 里那条关于热重载的注释记的正是这个坑。负载类型倒是可以共用
/// [`DebugLevel`]，它描述的本来就是采集流的状态，与谁在看无关。
pub const PET_LEVEL_CHANNEL: &str = "pet://level";

/// 音频选择的当前状态。选择框与调试面板都读它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioStatus {
    /// 不监听：语气分析**整套不启用**（不是把阈值调大，是根本不建工作线程）。
    Off,
    /// 正在连接那只麦克风：加载模型 + 打开流。要一两秒。
    Connecting,
    /// 正在聆听。
    Listening,
    /// 手上没有能用的麦克风，等用户在选择框里挑一只（或挑「不监听」）。
    NeedChoice,
    /// 试过了，没成。`reason` 是给用户看的说明。
    Failed { reason: String },
}

/// 「用哪只麦克风 + 现在什么状态」，一起送出去。
///
/// 做成一个整体是因为两者必须同时到达：单说「状态是 Listening」而不知道是哪只，
/// 选择框就没法把高亮标在正确的行上。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AudioChoice {
    /// 用户选定的设备；`None` = 不监听。
    pub device: Option<DeviceChoice>,
    pub status: AudioStatus,
}

impl AudioChoice {
    /// 从没配过（配置文件里没有 `audio` 这一节）。
    pub fn need_choice() -> Self {
        Self {
            device: None,
            status: AudioStatus::NeedChoice,
        }
    }

    /// 用户明确选了「不监听」。
    pub fn off() -> Self {
        Self {
            device: None,
            status: AudioStatus::Off,
        }
    }
}

/// 一次识别结果（原文 + 模型给出的原始 token）。
///
/// `emotion`/`event` 是**模型原始标签**（如 `<|HAPPY|>`、`<|Laughter|>`），
/// 不是映射后的 `PetEventKind` —— 调试时要看的恰恰是「模型说了什么」，
/// 而不是稳定化之后剩下什么。
#[derive(Debug, Clone, Serialize)]
pub struct DebugTranscript {
    /// true = 语音段结束后的权威解码；false = 说话过程中的中间解码
    pub segment: bool,
    pub text: String,
    pub lang: String,
    pub emotion: String,
    pub event: String,
    /// 送去识别的音频长度（秒）
    pub audio_secs: f32,
    /// 本次推理耗时（毫秒）
    pub elapsed_ms: u64,
}

/// 采集电平。判断「麦克风到底有没有声音」只能靠它 ——
/// 全是数字静音时识别不出东西是必然的，跟模型、跟映射都无关。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DebugLevel {
    /// 本报告周期内的采样峰值（0.0–1.0）
    pub peak: f32,
    /// 本报告周期内的均方根
    pub rms: f32,
    /// 累计丢弃的音频块
    pub dropped: u64,
    /// 累计驱动错误通知次数
    pub capture_errors: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PetDebug {
    Level(DebugLevel),
    Transcript(DebugTranscript),
}

// `Hash` 是为了让稳定化那一层能用 `HashMap<PetEventKind, Instant>` 记录各事件的
// 上次上报时刻（去抖窗口），见 `audio::emotion::Stabilizer`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PetEventKind {
    /// 检测到主播开始说话（由 VAD 判定）
    VoiceStart,
    /// 一段语音结束（由 VAD 判定）
    VoiceEnd,
    /// 一段时间未检测到人声
    Idle,
    /// 情绪偏开心
    Happy,
    /// 情绪偏生气
    Angry,
    /// 情绪偏难过
    Sad,
    /// 情绪偏惊讶
    Surprised,
    /// 检测到笑声
    Laugh,
    /// 没有明显目标情绪，继续普通说话状态
    Neutral,
}

impl PetEventKind {
    /// 语音起止直接来自 VAD，不参与去抖 —— 否则一次「说话→停顿→说话」里的
    /// 停顿会被吞掉，前端就一直停在 speaking。
    pub fn is_voice_transition(self) -> bool {
        matches!(self, PetEventKind::VoiceStart | PetEventKind::VoiceEnd)
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct PetEvent {
    pub kind: PetEventKind,
    /// 事件产生时刻（Unix 毫秒）。**只用于诊断** —— 前端没有冷却计算，
    /// 动画的节奏由表情序列里的停顿决定（见 DEV.md §4.4），这里只用来显示两条事件隔了多久。
    pub ts: i64,
}

impl PetEvent {
    pub fn new(kind: PetEventKind) -> Self {
        Self {
            kind,
            ts: now_millis(),
        }
    }
}

pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
