import type { PetEventKind } from './events'

/** 第一版预置的 7 个动作，对应 DEV.md §4.3 的表格 */
export type ActionName = 'idle' | 'speaking' | 'happy' | 'angry' | 'sad' | 'surprised' | 'laugh'

export const ACTION_NAMES: readonly ActionName[] = [
  'idle',
  'speaking',
  'happy',
  'angry',
  'sad',
  'surprised',
  'laugh',
]

/**
 * 动作的运动参数，值直接作为 CSS 自定义属性注入。
 * 全部来自 theme.json，组件里不硬编码任何数值。
 */
export interface ActionParams {
  /** CSS 时间值，如 "13s" / "900ms" */
  duration?: string
  /** 倾角，如 "3.5deg" */
  tilt?: string
  /** 水平漂移距离 */
  drift?: string
  /** 垂直弹跳/浮动距离 */
  bob?: string
  /** 呼吸缩放增量（无单位） */
  breath?: string
  /** 高频震动幅度 */
  shake?: string
  /** 下沉距离 */
  sink?: string
  /** 惊讶时的放大倍数 */
  pop?: string
}

export interface ActionConfig {
  /** 数字越大越优先；高优先级可中断低优先级 */
  priority: number
  /** 是否为持续循环动作（idle / speaking） */
  loop: boolean
  /** 非循环动作的持续时间；循环动作为 null */
  durationMs: number | null
  /** 同一动作的冷却时间，避免重复触发 */
  cooldownMs: number
  params: ActionParams
}

export interface ThemeDefinition {
  id: string
  name: string
  version: string
  assets: {
    /** 基础素材文件名，相对主题目录 */
    base: string
    /**
     * 额外表情图。第一版为空；未来加入 happy.png 等后，
     * 只需要在这里登记，动画层无需改动（见 DEV.md §4.5）。
     */
    expressions: Partial<Record<ActionName, string>>
  }
  // 这里曾经有个 `window: { width, height }`。它是死字段：窗口尺寸由
  // `src-tauri/tauri.conf.json` 的 `app.windows[0]` 决定，theme.json 里那份
  // 从来没有代码读过，改了不生效。删掉以免有人以为调主题就能改窗口大小。
  scheduler: {
    idleTimeoutMs: number
    defaultCooldownMs: number
  }
  baseState: {
    voiceActive: ActionName
    silent: ActionName
  }
  /** 标准化事件 → 动画动作的映射。换模型或换主题时只改这里（DEV.md §5.5） */
  eventToAction: Partial<Record<PetEventKind, ActionName>>
  actions: Record<ActionName, ActionConfig>
}
