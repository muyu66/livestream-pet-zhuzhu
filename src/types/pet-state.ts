import type { PetEvent } from './events'
import type { ExpressionName, MotionName } from './theme'

/**
 * 角色窗口 → 调试窗口的状态广播。
 *
 * 动画状态住在角色窗口的调度器里，调试窗口只是个订阅方。之所以做成广播而不是
 * 「调试窗口去问角色窗口」：状态变化本身就是推模型，问的方式还要多一套请求-响应，
 * 而且调试窗口开着的时候角色窗口并不知道。
 *
 * 这条通道**不参与动画决策**，只是把现有状态抄一份给调试界面看。
 */
export const PET_STATE_CHANNEL = 'pet://state'

/**
 * 调试窗口刚打开时，要求角色窗口立刻补发一份当前状态。
 *
 * 没有它的话，角色一直待机、状态不变，调试窗口会一直是空的 ——
 * 看起来像「连不上」，其实只是没赶上上一次广播。
 */
export const PET_STATE_REQUEST_CHANNEL = 'pet://state-request'

/**
 * 角色窗口的动画状态快照。
 *
 * 拆成三个标量（表情 / 动作 / 第几步）而不是一个字符串：广播是跨窗口的 JSON，
 * 拼成一个显示字符串的话，调试窗口就只能拿它给人看、没法再判断什么；
 * 而且这几个字段本来就各自变化（同一个表情里动作一直在换）。
 */
export interface PetState {
  /** 当前表情（决定用哪张图） */
  expression: ExpressionName
  /** 当前这一拍的动作名 */
  motion: MotionName
  /** 第几步（0 起）与这一列一共几步 */
  stepIndex: number
  stepCount: number
  voiceActive: boolean
  /**
   * 调度器里还活着的定时器个数，正常不会超过 2（见 scheduler.ts）。
   * 待机循环时**稳定是 2**（这一拍的兜底 + 静音超时），不是 0 —— 只增不减才是泄漏。
   */
  pendingTimers: number
  /** 最近一次收到的标准化事件，没收到过就是 null */
  lastEvent: PetEvent | null
}

/**
 * 校验广播来的快照。
 *
 * 只查最关键的几个字段：这是调试展示用的数据，字段不全顶多显示不全，
 * 不值得为它写一套完整校验；但整个载荷类型不对（比如拿旧版本的事件当快照）
 * 必须挡掉，否则界面会渲染出 `undefined @ undefined` 这种假信息。
 */
export function isPetState(value: unknown): value is PetState {
  if (typeof value !== 'object' || value === null) return false
  const state = value as Partial<PetState>
  return typeof state.expression === 'string' && typeof state.pendingTimers === 'number'
}
