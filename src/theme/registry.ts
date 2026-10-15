import type { ExpressionName, ThemeDefinition } from '../types/theme'

/**
 * 主题注册表。
 *
 * 按 DEV.md §4.5 的结构组织：一个主题 = `themes/<id>/` 下的 `theme.json` 加若干素材，
 * 动画引擎不关心具体有几张图。新增主题只要建目录，不在这里改代码。
 *
 * 素材全部用 `import.meta.glob` 在构建期展开。这样 `theme.json` 里登记的图是
 * **真的在生效**的配置，而不是写着好看：加一张表情图 = 把 PNG 放进目录 +
 * 在 theme.json 的对应表情下写一行 `image`。
 *
 * 这里用静态 glob 而不是运行时 fetch，是为了让 Vite 正确处理资源哈希；
 * 代价是主题在构建期固定，运行时切换主题留给后续版本。
 */
interface ThemeEntry {
  definition: ThemeDefinition
  /** 基础图已解析后的最终 URL */
  baseUrl: string
  /** 已登记的表情图；未登记或文件缺失的用 baseUrl 回退 */
  images: Partial<Record<ExpressionName, string>>
}

/** `../../themes/zhuzhu/theme.json` -> 已解析的 JSON */
const DEFINITIONS = import.meta.glob('../../themes/*/theme.json', {
  eager: true,
  import: 'default',
}) as Record<string, ThemeDefinition>

/** `../../themes/zhuzhu/base.png` -> 带哈希的 URL */
const ASSETS = import.meta.glob('../../themes/**/*.png', {
  eager: true,
  query: '?url',
  import: 'default',
}) as Record<string, string>

const THEME_ROOT = '../../themes/'

function resolveAssetUrl(dir: string, file: string): string | undefined {
  return ASSETS[THEME_ROOT + dir + '/' + file]
}

/**
 * `_说明` 这类「注释键」：JSON 没有注释，配置文件里一直用 `_` 开头的键来写说明。
 * 遍历配置时必须跳过它们，否则 `_说明` 会被当成一个动作 / 一个表情。
 */
function isDocKey(key: string): boolean {
  return key.startsWith('_')
}

/**
 * 校验主题配置。**在启动时就喊出来，不要等到角色不动了再排查。**
 *
 * 为什么必须有这一层：`import.meta.glob` 把 JSON 直接断言成 `ThemeDefinition`，
 * 没有任何运行时校验。而一个动作名拼错（`motions` 里没有、CSS 里也没有同名关键帧）
 * 的后果是 `.pet-motion.is-<拼错的>` —— `animation-name` 解析不到，**动画不会启动**，
 * 于是 `animationend` 永远不来，调度器只能靠兜底定时器一拍一拍地空转：
 * 角色完全静止、控制台干干净净，看起来像「程序坏了」而不是「配置写错了」。
 *
 * 重的不放行（抛错），轻的只警告 —— 两者的差别是「能不能忍」：
 *  - 抛出：序列里的动作名不存在 / 序列为空 / durationMs 不是正数 / 基准表情不存在。
 *    这些会让角色不动或者乱跳，属于必须修好才能跑。
 *  - 警告：相邻两步同名（含循环列表收尾相接的那一对）。只影响观感 ——
 *    那一步的动画不会重播，角色多停一拍，其余照常。
 *
 * 注意作者视角的同类检查在 `tools/fit_check.py` 里也有一份，那是**写配置时**的闸门；
 * 这一份是**跑起来之后**的兜底，两者针对的是同一个错，位置不同不是重复。
 */
