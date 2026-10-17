//! 运行期音频配置。
//!
//! DEV.md §5.5 / §5.6 要求映射关系、静音超时、各类阈值都「必须可配置」，
//! 所以这些数值全部来自 `config/audio.json`，代码里只保留默认值作为兜底。
//!
//! 注意区分两类配置：
//!
//!  * 这里（Rust）管「检测」：VAD 参数、模型参数、稳定化阈值。
//!  * `themes/zhuzhu/theme.json`（前端）管「表现」：事件→动作映射、动画参数。
//!
//! 换模型只动前者，换主题只动后者，互不影响。

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// 推理与 VAD 统一使用 16 kHz 单声道
    pub sample_rate: i32,
    pub vad: VadConfig,
    pub recognizer: RecognizerConfig,
    pub stability: StabilityConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VadConfig {
    /// 相对 models/ 的路径
    pub silero_model: String,
    /// 语音判定阈值，越高越保守
    pub threshold: f32,
    /// 短于此长度的语音片段被判为噪声丢弃
    pub min_speech_duration: f32,
    /// silero 要求的最小静音长度，用于切分句子
    pub min_silence_duration: f32,
    /// 单个语音片段的最长时长，防止一直不切分导致延迟无限增长
    pub max_speech_duration: f32,
    /// Silero VAD 固定窗口，必须 512（16 kHz 下约 32 ms）
    pub window_size: i32,
    /// VAD 内部环形缓冲秒数
    pub buffer_seconds: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RecognizerConfig {
    /// 相对 models/ 的路径
    pub sense_voice_model: String,
    pub tokens: String,
    /// "auto" 让模型自行判语种，默认固定 "zh"（见 audio.json 的 `_language_说明`）
    pub language: String,
    /// 逆文本归一化（把「一二三」转成「123」）。第一版不需要，关掉省一点时间。
    pub use_itn: bool,
    pub num_threads: i32,

    /// 说话过程中做「中间解码」的间隔。SenseVoice 是离线模型，
    /// 只在语音段结束时才解码的话，长句子要等到切分才有情绪反馈（最长可达
    /// max_speech_duration）。按固定间隔对最近一段音频做一次解码，
    /// 可以把情绪反馈延迟压到接近实时（DEV.md §5.3）。
    /// 设为 0 关闭中间解码（省 CPU，但情绪响应会明显变慢）。
    pub interim_interval_ms: u64,
    /// 中间解码只取「最近这么多秒」的音频。固定窗口让单次推理耗时不随说话时长增长。
    pub interim_window_secs: f32,
    /// 本段语音至少要攒够这么多秒，才开始做中间解码。
    ///
    /// 不设这个下限会出两个问题，实测都会碰到：
    ///  * 语音刚被 VAD 确认时 `recent` 里几乎全是上一句之后的静音，解出来是空文本 +
    ///    `<|EMO_UNKNOWN|>`，白白花掉一次完整推理，还往一致性窗口里塞一个无效观察；
    ///  * 短句（比 `interim_window_secs` 还短）的「最近 N 秒」窗口就等于整段音频，
    ///    于是同一次说话会先出一条中间结果、再出一条一模一样的语音段结果 ——
    ///    同一次推理做了两遍，调试面板里看着也像是重复上报。
    ///
    /// 所以下限取「比 `interim_window_secs` 略大」：只有明显长于窗口的句子才值得
    /// 抢在切分之前先给一次反馈，此时中间窗口必然是整段的真子集，不会重复。
    /// 短句本来就会在 `min_silence_duration` 后立刻切分，中间解码对它没有价值。
    pub interim_min_speech_secs: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StabilityConfig {
    /// 静音超时：超过这个时间没检测到人声就进入待机（DEV.md §5.5）
    pub idle_timeout_ms: u64,
    /// 一致性的窗口数：连续这么多次**有效**解码得到同一情绪才上报。
    /// 由于模型不给置信度，用「窗口一致性」代替「阈值筛选」。
    ///
    /// 窗口**跨语音段**累计：DEV.md §5.6 的原话是「连续 N 个语音段得到同一情绪」。
    /// 同一次说话的多次解码不能各算一票 —— 那等于让同一段音频自己给自己投票，
    /// 筛选作用归零（实测中间解码与紧随其后的语音段解码会给出完全一样的结果）。
    /// 窗口只在确认到新情绪时向前滑动，不按时间清空。
    /// `Unknown`（模型这次什么都没给出）不算有效观察，不占窗口位置。
    pub emotion_consistency_windows: u32,
    /// 无事件去抖窗口，抑制同一类事件的高频重复
    pub event_debounce_ms: u64,
    /// 情绪落地后的一段锁定期，期间不再切换情绪，避免来回抖
    pub emotion_hold_ms: u64,
}

// ------------------------------------------------------------------ defaults

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            vad: VadConfig::default(),
            recognizer: RecognizerConfig::default(),
            stability: StabilityConfig::default(),
        }
    }
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            silero_model: "silero_vad.onnx".into(),
            threshold: 0.5,
            min_speech_duration: 0.25,
            // 比 sherpa-onnx 示例的 0.1 略长：直播场景里若切得太碎，
            // 每次切分都会触发一次解码，CPU 和动画抖动都会变多。
            min_silence_duration: 0.35,
            max_speech_duration: 8.0,
            window_size: 512,
            buffer_seconds: 30.0,
        }
    }
}

