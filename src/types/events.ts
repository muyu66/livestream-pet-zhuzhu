/**
 * 标准化角色事件契约。
 *
 * 这份定义必须与 Rust 侧 `src-tauri/src/events.rs` 的 `PetEvent` 保持一致：
 * 后端只把「已稳定过」的粗粒度事件推给前端，前端不接触原始音频。
 *
 * 注意：这里刻意没有 `confidence` 字段。
 * SenseVoice 不返回可用置信度，DEV.md §5.6 明确禁止凭空生成置信度，
 * 因此稳定性依靠「连续窗口一致性 + 最短持续时间 + 冷却」实现，而不是靠分数。
 */
export type PetEventKind =
  /** 检测到主播开始说话（VAD） */
  | 'voice_start'
  /** 一段语音结束（VAD） */
  | 'voice_end'
  /** 一段时间未检测到人声 */
  | 'idle'
  /** 情绪偏开心 */
  | 'happy'
  /** 情绪偏生气 */
  | 'angry'
  /** 情绪偏难过 */
  | 'sad'
  /** 情绪偏惊讶 */
  | 'surprised'
  /** 检测到笑声 */
  | 'laugh'
  /** 没有明显目标情绪，继续普通说话状态 */
  | 'neutral'

export interface PetEvent {
  kind: PetEventKind
  /**
   * 事件产生时刻（Unix 毫秒）。**只用于诊断**：调试窗口显示「最近事件」时带上它，
   * 好看出两条事件之间隔了多久（动画层没有冷却计算，见 scheduler.ts）。
   */
  ts: number
}

/** 后端推送给前端的 Tauri 事件名 */
export const PET_EVENT_CHANNEL = 'pet://event'

/** 退出前通知前端清理动画定时器，避免退出时残留任务 */
export const PET_SHUTDOWN_CHANNEL = 'pet://shutdown'

/**
 * 置顶状态变化通道，负载是 boolean。
 *
 * 从托盘菜单切开置顶时，自绘菜单的文案也必须跟着变，
 * 否则会出现「菜单写着置顶、窗口却已经置顶」的不一致。
 */
export const PET_ALWAYS_ON_TOP_CHANNEL = 'pet://always-on-top'

/**
 * 菜单项。由 Rust 侧 `menu.rs` 生成，前端只负责渲染。
 * 两份菜单（托盘 + 角色右键）共用同一份定义，见 DEV.md §3.3。
 */
export interface MenuEntry {
  id: string
  label: string
}

/** 与 Rust `menu.rs` 中的常量对应 */
export const MENU_TOGGLE_TOPMOST = 'toggle-topmost'
export const MENU_QUIT = 'quit'

/**
 * 诊断信息通道。**不属于动画契约**。
 *
 * `pet://event` 上只有稳定后的粗粒度事件，原文、语言、模型原始 token、
 * 耗时这些排查用的中间信息在那条链路上是被刻意丢掉的。所以单开一条，
 * 且只在调试面板打开时推送。
 */
export const PET_DEBUG_CHANNEL = 'pet://debug'

/** 一次识别结果：模型的原始输出，尚未经过稳定化 */
export interface DebugTranscript {
  type: 'transcript'
  /** true = 语音段结束后的权威解码；false = 说话过程中的中间解码 */
  segment: boolean
  text: string
  lang: string
  emotion: string
  event: string
  audio_secs: number
  elapsed_ms: number
}

/** 采集电平：判断「麦克风有没有声音」只能靠它 */
export interface DebugLevel {
  type: 'level'
  peak: number
  rms: number
  dropped: number
  capture_errors: number
}

export type PetDebug = DebugTranscript | DebugLevel

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

export function isDebugTranscript(value: unknown): value is DebugTranscript {
  return (
    isRecord(value) &&
    value.type === 'transcript' &&
    typeof value.segment === 'boolean' &&
    typeof value.text === 'string'
  )
}

export function isDebugLevel(value: unknown): value is DebugLevel {
  return isRecord(value) && value.type === 'level' && typeof value.peak === 'number'
}

export function isPetDebug(value: unknown): value is PetDebug {
  return isDebugTranscript(value) || isDebugLevel(value)
}

export function isMenuEntry(value: unknown): value is MenuEntry {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as MenuEntry).id === 'string' &&
    typeof (value as MenuEntry).label === 'string'
  )
}

export const PET_EVENT_KINDS: readonly PetEventKind[] = [
  'voice_start',
  'voice_end',
  'idle',
  'happy',
  'angry',
  'sad',
  'surprised',
  'laugh',
  'neutral',
]

export function isPetEventKind(value: unknown): value is PetEventKind {
  return typeof value === 'string' && (PET_EVENT_KINDS as readonly string[]).includes(value)
}
