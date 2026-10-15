import { ref, type Ref } from 'vue'
import type { PetEventKind } from '../types/events'
import type { ExpressionName, MotionName, ThemeDefinition } from '../types/theme'

/** 兜底定时器比动作时长多留的余量：掉帧、标签页不可见时不至于卡在原地 */
const FALLBACK_SLACK_MS = 120

/**
 * 找不到配置的动作名时的兜底时长（正常不该发生 —— 动作名拼错由 fit_check.py 报错）。
 * 有它只是为了「配置坏了也还能跑」，而不是把错误藏起来。
 */
const UNKNOWN_MOTION_MS = 800

/**
 * 动作名全部拼错时的兜底：约定里 `hold`（停顿）永远存在。 */
const MOTION_FALLBACK = 'hold'

/**
 * 统一动画调度器（DEV.md §4.4）。
 *
 * 设计要点：
 *  - 状态是「当前表情 + 播放到第几步」。表情绑着一列动作（`expressions[x].motions`），
 *    **依次播放**，每一拍都是那一列里的一项 —— 任何时刻只有一个变换在演，
 *    所以渲染层始终是单层 `.pet-motion`，不需要为「动作叠加」再套一层 DOM。
 *  - **没有冷却时间**：序列里的 `hold`（停顿）就承担了原来冷却的作用（DEV.md §4.4）。
 *    同一个表情重复触发一律忽略，否则「连着说两次开心」会让动画永远停在第一步。
 *  - 中途来了别的表情：**等当前这一拍演完再换**（排队）。只有两种情况下立刻打断：
 *    来者优先级**严格更高**，或正在演的是**循环**表情（循环没有「完整的一个动作」可言，
 *    随便切在哪都不算切坏）。这样既不会把一个弹跳切两半，说话开始时角色也不会慢半拍。
 *  - 排队意味着**最坏延迟 = 当前这一拍的剩余时长**，所以一次性表情的每一步别超过
 *    两秒半左右（见 animations.css 顶部）。排队的只会是优先级不高于当前的情绪，
 *    本来就不急。
 *  - 每一拍配一个**唯一**的兜底定时器，加上静音超时，同一时刻最多两个（见 pendingTimers）。
 *  - dispose() 清空全部定时器，程序退出后不遗留动画任务。
 */
export class AnimationScheduler {
  /** 当前表情，供视图选图 */
  readonly expression: Ref<ExpressionName>
  /** 当前这一拍的动作名，供视图绑定 class 与 CSS 变量 */
  readonly motion: Ref<MotionName>
  /** 当前是第几步（0 起）与这一列一共几步，调试窗口显示用 */
  readonly stepIndex: Ref<number>
  readonly stepCount: Ref<number>
  /** 当前是否处于「主播正在说话」状态 */
  readonly voiceActive: Ref<boolean>

  private readonly theme: ThemeDefinition
  /** 无事件时应当回到的表情：speaking（人声刚结束的宽限期）或 idle */
  private baseExpression: ExpressionName
  /** 当前播放到 `motions` 的第几项 */
  private index = 0
  /** 待切换的表情：当前这一拍演完就切过去。空槽用 null 表示 */
  private queued: ExpressionName | null = null
  /** 当前这一拍的兜底定时器，同一时刻最多一个 */
  private stepTimer: ReturnType<typeof setTimeout> | null = null
  /** 静音超时定时器 */
  private idleTimer: ReturnType<typeof setTimeout> | null = null
  /** 这一拍的起点，用来挡掉上一拍迟到的 animationend（见 finishStep） */
  private stepStartedAt = 0
  private disposed = false

  constructor(theme: ThemeDefinition) {
    this.theme = theme
    this.baseExpression = theme.baseState.silent
    const list = this.steps(theme.baseState.silent)
    this.expression = ref<ExpressionName>(theme.baseState.silent)
    this.motion = ref<MotionName>(list[0] ?? MOTION_FALLBACK)
    this.stepIndex = ref(0)
    this.stepCount = ref(list.length)
    this.voiceActive = ref(false)
  }

