//! 麦克风采集（cpal）。
//!
//! DEV.md §5.2 的硬性约束：**采集回调里只做「混单声道 + 塞进有界通道」**，
//! 绝不做重采样、VAD 或模型推理。回调运行在音频驱动的高优先级线程上，
//! 一旦阻塞就会导致丢帧甚至设备报错。
//!
//! 通道满时选择丢弃当前块而不是阻塞 —— 对「直播陪伴」这个用途来说，
//! 丢掉一小段音频远好过让驱动线程卡住。

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, Stream, StreamConfig};
use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 采集通道容量（块数）。每块通常是几百个采样点，
/// 32 块在 16 kHz 下大约对应几百毫秒的缓冲，足够吸收推理抖动。
pub const CAPTURE_CHANNEL_CAPACITY: usize = 32;

/// 采集到的音频块。里面已经是「单声道 f32」。
pub type AudioChunk = Vec<f32>;

pub struct CaptureStream {
    stream: Stream,
    pub sample_rate: u32,
    pub channels: u16,
    /// 驱动实际给出的采样格式。诊断「有设备但全是 0」这类问题时，
    /// 光看采样率不够 —— 格式不对会让人误以为是设备没在工作。
    pub sample_format: SampleFormat,
    pub device_name: String,
    pub dropped_chunks: Arc<AtomicU64>,
    /// 驱动回调报错的累计次数。与 `dropped_chunks` 是两回事：
    /// 前者是设备层自己的通知，后者才是我们没接住的音频。
    pub capture_errors: Arc<AtomicU64>,
}

impl CaptureStream {
    pub fn play(&self) -> Result<()> {
        self.stream.play().context("启动音频流失败")
    }
}

/// 枚举可用输入设备，返回 (名称, 是否为默认设备)
///
/// 判定默认设备**只能比 `DeviceId`，不能比 `Device` 本身**。
/// cpal 0.18 的 `Device` 虽然实现了 `Eq`，但 WASAPI 下的实现是
/// `(DefaultInput, DefaultInput) => true`、`(Specific(a), Specific(b)) => 比较 endpoint`，
/// 其余一律 `false` —— 也就是说 `default_input_device()` 返回的 `DefaultInput`
/// 与枚举出来的 `Specific(endpoint)` **永远不会相等**，直接比会永远判不出默认设备。
/// 而 `id()` 会把两种 handle 都解析成真实 endpoint 的 ID，所以比 ID 才是可靠的。
/// 也不能比名字：同型号麦克风插两个时名字完全相同。
pub fn list_input_devices() -> Result<Vec<(String, bool)>> {
    let host = cpal::default_host();
    let default_id = host.default_input_device().and_then(|d| d.id().ok());

    let mut devices = Vec::new();
    for device in host.input_devices().context("枚举输入设备失败")? {
        let is_default = match (&default_id, device.id().ok()) {
            (Some(default), Some(id)) => *default == id,
            _ => false,
        };
        devices.push((device_name(&device), is_default));
    }
    Ok(devices)
}

/// 取设备名。
///
/// cpal 0.18 去掉了 `DeviceTrait::name()`，改由 `description()` 返回结构化描述
/// （名字、厂商、驱动、总线类型等）。取不到时退回 `Display`，
/// 这是 cpal 文档里给「只想要名字」的推荐做法。
fn device_name(device: &cpal::Device) -> String {
    match device.description() {
        Ok(desc) => desc.name().to_string(),
        Err(_) => device.to_string(),
    }
}