function validateTheme(dir: string, definition: ThemeDefinition): void {
  const where = `主题 "${dir}"`;

  for (const [name, motion] of Object.entries(definition.motions ?? {})) {
    if (isDocKey(name)) continue
    const duration = motion?.durationMs
    if (typeof duration !== 'number' || !Number.isFinite(duration) || duration <= 0) {
      throw new Error(`${where} 的动作 "${name}" 的 durationMs 不是正数（${String(duration)}）`)
    }
  }

  const motions = Object.keys(definition.motions ?? {}).filter((k) => !isDocKey(k))
  const known = new Set(motions)

  // 朝向开关（主题级默认值 + 每个表情的覆盖）必须是真正的布尔值：
  // 写成字符串 `"false"` 在 JS 里是 truthy，角色会**悄悄反着站**，而配置看起来「写的是 false」。
  // 这种错靠人眼发现不了，所以在启动时就抛出来。
  const mirrorFields: Array<[string, unknown]> = [['mirror', definition.mirror]]
  for (const [name, expression] of Object.entries(definition.expressions ?? {})) {
    if (isDocKey(name)) continue
    mirrorFields.push([`表情 "${name}" 的 mirror`, expression?.mirror])
  }
  for (const [field, value] of mirrorFields) {
    if (value === undefined) continue
    if (typeof value !== 'boolean') {
      throw new Error(
        `${where} 的 ${field} 必须是 true / false，收到 ${JSON.stringify(value)}` +
          '（字符串 "false" 也会被当成 true，角色朝向会反掉）',
      )
    }
  }

  for (const [name, expression] of Object.entries(definition.expressions ?? {})) {
    if (isDocKey(name)) continue
    const list = expression?.motions ?? []
    if (list.length === 0) {
      throw new Error(`${where} 的表情 "${name}" 一个动作都没有（motions 是空的）`)
    }
    for (const motion of list) {
      if (!known.has(motion)) {
        throw new Error(
          `${where} 的表情 "${name}" 用到了动作 "${motion}"，但 motions 里没有它。` +
            `可用的动作：${motions.join(' / ')}`,
        )
      }
    }
    list.forEach((motion, i) => {
      const previous = i === 0 ? (expression.loop ? list[list.length - 1] : undefined) : list[i - 1]
      if (previous === motion) {
        console.warn(
          `${where} 的表情 "${name}" 第 ${i} 步的动作 "${motion}" 与相邻一步同名：` +
            '类名没变，CSS 动画不会重播，那一步看起来是「停着不动」。' +
            '中间插一个 hold，或者换成另一个动作。',
        )
      }
    })
  }

  const expressions = Object.keys(definition.expressions ?? {}).filter((k) => !isDocKey(k))
  const wanted: Array<[string, string | undefined]> = [
    ...Object.entries(definition.baseState ?? {}),
    ...Object.entries(definition.eventToExpression ?? {}),
  ]
  for (const [field, target] of wanted) {
    if (target === undefined) continue
    if (!expressions.includes(target)) {
      throw new Error(
        `${where} 的 ${field} 指向表情 "${target}"，但 expressions 里没有它。` +
          `可用的表情：${expressions.join(' / ')}`,
      )
    }
  }
}

function buildEntry(dir: string, definition: ThemeDefinition): ThemeEntry {
  validateTheme(dir, definition)

  const baseUrl = resolveAssetUrl(dir, definition.assets.base)
  if (!baseUrl) {
    throw new Error(`主题 "${dir}" 的基础图 ${definition.assets.base} 不在 themes/${dir}/ 下`)
  }

  const images: Partial<Record<ExpressionName, string>> = {}
  for (const [expression, config] of Object.entries(definition.expressions)) {
    const file = config?.image
    // 没写 image 是合法的：这个表情就用基础图（比如想让 idle 用回线条更锐利的 base.png）。
    // 这里不该报错，静默回退即可 —— 它和「写了但文件不存在」是两回事。
    if (!file) continue
    const url = resolveAssetUrl(dir, file)
    if (!url) {
      // 少一张表情图不该让应用起不来：报一声，该表情回退到基础图。
      console.warn(`主题 "${dir}" 登记了 ${expression} -> ${file}，但文件不存在，该表情回退到基础图`)
      continue
    }
    images[expression as ExpressionName] = url
  }
  return { definition, baseUrl, images }
}

const REGISTRY: Record<string, ThemeEntry> = {}
for (const [path, definition] of Object.entries(DEFINITIONS)) {
  const dir = path.slice(THEME_ROOT.length).replace(/\/theme\.json$/, '')
  if (dir !== definition.id) {
    console.warn(`主题目录名 "${dir}" 与 theme.json 的 id "${definition.id}" 不一致，按目录名注册`)
  }
  REGISTRY[dir] = buildEntry(dir, definition)
}

export const DEFAULT_THEME_ID = 'zhuzhu'

export interface LoadedTheme {
  id: string
  definition: ThemeDefinition
  /**
   * 取某个表情应当显示的图片。
   * 缺少对应表情图时回退到基础图 —— 这是 DEV.md §4.5 的硬性要求。
   *
   * 参数是**表情**而不是动作：图归表情、运动归动作，两者是分开的（DEV.md §4.3）。
   */
  resolveImage(expression: ExpressionName): string
}

export function loadTheme(id: string = DEFAULT_THEME_ID): LoadedTheme {
  const entry = REGISTRY[id] ?? REGISTRY[DEFAULT_THEME_ID]
  if (!entry) {
    throw new Error(`找不到主题 "${id}"，且默认主题 "${DEFAULT_THEME_ID}" 未注册`)
  }

  return {
    id: REGISTRY[id] ? id : DEFAULT_THEME_ID,
    definition: entry.definition,
    resolveImage(expression: ExpressionName): string {
      // 表情图缺失时回退到 base.png。
      return entry.images[expression] ?? entry.baseUrl
    },
  }
}