  /**
   * 诊断用：确认定时器没有泄漏。
   *
   * 用 ref 而不是 getter —— 调试窗口要实时看这个数（阶段 2 的验收项之一就是
   * 「反复触发不累积动画实例」），getter 读的是普通字段，没有响应性，
   * 数值变了也不会触发 watch，界面上会一直停在一个过期的数上。
   */
  readonly pendingTimers = ref(0)

  /** 每次增删定时器后同步一下计数，保证它和上面两个字段永远一致 */
  private syncTimerCount(): void {
    this.pendingTimers.value = (this.stepTimer === null ? 0 : 1) + (this.idleTimer === null ? 0 : 1)
  }

  /** 处理一个标准化事件 */
  dispatch(kind: PetEventKind): void {
    if (this.disposed) return

    this.noteActivity()

    switch (kind) {
      case 'voice_start':
      case 'neutral':
        // 说话是基准状态：切回基准，但基准变化本身不打断正在演的情绪序列，
        // 要走优先级判断（speaking 的优先级高于 idle、低于任何情绪）。
        this.voiceActive.value = true
        this.baseExpression = this.theme.baseState.voiceActive
        this.request(this.baseExpression)
        return

      case 'voice_end':
        // DEV.md §5.5：voice_end 不立刻回 idle，先等静音超时。
        this.voiceActive.value = false
        return

      case 'idle':
        this.voiceActive.value = false
        this.baseExpression = this.theme.baseState.silent
        this.request(this.baseExpression)
        return

      default: {
        const expression = this.theme.eventToExpression[kind]
        if (!expression) return
        this.request(expression)
      }
    }
  }

  /**
   * 由组件在 animationend 时回调。
   *
   * 参数是**这一拍的动作名**（组件把它渲染成的那个）：事件可能来自上一拍
   * （它已经演完、事件还在队列里，而我们已经切到下一拍了），名字对不上就丢掉。
   */
  handleAnimationEnd(motion: MotionName): void {
    if (this.disposed) return
    this.finishStep(motion)
  }

  dispose(): void {
    this.disposed = true
    this.clearStepTimer()
    this.clearIdleTimer()
  }

  // ------------------------------------------------------------- internals

  /** 某个表情的播放序列（缺配置就是空列表，下面各处都按空列表兜底） */
  private steps(expression: ExpressionName): readonly MotionName[] {
    return this.theme.expressions[expression]?.motions ?? []
  }

  private priorityOf(expression: ExpressionName): number {
    return this.theme.expressions[expression]?.priority ?? 0
  }

  private motionDuration(motion: MotionName): number {
    return this.theme.motions[motion]?.durationMs ?? UNKNOWN_MOTION_MS
  }

  /**
   * 请求演一个表情。
   *
   * 三种结果：已经在演（忽略）／当前可以立刻让位（打断）／否则（排队等这一拍收尾）。
   */
  private request(expression: ExpressionName): void {
    // 已经在演这个表情：什么都不做。
    // 不是「太近了先跳过」（那才是冷却），是「它正在演的那一列里就有停顿」——
    // 重新从第 0 步开始的话，连着来两次同一个情绪会让序列永远停在第一步。
    if (expression === this.expression.value) return

    // 两条立刻让位的理由：
    //  1. 优先级严格更高 —— 情绪比正在演的更急，哪怕把动作切两半也要换（原行为）；
    //  2. 正在演的是**循环**表情 —— 它本来就没有「一个完整的动作」可言（每一步都是
    //     从头循环的第 N 拍，随便切在哪都不算切坏），所以谁来都能立刻接上。
    //     这一条正是「开始说话时角色要马上跟上」：idle 是循环的，voice_start 立刻生效，
    //     不会让角色呆呆地摆完手里那一拍。
    const looping = this.theme.expressions[this.expression.value]?.loop ?? false
    if (looping || this.priorityOf(expression) > this.priorityOf(this.expression.value)) {
      this.enter(expression, 0)
      return
    }

    // 排队。槽里只留一个：已经排着更高优先级的就不被顶掉，否则后来者替换它
    // —— 同等紧急的情况下，用户刚说的那句更重要。
    if (this.queued !== null && this.priorityOf(this.queued) > this.priorityOf(expression)) return
    this.queued = expression
  }

