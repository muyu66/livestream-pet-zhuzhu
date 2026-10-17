//! 情绪 / 声音事件的解析与稳定处理（DEV.md §5.6）。
//!
//! 分两层：
//!  * `parse_*`：把模型输出的 `<|HAPPY|>` 这类 token 变成枚举，纯函数、可单测。
//!  * `Stabilizer`：实现静音超时、一致性窗口、去抖、持续时间、冷却。
//!
//! 为什么不用「情绪阈值」：SenseVoice 不返回可用置信度，而 DEV.md §5.6 明确禁止
//! 凭空生成 confidence 字段。所以这里改用**连续窗口一致性 + 最短持续时间**替代
//! 阈值筛选 —— 这是文档允许的替代方案。

use crate::config::StabilityConfig;
use crate::events::PetEventKind;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 第一版关注的粗粒度情绪。`Unknown` 表示模型给了不处理的标签或没给标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emotion {
    Happy,
    Angry,
    Sad,
    Surprised,
    Neutral,
    Unknown,
}

impl Emotion {
    fn to_event(self) -> Option<PetEventKind> {
        match self {
            Emotion::Happy => Some(PetEventKind::Happy),
            Emotion::Angry => Some(PetEventKind::Angry),
            Emotion::Sad => Some(PetEventKind::Sad),
            Emotion::Surprised => Some(PetEventKind::Surprised),
            Emotion::Neutral => Some(PetEventKind::Neutral),
            Emotion::Unknown => None,
        }
    }
}

/// 只关心笑声；其它声音事件第一版不驱动动画，仅用于日志诊断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundEvent {
    Laughter,
    Other,
    None,
}

/// 去掉 `<|` `|>` 外壳并大写，便于匹配。
fn normalize_tag(tag: &str) -> String {
    tag.trim()
        .trim_start_matches("<|")
        .trim_end_matches("|>")
        .trim()
        .to_ascii_uppercase()
}

/// `<|HAPPY|>` → `Emotion::Happy`
///
/// 刻意不处理 FEARFUL / DISGUSTED：DEV.md §4.3 的动作表里没有对应动画，
/// 硬映射成「惊讶」之类属于凭空发明语义。第一版把它们当 Unknown 忽略并打日志，
/// 等实际麦克风测试阶段拿到数据后再决定是否需要扩展动作集。
pub fn parse_emotion(tag: &str) -> Emotion {
    match normalize_tag(tag).as_str() {
        "HAPPY" => Emotion::Happy,
        "ANGRY" => Emotion::Angry,
        "SAD" => Emotion::Sad,
        "SURPRISED" => Emotion::Surprised,
        "NEUTRAL" => Emotion::Neutral,
        // 模型明确表示「这段判不出情绪」（静音、噪声、太短、语气音）。
        // 这是正常结果，不是「出现了没映射的新标签」，所以不刷 debug 日志。
        // `[语音段]` 日志仍会原样打印这个标签，方便排查。
        "EMO_UNKNOWN" => Emotion::Unknown,
        "" => Emotion::Unknown,
        other => {
            log::debug!("未映射的情绪标签: <|{other}|>");
            Emotion::Unknown
        }
    }
}

/// `<|Laughter|>` → `SoundEvent::Laughter`
pub fn parse_event(tag: &str) -> SoundEvent {
    match normalize_tag(tag).as_str() {
        "LAUGHTER" => SoundEvent::Laughter,
        "" => SoundEvent::None,
        _ => SoundEvent::Other,
    }
}

/// 把不稳定的模型输出变成稳定的动画事件。
pub struct Stabilizer {
    cfg: StabilityConfig,

    /// 最近若干次情绪观察，用于一致性判断
    recent: VecDeque<Emotion>,
    /// 最近一次已确认（且已上报）的情绪
    reported_emotion: Option<Emotion>,
    /// 上次上报情绪的时刻，用于 `emotion_hold_ms`
    last_emotion_at: Option<Instant>,

    /// 各类事件的上次上报时刻，用于去抖
    last_emitted: HashMap<PetEventKind, Instant>,

    /// 最近一次检测到人声的时刻，用于静音超时
    last_voice_at: Option<Instant>,
    voice_active: bool,
    idle_reported: bool,
}

impl Stabilizer {
    /// `now` 作为「上次听到人声」的初始值。
    ///
    /// 不能初始化成 `None`：那样冷启动后若一直没人说话，`poll_idle` 会因为
    /// `last_voice_at?` 直接返回 `None`，静音超时**永远不触发**，
    /// 「一段时间没人声就进待机」这条规则等于没实现。
    pub fn new(cfg: StabilityConfig, now: Instant) -> Self {
        let window = cfg.emotion_consistency_windows.max(1) as usize;
        Self {
            cfg,
            recent: VecDeque::with_capacity(window),
            reported_emotion: None,
            last_emotion_at: None,
            last_emitted: HashMap::new(),
            last_voice_at: Some(now),
            voice_active: false,
            idle_reported: false,
        }
    }

