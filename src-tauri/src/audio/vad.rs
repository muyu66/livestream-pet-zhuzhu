//! Silero VAD 封装（DEV.md §3：不自研 VAD，用 sherpa-onnx 已有的实现）。
//!
//! VAD 在本项目里只回答一个问题：**有没有人声**。
//! 情绪标签和笑声等事件一律交给 SenseVoice，两者不混为一谈（DEV.md §5.4）。

use crate::config::VadConfig;
use crate::paths;
use anyhow::{Context, Result};
use sherpa_onnx::{VadModelConfig, VoiceActivityDetector};
use std::path::Path;

pub struct Vad {
    inner: VoiceActivityDetector,
    window_size: usize,
}

impl Vad {
    pub fn new(cfg: &VadConfig, sample_rate: i32, models_dir: &Path) -> Result<Self> {
        let model_path = paths::resolve(models_dir, &cfg.silero_model);
        if !model_path.is_file() {
            anyhow::bail!(
                "找不到 Silero VAD 模型: {}\n请先运行 `npm run fetch-models` 下载模型资源。",
                model_path.display()
            );
        }

        let mut model_config = VadModelConfig::default();
        model_config.silero_vad.model = Some(model_path.to_string_lossy().into_owned());
        model_config.silero_vad.threshold = cfg.threshold;
        model_config.silero_vad.min_silence_duration = cfg.min_silence_duration;
        model_config.silero_vad.min_speech_duration = cfg.min_speech_duration;
        model_config.silero_vad.max_speech_duration = cfg.max_speech_duration;
        model_config.silero_vad.window_size = cfg.window_size;
        model_config.sample_rate = sample_rate;
        model_config.debug = false;

        let inner = VoiceActivityDetector::create(&model_config, cfg.buffer_seconds)
            .context("创建 Silero VAD 失败（模型文件可能损坏）")?;

        Ok(Self {
            inner,
            window_size: cfg.window_size.max(1) as usize,
        })
    }

    /// 送入 VAD 的窗口大小（采样点数）。调用方需要按这个粒度切分音频。
    pub fn window_size(&self) -> usize {
        self.window_size
    }

    pub fn accept(&self, samples: &[f32]) {
        self.inner.accept_waveform(samples);
    }

    /// 当前是否处于「检测到人声」状态
    pub fn detected(&self) -> bool {
        self.inner.detected()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 取出一个已完成的语音段（同时从 VAD 队列移除）。
    ///
    /// 这里把采样点拷成 `Vec<f32>`：`SpeechSegment` 的生命周期绑定在 VAD 上，
    /// 而后续推理需要在没有 VAD 借用的情况下进行。
    pub fn take_segment(&self) -> Option<Vec<f32>> {
        let segment = self.inner.front()?;
        let samples = segment.samples().to_vec();
        drop(segment);
        self.inner.pop();
        Some(samples)
    }
}
