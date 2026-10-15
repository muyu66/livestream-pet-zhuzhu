import { ref, type Ref } from 'vue'
import type { PetEventKind } from '../types/events'
import type { ActionName, ThemeDefinition } from '../types/theme'

/**
 * 统一动画调度器（DEV.md §4.4）。
 *
 * 设计要点：
 *  - 任意时刻只有一个主动画状态，不存在「每种情绪各自跑一个永不停止的定时器」。
 *  - 循环动作（idle / speaking）常驻；一次性动作用 animationend 收尾，
 *    并配一个**唯一**的兜底定时器 —— 每次切换都会先清掉上一个，不会累积实例。
 *  - 高优先级可中断低优先级；同优先级不重启，保证「新事件不能无限启动新动画实例」。
 *  - dispose() 清空全部定时器，程序退出后不遗留动画任务。
 */
export class AnimationScheduler {
  /** 当前动作，供视图绑定 class */
  readonly current: Ref<ActionName>
  /** 当前是否处于「主播正在说话」状态 */
  readonly voiceActive: Ref<boolean>

  private readonly theme: ThemeDefinition
  /** 无事件时应当回到的动作：speaking（人声刚结束的宽限期）或 idle */
  private baseAction: ActionName
  /** 每个动作上次真正进入的时间，用于冷却判断 */
  private readonly lastEnteredAt = new Map<ActionName, number>()
  /** 一次性动作的兜底定时器，同一时刻最多一个 */
  private endTimer: ReturnType<typeof setTimeout> | null = null
  /** 静音超时定时器 */
  private idleTimer: ReturnType<typeof setTimeout> | null = null
  private disposed = false

  constructor(theme: ThemeDefinition) {
    this.theme = theme
    this.baseAction = theme.baseState.silent
    this.current = ref<ActionName>(theme.baseState.silent)
    this.voiceActive = ref(false)
  }

  /** 诊断用：确认定时器没有泄漏 */
  get pendingTimers(): number {
    return (this.endTimer === null ? 0 : 1) + (this.idleTimer === null ? 0 : 1)
  }

  /** 处理一个标准化事件 */
  dispatch(kind: PetEventKind): void {
    if (this.disposed) return

    this.noteActivity()

    switch (kind) {
      case 'voice_start':
      case 'neutral':
        // 说话是基准状态，优先级低于任何情绪动作，所以不能打断正在播的一次性动作。
        this.voiceActive.value = true
        this.baseAction = this.theme.baseState.voiceActive
        this.revertToBase(false)
        return

      case 'voice_end':
        // DEV.md §5.5：voice_end 不立刻回 idle，先等静音超时。
        this.voiceActive.value = false
        return

      case 'idle':
        this.voiceActive.value = false
        this.baseAction = this.theme.baseState.silent
        this.revertToBase(false)
        return

      default: {
        const action = this.theme.eventToAction[kind]
        if (!action) return
        if (this.theme.actions[action].loop) {
          this.baseAction = action
          this.revertToBase(false)
        } else {
          this.triggerTemporary(action)
        }
      }
    }
  }

  /** 由组件在 animationend 时回调，用于结束一次性动作 */
  handleAnimationEnd(action: ActionName): void {
    if (this.disposed) return
    const config = this.theme.actions[action]
    if (config.loop) return
    this.finishTemporary(action)
  }

  dispose(): void {
    this.disposed = true
    this.clearEndTimer()
    this.clearIdleTimer()
  }

  // ------------------------------------------------------------- internals

  /**
   * 尝试进入一个一次性（情绪）动作。
   * 两道闸门：优先级中断控制 + 冷却时间。
   */
  private triggerTemporary(action: ActionName): void {
    const config = this.theme.actions[action]
    const currentAction = this.current.value
    const currentConfig = this.theme.actions[currentAction]

    // 中断控制：正在播的一次性动作，只有更高优先级才能打断它。
    if (!currentConfig.loop && config.priority <= currentConfig.priority) {
      return
    }

    // 冷却：避免同一种情绪动画被反复触发。
    const now = Date.now()
    const last = this.lastEnteredAt.get(action)
    if (last !== undefined && now - last < config.cooldownMs) {
      return
    }

    this.enter(action)
  }

  /** 无差别切换到指定动作，并重置计时 */
  private enter(action: ActionName): void {
    this.clearEndTimer()

    const config = this.theme.actions[action]
    this.current.value = action
    this.lastEnteredAt.set(action, Date.now())

    if (!config.loop) {
      const holdMs = config.durationMs ?? this.theme.scheduler.defaultCooldownMs
      // 兜底：正常情况下 animationend 先到；这里多留一点余量，
      // 以免因为掉帧或标签页不可见导致 animationend 不触发而卡住状态。
      this.endTimer = setTimeout(() => this.finishTemporary(action), holdMs + 120)
    }
  }

  private finishTemporary(action: ActionName): void {
    // 期间可能已经被更高优先级的事件切走了，此时什么都不该做。
    if (this.current.value !== action) return
    this.clearEndTimer()
    this.revertToBase(true)
  }

  /**
   * 回到基准状态。
   * @param force true 表示一次性动作播完了，无条件回退；
   *              false 表示只是基准状态变了，正在播的一次性动作应当继续播完。
   */
  private revertToBase(force: boolean): void {
    const currentConfig = this.theme.actions[this.current.value]
    if (!force && !currentConfig.loop) return
    if (this.current.value === this.baseAction && this.endTimer === null) return
    this.enter(this.baseAction)
  }

  /** 每次收到事件都重置静音超时计时 */
  private noteActivity(): void {
    this.clearIdleTimer()
    this.idleTimer = setTimeout(() => {
      this.idleTimer = null
      if (this.disposed) return
      this.voiceActive.value = false
      this.baseAction = this.theme.baseState.silent
      // 不打断正在播的情绪动作，让它自然收尾后再回待机。
      this.revertToBase(false)
    }, this.theme.scheduler.idleTimeoutMs)
  }

  private clearEndTimer(): void {
    if (this.endTimer !== null) {
      clearTimeout(this.endTimer)
      this.endTimer = null
    }
  }

  private clearIdleTimer(): void {
    if (this.idleTimer !== null) {
      clearTimeout(this.idleTimer)
      this.idleTimer = null
    }
  }
}