impl Default for RecognizerConfig {
    fn default() -> Self {
        Self {
            sense_voice_model: "sense-voice/model.int8.onnx".into(),
            tokens: "sense-voice/tokens.txt".into(),
            language: "zh".into(),
            use_itn: false,
            num_threads: 2,
            interim_interval_ms: 700,
            interim_window_secs: 2.5,
            // 比上面的窗口长 0.5 s，保证中间解码看到的是整段的真子集
            interim_min_speech_secs: 3.0,
        }
    }
}

impl Default for StabilityConfig {
    fn default() -> Self {
        Self {
            idle_timeout_ms: 4000,
            // 1 次一致性：判到就报，交给 event_debounce_ms / emotion_hold_ms 兜底。
            // 原先默认 2，实测同一句话反复说时 SenseVoice 会在 NEUTRAL/ANGRY 之间跳，
            // 交替的标签永远凑不齐连续两次，日志里有情绪而动画一直不出。
            emotion_consistency_windows: 1,
            event_debounce_ms: 1200,
            emotion_hold_ms: 1800,
        }
    }
}

impl AudioConfig {
    /// 从 `config/audio.json` 读取；文件不存在或解析失败时退回默认值并给出警告，
    /// 不因为一个配置文件的笔误就让应用起不来。
    pub fn load() -> Self {
        let path = crate::paths::config_dir().join("audio.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<AudioConfig>(&text) {
                Ok(cfg) => {
                    log::info!("已加载音频配置: {}", path.display());
                    cfg
                }
                Err(err) => {
                    log::warn!("{} 解析失败，使用默认配置: {err}", path.display());
                    AudioConfig::default()
                }
            },
            Err(err) => {
                log::warn!("读取 {} 失败，使用默认配置: {err}", path.display());
                AudioConfig::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = AudioConfig::default();
        assert_eq!(cfg.vad.window_size, 512, "Silero VAD 窗口固定为 512");
        assert_eq!(cfg.sample_rate, 16_000);
        assert!(cfg.stability.emotion_consistency_windows >= 1);
    }

    #[test]
    fn interim_window_is_shorter_than_the_minimum_speech() {
        // 中间窗口必须严格短于「开始做中间解码所需的本段时长」，
        // 否则短句的中间窗口就等于整段音频，同一次说话会被解码两遍，
        // 中间结果和语音段结果一字不差。
        let cfg = RecognizerConfig::default();
        assert!(
            cfg.interim_min_speech_secs > cfg.interim_window_secs,
            "interim_min_speech_secs({}) 必须大于 interim_window_secs({})",
            cfg.interim_min_speech_secs,
            cfg.interim_window_secs
        );
    }

    #[test]
    fn partial_json_falls_back_to_defaults() {
        // 只写一个字段，其余应当由 serde(default) 补齐，而不是解析失败
        let cfg: AudioConfig = serde_json::from_str(r#"{"vad":{"threshold":0.7}}"#).unwrap();
        assert_eq!(cfg.vad.threshold, 0.7);
        assert_eq!(cfg.stability.idle_timeout_ms, 4000);
    }
}