    /// VAD 报告「当前有人声」。每帧都会调用，用于推后静音超时。
    pub fn note_voice(&mut self, now: Instant) {
        self.last_voice_at = Some(now);
        self.idle_reported = false;
    }

    /// 开始说话。返回需要下发的事件。
    pub fn on_voice_start(&mut self, now: Instant) -> Vec<PetEventKind> {
        self.note_voice(now);
        if self.voice_active {
            return Vec::new();
        }
        self.voice_active = true;
        // 这里**不能**清 `recent`：一致性窗口的语义是「连续 N 个语音段情绪一致」
        // （DEV.md §5.6），清掉就成了「同一段音频自己给自己投票」——
        // 中间解码 + 语音段解码本来就会解出同一个结果，等于窗口永远自动满足，
        // 筛选作用归零。窗口只在静音超时（`poll_idle`）时清空。
        //
        // 但已上报情绪要重置：前端动画是一次性的，同一情绪每句话重新上报一次
        // 才会重新触发动画，否则主播一直开心，宠物只弹一下就不动了。
        self.reported_emotion = None;
        vec![PetEventKind::VoiceStart]
    }

    /// 一段语音结束。按 DEV.md §5.5，这里**不**直接进 idle，
    /// 而是等静音超时（见 `poll_idle`）。同样不清 `recent`，理由见 `on_voice_start`。
    pub fn on_voice_end(&mut self, now: Instant) -> Vec<PetEventKind> {
        self.note_voice(now);
        if !self.voice_active {
            return Vec::new();
        }
        self.voice_active = false;
        vec![PetEventKind::VoiceEnd]
    }

    /// 处理一次解码结果（中间解码或整段解码都走这里）。
    pub fn on_transcript(
        &mut self,
        emotion_tag: &str,
        event_tag: &str,
        now: Instant,
    ) -> Vec<PetEventKind> {
        self.note_voice(now);

        let mut out = Vec::new();

        // 先处理情绪，再处理笑声：笑声优先级更高，
        // 前端调度器会在同一次 dispatch 里让笑声顶掉刚进入的情绪动作。
        if let Some(kind) = self.observe_emotion(parse_emotion(emotion_tag), now) {
            out.push(kind);
        }

        if parse_event(event_tag) == SoundEvent::Laughter
            && self.should_emit(PetEventKind::Laugh, now)
        {
            out.push(PetEventKind::Laugh);
        }

        out
    }

    /// 轮询静音超时。返回 `Some(Idle)` 表示本次应当进入待机。
    pub fn poll_idle(&mut self, now: Instant) -> Option<PetEventKind> {
        if self.idle_reported {
            return None;
        }
        let last = self.last_voice_at?;
        if now.duration_since(last) < Duration::from_millis(self.cfg.idle_timeout_ms) {
            return None;
        }

        self.idle_reported = true;
        self.voice_active = false;
        // 这里同样不清 `recent`（理由见 `on_voice_start`），但已上报情绪要重置，
        // 好让待机之后的同一情绪能重新触发动画。
        //
        // 不去清窗口是有意的：窗口的作用是「两次解码说的是同一个情绪」，
        // 判的是**一致性**而不是时间。清掉它只会让「隔几秒说一句」的正常节奏
        // 永远攒不满窗口、一个情绪也报不出来；而留着的旧投票最多只能帮当前这句
        // 更快确认它自己已经说出的情绪，编不出新情绪来。
        self.reported_emotion = None;
        // idle 本身也过一遍去抖，避免超时边界反复触发
        if self.should_emit(PetEventKind::Idle, now) {
            Some(PetEventKind::Idle)
        } else {
            None
        }
    }

    // ------------------------------------------------------------- internals

    /// 把一次情绪观察喂进一致性窗口；只有确认且需要变化时才返回事件。
    fn observe_emotion(&mut self, emotion: Emotion, now: Instant) -> Option<PetEventKind> {
        let window = self.cfg.emotion_consistency_windows.max(1) as usize;

        // `Unknown` 不占窗口位置。它表示「模型这次什么也没给出」（静音、语气音、
        // 说了但没听清），不是「情绪和上次不一致」—— 让它占位会把窗口推向
        // 永远不可能全等，等于用一次无效解码否掉两次一致的判断。
        if emotion == Emotion::Unknown {
            return None;
        }

        self.recent.push_back(emotion);
        while self.recent.len() > window {
            self.recent.pop_front();
        }

        if self.recent.len() < window {
            return None;
        }

        // 窗口内必须全部一致 —— 这就是「阈值」的替代品
        // （Unknown 在入口就被挡掉了，所以这里只比一致性）
        let confirmed = *self.recent.back()?;
        if self.recent.iter().any(|e| *e != confirmed) {
            return None;
        }

        // 情绪没变就不重复上报
        if self.reported_emotion == Some(confirmed) {
            return None;
        }

        // 持续时间：情绪刚落地的一段时间内不再切换，避免来回抖
        if let Some(last) = self.last_emotion_at {
            if now.duration_since(last) < Duration::from_millis(self.cfg.emotion_hold_ms) {
                return None;
            }
        }

        let kind = confirmed.to_event()?;
        if !self.should_emit(kind, now) {
            return None;
        }

        self.reported_emotion = Some(confirmed);
        self.last_emotion_at = Some(now);
        Some(kind)
    }