  /** 进入某个表情的第 index 步 */
  private enter(expression: ExpressionName, index: number): void {
    this.clearStepTimer()

    const list = this.steps(expression)
    const motion = list[index] ?? list[0] ?? MOTION_FALLBACK
    this.expression.value = expression
    this.index = index
    this.motion.value = motion
    this.stepIndex.value = index
    this.stepCount.value = list.length
    this.stepStartedAt = Date.now()

    // 每一拍都配兜底定时器，多留一点余量。正常情况下 animationend 先到，
    // 但下面三种情况下它不会来，没有这个定时器状态机会卡死在原地：
    //   - 这一拍和上一拍的动作重名（DOM 没变，CSS 不会重启动画，也就没有 animationend）；
    //   - 系统开了「减少动态效果」，动作动画被整个关掉（见 animations.css）；
    //   - 窗口不可见时浏览器可能不推进动画。
    this.stepTimer = setTimeout(
      () => this.finishStep(motion),
      this.motionDuration(motion) + FALLBACK_SLACK_MS,
    )
    this.syncTimerCount()
  }

  /** 这一拍演完了（animationend 或兜底定时器），决定下一步演什么 */
  private finishStep(motion: MotionName): void {
    // 期间可能已经被更高优先级的事件切走了，或已经换到下一拍了
    if (this.motion.value !== motion) return

    // 挡掉上一拍迟到的 animationend：这一拍的动画至少要演到九成才算演完。
    // 名字相同的相邻两步（跨表情切换时会出现）全靠这条区分 ——
    // 没有它，迟到的事件会把刚开始的新一拍当场结束掉，序列会跳步。
    if (Date.now() - this.stepStartedAt < this.motionDuration(motion) * 0.9) return

    this.clearStepTimer()
    this.advance()
  }

  /** 走到下一步：优先切到排队中的表情，其次本列表的下一步，最后回基础表情 */
  private advance(): void {
    const queued = this.queued
    if (queued !== null) {
      this.queued = null
      this.enter(queued, 0)
      return
    }

    const list = this.steps(this.expression.value)
    const next = this.index + 1
    if (next < list.length) {
      this.enter(this.expression.value, next)
      return
    }

    if (this.theme.expressions[this.expression.value]?.loop) {
      this.enter(this.expression.value, 0)
      return
    }

    // 非循环序列演完了，回基础表情。这里**不再走优先级判断**：
    // 我们本来就站在步骤边界上，而且「没得演了」只能回基准，
    // 再排一次队会白等一拍（角色会以最后一帧的静止姿态多站一会儿）。
    this.queued = null
    this.enter(this.baseExpression, 0)
  }

  /** 每次收到事件都重置静音超时计时 */
  private noteActivity(): void {
    this.clearIdleTimer()
    this.idleTimer = setTimeout(() => {
      this.idleTimer = null
      this.syncTimerCount()
      if (this.disposed) return
      this.voiceActive.value = false
      this.baseExpression = this.theme.baseState.silent
      // 不打断正在演的情绪序列，让它自然收尾后再回待机。
      this.request(this.baseExpression)
    }, this.theme.scheduler.idleTimeoutMs)
    this.syncTimerCount()
  }

  private clearStepTimer(): void {
    if (this.stepTimer !== null) {
      clearTimeout(this.stepTimer)
      this.stepTimer = null
      this.syncTimerCount()
    }
  }

  private clearIdleTimer(): void {
    if (this.idleTimer !== null) {
      clearTimeout(this.idleTimer)
      this.idleTimer = null
      this.syncTimerCount()
    }
  }
}
