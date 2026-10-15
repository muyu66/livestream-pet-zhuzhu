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
use cpal::{DeviceId, ErrorKind, Sample, SampleFormat, Stream, StreamConfig};
use crossbeam_channel::Sender;
use serde::Serialize;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// 驱动明确报了「这台设备没了」，采集流已经死了。
    ///
    /// 判据是 `ErrorKind::DeviceNotAvailable`：cpal 0.18 的文档写明这个 kind 的含义就是
    /// 「设备在运行中被拔掉，或这个 id 指向的设备根本不存在」；而 `DeviceChanged`
    /// （另一种常见的路由变化通知）**只会从默认设备的流上发出** ——
    /// 我们这条流是按具体设备建的，所以不会收到它，也就不会把「换了默认设备」误判成掉线。
    /// 见 cpal error.rs 里 `DeviceChanged` / `DeviceNotAvailable` 的文档。
    pub device_lost: Arc<AtomicBool>,
}

impl CaptureStream {
    pub fn play(&self) -> Result<()> {
        self.stream.play().context("启动音频流失败")
    }
}

/// 一只供用户选择的输入设备。
///
/// `id` 是 cpal 的 `DeviceId` 字符串（`"<后端>:<设备>"`，Windows 上形如
/// `wasapi:{0.0.1.00000000}.{guid}`）。选择框把它交给 Rust 存进 `settings.json`，
/// 下次启动就按它把那台设备找回来 —— cpal 的文档明确建议如此持久化 `DeviceId`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// 枚举可用输入设备。
///
/// 判定默认设备**只能比 `DeviceId`，不能比 `Device` 本身**。
/// cpal 0.18 的 `Device` 虽然实现了 `Eq`，但 WASAPI 下的实现是
/// `(DefaultInput, DefaultInput) => true`、`(Specific(a), Specific(b)) => 比较 endpoint`，
/// 其余一律 `false` —— 也就是说 `default_input_device()` 返回的 `DefaultInput`
/// 与枚举出来的 `Specific(endpoint)` **永远不会相等**，直接比会永远判不出默认设备。
/// 而 `id()` 会把两种 handle 都解析成真实 endpoint 的 ID，所以比 ID 才是可靠的。
/// 也不能比名字：同型号麦克风插两个时名字完全相同。
pub fn list_input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_id = host.default_input_device().and_then(|d| d.id().ok());

    let mut devices = Vec::new();
    for device in host.input_devices().context("枚举输入设备失败")? {
        let id = match device.id() {
            Ok(id) => id.to_string(),
            // 取不到 id 的设备**没法被选中**（存下去也没有身份可比），
            // 与其在列表里放一个点不动的行，不如跳过并说明。
            Err(err) => {
                log::warn!("跳过一台取不到 id 的输入设备「{}」: {err}", device_name(&device));
                continue;
            }
        };
        let is_default = default_id.as_ref().is_some_and(|d| d.to_string() == id);
        devices.push(DeviceInfo {
            id,
            name: device_name(&device),
            is_default,
        });
    }
    Ok(devices)
}

/// 存档里的那台设备现在还在不在。
///
/// 给工作线程在**加载模型之前**用：设备已经没了（拔掉了，或换个 USB 口导致 WASAPI
/// 端点 id 变了）时，没必要先花一秒多加载 VAD 与识别模型，再报一个本来就知道的失败。
///
/// 只认 id：名字可能重复（见 [`list_input_devices`] 的说明），拿名字找会找错人。
pub fn device_is_available(id: &str) -> bool {
    match DeviceId::from_str(id) {
        Ok(parsed) => cpal::default_host().device_by_id(&parsed).is_some(),
        Err(err) => {
            log::warn!("存档里的麦克风 id 看不懂: {id} ({err})");
            false
        }
    }
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

/// 打开输入设备并启动采集。
///
/// `device_id` 是 [`DeviceInfo::id`]（也就是存档里那份）；`None` 表示用系统默认输入设备
/// —— 只有 `examples/audio_probe.rs` 走这条路，应用本体永远传具体设备：
/// 「用哪只麦克风」是用户明确选的（或明确选了「不监听」，那就根本不会走到这里）。
///
/// 必须在**将来要长期持有它的那条线程**上调用：cpal 的 `Stream` 在部分后端上
/// 不是 `Send`，跨线程传递会编译不过；而且流的析构要发生在创建设备的线程上。
/// `DeviceId` 本身是 `Send` 的，所以它可以跨线程传进来，在这一层才解析成 `Device`。
pub fn build_input_stream(tx: Sender<AudioChunk>, device_id: Option<&str>) -> Result<CaptureStream> {
    let host = cpal::default_host();
    let device = match device_id {
        Some(id) => resolve_device(&host, id)?,
        None => host
            .default_input_device()
            .context("没有找到默认输入设备，请确认系统存在可用麦克风")?,
    };
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
    let device_lost = Arc::new(AtomicBool::new(false));
    let on_error = {
        let dropped = dropped_chunks.clone();
        let errors = capture_errors.clone();
        let lost = device_lost.clone();
        let device = name.clone();
        move |err: cpal::Error| {
            // 采集期间设备被拔掉/被独占时最常见的就是这里。
            //
            // WASAPI 的 underrun/overrun 通知会成串出现，而且是**可以无害的**：
            // 实测蓝牙耳机上一次会话 3 分钟报了 70 次，同期「未接住的音频块」是 0，
            // 识别转写也完全正常 —— 说明它更像设备侧的缓冲区抖动通知，
            // 而不是我们真的丢了音频。所以前几次按 error 打（让人看得见），
            // 之后降到 debug：总次数在收尾时统一汇报，调试面板里也实时显示，
            // 信息不会丢，但不至于把日志刷满。
            //
            // **只有 `DeviceNotAvailable` 才算掉线**，别的错误（含上面那种 xrun）一律不算 ——
            // 「报过错就换设备」会把一台好麦克风判死。设备真的没了时驱动会先发一条这个错误
            // 再停掉回调（WASAPI 的 run_input 就是这么做的），所以这里是掉线唯一的权威信号，
            // 工作线程只负责把它转成「停采集 + 弹选择框」。
            if err.kind() == ErrorKind::DeviceNotAvailable && !lost.swap(true, Ordering::Relaxed) {
                log::error!("麦克风「{device}」在采集过程中断开（设备已不可用）");
            }

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
        device_lost,
    })
}

/// 按存档里的 id 把那台设备找回来。
///
/// 找不到时给出的信息要能让用户**据此做决定**（「去插上那只麦」还是「换一只」），
/// 所以名字里带上存档里的原文：看起来像 `wasapi:{0.0.1...}.{guid}` 的 id 对人毫无意义。
fn resolve_device(host: &cpal::Host, id: &str) -> Result<cpal::Device> {
    let parsed = DeviceId::from_str(id).with_context(|| {
        format!("存档里的麦克风 id 不是合法格式（{id}），配置文件可能被改坏了")
    })?;

    host.device_by_id(&parsed).with_context(|| {
        format!("存档的麦克风现在找不到了（可能被拔掉、换了 USB 口或改名了）: {id}")
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
