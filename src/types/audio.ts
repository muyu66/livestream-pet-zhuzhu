/**
 * 麦克风选择的状态契约。
 *
 * 必须与 Rust 侧保持一致：
 *  * `AudioDevice` ← `audio::capture::DeviceInfo`
 *  * `DeviceChoice` ← `window_state::DeviceChoice`（会写进 settings.json 的那份）
 *  * `AudioStatus` / `AudioChoice` ← `events.rs` 里同名类型
 */

/**
 * 一只可选的输入设备。
 *
 * `id` 是 cpal 的 `DeviceId` 字符串，**记住的是它、不是名字**：
 * 同型号麦克风插两只时名字完全一样。`name` 只用于显示。
 */
export interface AudioDevice {
  id: string
  name: string
  /** 系统当前的默认输入设备。只在列表里做个标记，不是一种可选状态 */
  is_default: boolean
}

/** 用户选定的设备。`null`（见 `AudioChoice.device`）= 不监听 */
export interface DeviceChoice {
  id: string
  name: string
}

/**
 * 「不监听」与「没配过」是两回事：
 *  * `off` —— 用户明确选了不监听，语气分析整套不启用；
 *  * `need_choice` —— 从来没配过，该弹出选择框让人挑。
 * 判据在后端是配置文件里 `audio` 这一节在不在。
 */
export type AudioStatus =
  /** 不监听：语气分析整套不启用 */
  | { kind: 'off' }
  /** 正在连接那只麦克风（加载模型 + 打开流），要一两秒 */
  | { kind: 'connecting' }
  /** 正在聆听 */
  | { kind: 'listening' }
  /** 手上没有能用的麦克风，等用户挑一只 */
  | { kind: 'need_choice' }
  /** 试过了，没成 */
  | { kind: 'failed'; reason: string }

/** 「用哪只麦克风 + 现在什么状态」，两者总是一起到达 */
export interface AudioChoice {
  device: DeviceChoice | null
  status: AudioStatus
}

/** 音频状态通道。与 `pet://debug` 分开：那条是排查用的可选数据流，这条是状态 */
export const PET_AUDIO_CHANNEL = 'pet://audio'

/** 实时电平通道。只在选择框打开时推送 */
export const PET_LEVEL_CHANNEL = 'pet://level'

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

export function isAudioDevice(value: unknown): value is AudioDevice {
  return (
    isRecord(value) &&
    typeof value.id === 'string' &&
    typeof value.name === 'string' &&
    typeof value.is_default === 'boolean'
  )
}

export function isDeviceChoice(value: unknown): value is DeviceChoice {
  return isRecord(value) && typeof value.id === 'string' && typeof value.name === 'string'
}

const AUDIO_STATUS_KINDS = ['off', 'connecting', 'listening', 'need_choice', 'failed'] as const

export function isAudioStatus(value: unknown): value is AudioStatus {
  if (!isRecord(value)) return false
  const kind = value.kind
  if (typeof kind !== 'string' || !(AUDIO_STATUS_KINDS as readonly string[]).includes(kind)) {
    return false
  }
  // failed 多带一个原因，缺了它就说不清「为什么没听成」
  return kind !== 'failed' || typeof value.reason === 'string'
}

export function isAudioChoice(value: unknown): value is AudioChoice {
  if (!isRecord(value)) return false
  const device = value.device
  return (device === null || isDeviceChoice(device)) && isAudioStatus(value.status)
}