    /// 通用去抖：同一类事件在窗口内只放行一次。
    fn should_emit(&mut self, kind: PetEventKind, now: Instant) -> bool {
        let window = Duration::from_millis(self.cfg.event_debounce_ms);
        if let Some(last) = self.last_emitted.get(&kind) {
            if now.duration_since(*last) < window {
                return false;
            }
        }
        self.last_emitted.insert(kind, now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> StabilityConfig {
        StabilityConfig {
            idle_timeout_ms: 1000,
            emotion_consistency_windows: 2,
            event_debounce_ms: 0,
            emotion_hold_ms: 0,
        }
    }

    #[test]
    fn parses_known_emotions() {
        assert_eq!(parse_emotion("<|HAPPY|>"), Emotion::Happy);
        assert_eq!(parse_emotion("<|SAD|>"), Emotion::Sad);
        assert_eq!(parse_emotion("<|ANGRY|>"), Emotion::Angry);
        assert_eq!(parse_emotion("<|SURPRISED|>"), Emotion::Surprised);
        assert_eq!(parse_emotion("<|NEUTRAL|>"), Emotion::Neutral);
    }

    #[test]
    fn unmapped_and_empty_emotions_are_unknown() {
        assert_eq!(parse_emotion(""), Emotion::Unknown);
        // 第一版不处理这两类，必须是 Unknown 而不是被硬映射
        assert_eq!(parse_emotion("<|FEARFUL|>"), Emotion::Unknown);
        assert_eq!(parse_emotion("<|DISGUSTED|>"), Emotion::Unknown);
        // 模型明确说「判不出情绪」，同样归为 Unknown（且不算未映射标签）
        assert_eq!(parse_emotion("<|EMO_UNKNOWN|>"), Emotion::Unknown);
    }

    #[test]
    fn parses_laughter_event() {
        assert_eq!(parse_event("<|Laughter|>"), SoundEvent::Laughter);
        assert_eq!(parse_event("<|Speech|>"), SoundEvent::Other);
        assert_eq!(parse_event(""), SoundEvent::None);
    }

    #[test]
    fn single_observation_does_not_emit_emotion() {
        // 一致性窗口为 2：只解出一次相同情绪时必须按兵不动，
        // 否则每次模型抖动都会直接切动画。
        let mut s = Stabilizer::new(cfg(), Instant::now());
        let t = Instant::now();
        assert!(s.on_transcript("<|HAPPY|>", "", t).is_empty());
    }

    #[test]
    fn consistent_windows_emit_emotion_once() {
        let mut s = Stabilizer::new(cfg(), Instant::now());
        let t = Instant::now();
        assert!(s.on_transcript("<|HAPPY|>", "", t).is_empty());
        assert_eq!(
            s.on_transcript("<|HAPPY|>", "", t),
            vec![PetEventKind::Happy]
        );
        // 情绪没变化时不应重复上报
        assert!(s.on_transcript("<|HAPPY|>", "", t).is_empty());
    }

    #[test]
    fn inconsistent_windows_do_not_emit() {
        let mut s = Stabilizer::new(cfg(), Instant::now());
        let t = Instant::now();
        s.on_transcript("<|HAPPY|>", "", t);
        // 窗口是 [HAPPY, ANGRY]，不一致 → 不确认
        assert!(s.on_transcript("<|ANGRY|>", "", t).is_empty());
    }

    #[test]
    fn unknown_observations_do_not_fill_the_window() {
        // 模型没给标签（静音段、语气音）不是「情绪不一致」，
        // 不能拿它顶掉一次有效观察，否则两次一致的判断会被一次空结果否掉。
        let mut s = Stabilizer::new(cfg(), Instant::now());
        let t = Instant::now();
        assert!(s.on_transcript("<|HAPPY|>", "", t).is_empty());
        assert!(s.on_transcript("<|EMO_UNKNOWN|>", "", t).is_empty());
        // 空结果没有占位，所以这次就该确认
        assert_eq!(
            s.on_transcript("<|HAPPY|>", "", t),
            vec![PetEventKind::Happy]
        );
    }

    #[test]
    fn consistency_window_spans_utterances() {
        // DEV.md §5.6 的「连续 N 个语音段」。若在 on_voice_start 清窗口，
        // 同一次说话里的中间解码 + 语音段解码就会自己满足窗口，
        // 筛选等于没做；这里锁住「跨语音段累计」这个语义。
        let mut s = Stabilizer::new(cfg(), Instant::now());
        let t = Instant::now();

        s.on_voice_start(t);
        assert!(s.on_transcript("<|HAPPY|>", "", t).is_empty(), "第一个语音段不该确认");
        s.on_voice_end(t);

        s.on_voice_start(t);
        assert_eq!(
            s.on_transcript("<|HAPPY|>", "", t),
            vec![PetEventKind::Happy],
            "第二个语音段情绪一致，应当确认"
        );
        s.on_voice_end(t);

        // 换了情绪则必须重新攒够窗口
        s.on_voice_start(t);
        assert!(s.on_transcript("<|ANGRY|>", "", t).is_empty());
        s.on_voice_end(t);

        s.on_voice_start(t);
        assert_eq!(
            s.on_transcript("<|ANGRY|>", "", t),
            vec![PetEventKind::Angry]
        );
    }

    #[test]
    fn window_survives_idle_so_sparse_speech_still_confirms() {
        // 大部分人是隔几秒说一句。如果待机就把窗口清空，每句之间都要重新攒，
        // 一个情绪也报不出来。窗口判的是「两次解码一致」，与时间无关，
        // 所以待机不该清它；待机后同一情绪仍应能确认并重新触发动画。
        let t = Instant::now();
        let mut s = Stabilizer::new(cfg(), t);
        s.on_voice_start(t);
        assert!(s.on_transcript("<|HAPPY|>", "", t).is_empty());
        s.on_voice_end(t);

        assert_eq!(
            s.poll_idle(t + Duration::from_millis(1500)),
            Some(PetEventKind::Idle)
        );

        let later = t + Duration::from_millis(8000);
        s.on_voice_start(later);
        assert_eq!(
            s.on_transcript("<|HAPPY|>", "", later),
            vec![PetEventKind::Happy],
            "待机前的那次有效观察仍然算数"
        );
    }

    #[test]
    fn laughter_is_reported() {
        let mut s = Stabilizer::new(cfg(), Instant::now());
        let t = Instant::now();
        let events = s.on_transcript("<|NEUTRAL|>", "<|Laughter|>", t);
        assert!(events.contains(&PetEventKind::Laugh));
    }

    #[test]
    fn laughter_is_debounced() {
        let mut cfg = cfg();
        cfg.event_debounce_ms = 5000;
        let mut s = Stabilizer::new(cfg, Instant::now());
        let t = Instant::now();
        assert!(s
            .on_transcript("<|NEUTRAL|>", "<|Laughter|>", t)
            .contains(&PetEventKind::Laugh));
        // 紧接着的第二次笑声应当被去抖吃掉
        assert!(!s
            .on_transcript("<|NEUTRAL|>", "<|Laughter|>", t)
            .contains(&PetEventKind::Laugh));
    }

    #[test]
    fn voice_start_is_emitted_once_per_utterance() {
        let mut s = Stabilizer::new(cfg(), Instant::now());
        let t = Instant::now();
        assert_eq!(s.on_voice_start(t), vec![PetEventKind::VoiceStart]);
        assert!(s.on_voice_start(t).is_empty());
        assert_eq!(s.on_voice_end(t), vec![PetEventKind::VoiceEnd]);
        assert!(s.on_voice_end(t).is_empty());
    }

    #[test]
    fn idle_fires_only_after_timeout() {
        let t = Instant::now();
        let mut s = Stabilizer::new(cfg(), t);
        s.on_voice_start(t);
        s.on_voice_end(t);

        // 超时未到
        assert_eq!(s.poll_idle(t + Duration::from_millis(500)), None);
        // 超时已到
        assert_eq!(
            s.poll_idle(t + Duration::from_millis(1500)),
            Some(PetEventKind::Idle)
        );
        // 不应重复触发
        assert_eq!(s.poll_idle(t + Duration::from_millis(3000)), None);
    }

    #[test]
    fn idle_fires_from_cold_start_without_any_voice() {
        // 冷启动后一直没人说话时也必须能进待机。
        // 早先把 `last_voice_at` 初始化成 `None` 会让这里永远是 None，
        // 静音超时形同虚设 —— 阶段 0 实测「静音 20 秒却 0 个事件」就是这个原因。
        let t = Instant::now();
        let mut s = Stabilizer::new(cfg(), t);
        assert_eq!(s.poll_idle(t + Duration::from_millis(500)), None);
        assert_eq!(
            s.poll_idle(t + Duration::from_millis(1500)),
            Some(PetEventKind::Idle)
        );
    }
}
