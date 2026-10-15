//! SenseVoice 推理封装。
//!
//! 这是本项目里唯一读模型输出情绪的地方，也是唯一依赖 vendor 补丁的地方：
//! 上游 `sherpa_onnx` 的 `OfflineRecognizerResult` 丢掉了 json 里的
//! lang/emotion/event 三个字段，补丁见 `vendor/sherpa-onnx/src/offline_asr.rs`。

use crate::config::RecognizerConfig;
use crate::paths;
use anyhow::{Context, Result};
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig};
use std::path::Path;
use std::time::Instant;

/// 一次解码的结构化结果
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub text: String,
    /// 形如 `<|HAPPY|>`，模型未返回时为空串
    pub emotion_tag: String,
    /// 形如 `<|Laughter|>`，模型未返回时为空串
    pub event_tag: String,
    /// 形如 `<|zh|>`，模型未返回时为空串
    pub lang_tag: String,
    /// 本次推理耗时，用于阶段 0 的延迟记录
    pub elapsed_ms: u128,
}

pub struct Recognizer {
    inner: OfflineRecognizer,
    sample_rate: i32,
}

impl Recognizer {
    pub fn new(cfg: &RecognizerConfig, sample_rate: i32, models_dir: &Path) -> Result<Self> {
        let model_path = paths::resolve(models_dir, &cfg.sense_voice_model);
        let tokens_path = paths::resolve(models_dir, &cfg.tokens);

        // 提前检查文件，给出比「模型加载失败」更有用的提示
        for path in [&model_path, &tokens_path] {
            if !path.is_file() {
                anyhow::bail!(
                    "找不到 SenseVoice 资源: {}\n请先运行 `npm run fetch-models` 下载模型资源。",
                    path.display()
                );
            }
        }

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.sense_voice.model = Some(model_path.to_string_lossy().into_owned());
        config.model_config.sense_voice.language = Some(cfg.language.clone());
        config.model_config.sense_voice.use_itn = cfg.use_itn;
        config.model_config.tokens = Some(tokens_path.to_string_lossy().into_owned());
        config.model_config.num_threads = cfg.num_threads;
        config.model_config.debug = false;

        let started = Instant::now();
        let inner = OfflineRecognizer::create(&config)
            .context("创建 SenseVoice 识别器失败（模型文件可能损坏）")?;
        log::info!("SenseVoice 模型加载完成，耗时 {:.0} ms", started.elapsed().as_millis());

        Ok(Self { inner, sample_rate })
    }

    /// 对一段音频（已重采样到目标采样率）做一次完整解码。
    pub fn transcribe(&self, samples: &[f32]) -> Option<Transcript> {
        if samples.is_empty() {
            return None;
        }

        let started = Instant::now();
        let stream = self.inner.create_stream();
        stream.accept_waveform(self.sample_rate, samples);
        self.inner.decode(&stream);
        let elapsed_ms = started.elapsed().as_millis();

        let result = stream.get_result()?;
        Some(Transcript {
            text: result.text,
            emotion_tag: result.emotion,
            event_tag: result.event,
            lang_tag: result.lang,
            elapsed_ms,
        })
    }
}
