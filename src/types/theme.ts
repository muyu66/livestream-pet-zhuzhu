import type { PetEventKind } from './events'

/**
 * 表情名。7 个预置表情，对应 DEV.md §4.3 的表格。
 *
 * 「表情」= 一张图（`expressions[x].image`）+ 一列动作（`expressions[x].motions`）。
 * 它与动作是**两套独立的名字**：表情 `happy` 用的是动作 `hop` / `hold`，
 * 想给两个表情用同一段动画，直接在各自的 `motions` 里写同一个动作名即可（DEV.md §4.3）。
 */
export type ExpressionName = 'idle' | 'speaking' | 'happy' | 'angry' | 'sad' | 'surprised' | 'laugh'

export const EXPRESSION_NAMES: readonly ExpressionName[] = [
  'idle',
  'speaking',
  'happy',
  'angry',
  'sad',
  'surprised',
  'laugh',
]

/**
 * 动作名（动作库里的键，见 theme.json 的 `motions`）。
 *
 * 刻意**不收窄成联合类型**：动作库是 theme.json 里手写的数据，加一个动作是「写段
 * keyframes + 在 theme.json 里登记一行」，不该还要改 TS 才能编译。
 * 代价是拼错一个动作名 TS 不会报错 —— 这件事交给 `tools/fit_check.py`：
 * 它会检查每个表情序列里的动作名在 `motions` 里存在、且 CSS 里有同名关键帧。
 */
export type MotionName = string

/**
 * 动作的运动参数，值直接作为 `--pet-*` CSS 自定义属性注入。
 * 全部来自 theme.json，组件里不硬编码任何数值。
 *
 * 加一个旋钮 = 这里加一行 + `PetCharacter.vue` 的映射表加一行 + 关键帧里开始用它。
 */
export interface MotionParams {
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
  /** 放大倍数，如 "1.12" */
  pop?: string
  /** 压缩比例，如 "0.94"（小于 1） */
  squash?: string
  /** 绕 Y 轴翻转的角度，如 "-180deg"（flip 用；±180 才是完整的翻到另一面） */
  spin?: string
  /** 透视距离，如 "600px"（flip 用）。越小立体感越强、变形也越夸张 */
  persp?: string
}

/**
 * 动作库里的一个动作 —— **一段只播一遍的动画**。
 *
 * 循环不在这里：一个动作永远只演一次（CSS 里一律 `1 both`），
 * 要不要循环由**表情**的 `loop` 决定。这样避免了「CSS 手写 infinite、theme.json 又写 loop」
 * 两处真相（DEV.md §4.3）。
 */
export interface MotionConfig {
  /** 中文说明：光看英文名看不出是什么动作，DEV.md §4.3 的表也是从这份说明生成的 */
  说明?: string
  /** 时长，**唯一**的时长来源；`--pet-duration` 由它推导 */
  durationMs: number
  params: MotionParams
}

/**
 * 一个表情：一张图 + 一列按时间顺序播放的动作。
 *
 * 序列是**依次播放**，不是叠加 —— 任何时刻只有一个变换在演，
 * 所以渲染层始终是单层 `.pet-motion`（DEV.md §4.4）。
 */
export interface ExpressionConfig {
  说明?: string
  /**
   * 表情图文件名，相对主题目录。缺这一项、或文件不存在时回退到 `assets.base`
   * —— 这是 DEV.md §4.5 的硬性要求。
   */
  image?: string
  /** 数字越大越优先；只有**严格更高**才能打断正在播的序列 */
  priority: number
  /** true = 整列无限循环（只有 idle / speaking 是）；false = 播一遍就回基础表情 */
  loop: boolean
  /**
   * 播放序列，按时间顺序。
   *
   * **相邻两项不能是同一个动作**：播放层靠 `is-<动作>` 这个类换动画，
   * 前后两步同名时 DOM 没有任何变化，CSS 动画不会重播 —— 那一步就只能靠兜底定时器
   * 干等过去。所以「连弹两下」要写成 `["bounce","hold","bounce"]`。
   * `tools/fit_check.py` 会把相邻同名报成错误。
   */
  motions: MotionName[]
  /**
   * 朝向：true = 这个表情左右镜像（贴屏幕右边时让它面朝里）。
   *
   * 镜像作用在**图上**（`--pet-mirror`），与动作、呼吸互不干扰，
   * 所以它不会让动作变形，也不会被任何一步的动画覆盖掉。
   * 不写就跟随主题级的 `mirror`（默认 false）。
   */
  mirror?: boolean
}

export interface ThemeDefinition {
  id: string
  name: string
  version: string
  assets: {
    /** 基础图：所有表情图缺失时的兜底 */
    base: string
  }
  /**
   * 角色框：PNG 用 `object-fit: contain` 塞进这个方框渲染，动画也在这个框的
   * 坐标系里算 —— 所以它决定的是**角色看起来多大**，不是窗口多大。
   *
   * 窗口尺寸（`src-tauri/tauri.conf.json`）必须与这个方框**相等**（DEV.md §3.1）：
   * 窗口偏小会把角色整体裁掉，偏大则多出一圈看不见却会吃掉鼠标事件的透明余量。
   * 动画越界的部分会被窗口边切掉，这是有意接受的代价。
   * `tools/fit_check.py` 会把两个文件都读进来，列出每个动作被切掉多少像素。
   */
  stage: {
    width: number
    height: number
  }
  scheduler: {
    idleTimeoutMs: number
  }
  /**
   * 主题级的默认朝向：true = 所有表情都左右镜像（单个表情可以用 `expressions[x].mirror`
   * 覆盖）。角色该朝哪边通常取决于它被拖到屏幕的哪一侧，所以这是一个「设定」而不是
   * 某个表情的属性 —— 放在主题层，逐个表情再微调。
   */
  mirror?: boolean
  baseState: {
    voiceActive: ExpressionName
    silent: ExpressionName
  }
  /** 标准化事件 → 表情的映射。换模型或换主题时只改这里（DEV.md §5.5） */
  eventToExpression: Partial<Record<PetEventKind, ExpressionName>>
  /** 动作库：可复用的动画片段，与表情相互独立 */
  motions: Record<MotionName, MotionConfig>
  expressions: Record<ExpressionName, ExpressionConfig>
}