/// 打开默认输入设备并启动采集。
///
/// 必须在**将来要长期持有它的那条线程**上调用：cpal 的 `Stream` 在部分后端上
/// 不是 `Send`，跨线程传递会编译不过；而且流的析构要发生在创建设备的线程上。
pub fn build_input_stream(tx: Sender<AudioChunk>) -> Result<CaptureStream> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("没有找到默认输入设备，请确认系统存在可用麦克风")?;
    let name = device_name(&device);

    let supported = device
        .default_input_config()
        .with_context(|| format!("无法获取设备「{name}」的默认输入配置"))?;

    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let channels = config.channels as usize;
    // cpal 0.18 起 `SampleRate` 就是 `u32` 的别名，不再有 `.0` 字段
    let sample_rate = config.sample_rate;

    let dropped_chunks = Arc::new(AtomicU64::new(0));
    let capture_errors = Arc::new(AtomicU64::new(0));
    let on_error = {
        let dropped = dropped_chunks.clone();
        let errors = capture_errors.clone();
        move |err| {
            // 采集期间设备被拔掉/被独占时最常见的就是这里。
            //
            // 但 WASAPI 的 underrun/overrun 通知会成串出现，而且是**可以无害的**：
            // 实测蓝牙耳机上一次会话 3 分钟报了 70 次，同期「未接住的音频块」是 0，
            // 识别转写也完全正常 —— 说明它更像设备侧的缓冲区抖动通知，
            // 而不是我们真的丢了音频。所以前几次按 error 打（让人看得见），
            // 之后降到 debug：总次数在收尾时统一汇报，调试面板里也实时显示，
            // 信息不会丢，但不至于把日志刷满。
            let n = errors.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 3 {
                log::error!(
                    "音频采集出错（第 {n} 次，未接住的音频块 {}）: {err}",
                    dropped.load(Ordering::Relaxed)
                );
            } else {
                log::debug!(
                    "音频采集出错（第 {n} 次，未接住的音频块 {}）: {err}",
                    dropped.load(Ordering::Relaxed)
                );
            }
        }
    };

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, config, tx, on_error, channels, &dropped_chunks),
        SampleFormat::I16 => build_stream::<i16>(&device, config, tx, on_error, channels, &dropped_chunks),
        SampleFormat::U16 => build_stream::<u16>(&device, config, tx, on_error, channels, &dropped_chunks),
        SampleFormat::I32 => build_stream::<i32>(&device, config, tx, on_error, channels, &dropped_chunks),
        SampleFormat::U32 => build_stream::<u32>(&device, config, tx, on_error, channels, &dropped_chunks),
        other => anyhow::bail!("暂不支持该采样格式: {other:?}"),
    }?;

    Ok(CaptureStream {
        stream,
        sample_rate,
        channels: channels as u16,
        sample_format,
        device_name: name,
        dropped_chunks,
        capture_errors,
    })
}

/// 把任意采样格式统一转成 f32 单声道后塞进通道。
///
/// 注意 trait bound 的方向：我们要的是 `T -> f32`，所以约束写在 `f32: FromSample<T>`
/// 上，而不是 `T: FromSample<f32>`（后者是 f32→T，方向相反）。
fn build_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    tx: Sender<AudioChunk>,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
    channels: usize,
    dropped_chunks: &Arc<AtomicU64>,
) -> Result<Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let dropped = dropped_chunks.clone();

    let stream = device
        .build_input_stream(
            config,
            move |data: &[T], _info: &cpal::InputCallbackInfo| {
                if data.is_empty() || channels == 0 {
                    return;
                }

                // 混单声道：多声道取平均。这一步很轻，可以放在回调里；
                // 重采样、VAD、推理全部留给工作线程。
                let mono: AudioChunk = if channels == 1 {
                    data.iter().map(|&s| f32::from_sample(s)).collect()
                } else {
                    data.chunks(channels)
                        .map(|frame| {
                            let sum: f32 = frame.iter().map(|&s| f32::from_sample(s)).sum();
                            sum / channels as f32
                        })
                        .collect()
                };

                // 绝不阻塞：通道满就丢这一块，并记数供诊断。
                if tx.try_send(mono).is_err() {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            },
            on_error,
            None,
        )
        .context("创建输入音频流失败")?;

    Ok(stream)
}
