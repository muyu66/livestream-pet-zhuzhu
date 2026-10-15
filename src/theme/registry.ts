import zhuzhuBase from '../../themes/zhuzhu/base.png'
import zhuzhuDefinition from '../../themes/zhuzhu/theme.json'
import type { ActionName, ThemeDefinition } from '../types/theme'

/**
 * 主题注册表。
 *
 * 第一版只有奇魔猪一个主题，但结构上按 DEV.md §4.5 预留多主题：
 * 新增主题 = 在这个表里加一项（theme.json + base.png），动画引擎不用改。
 *
 * 这里用静态 import 而不是运行时 fetch，是为了让 Vite 正确处理资源哈希；
 * 代价是主题在构建期固定，运行时切换主题留给后续版本。
 */
interface ThemeEntry {
  definition: ThemeDefinition
  /** 基础图已解析后的最终 URL */
  baseUrl: string
  /** 已登记的表情图；未登记的动作用 baseUrl 回退 */
  expressions: Partial<Record<ActionName, string>>
}

const REGISTRY: Record<string, ThemeEntry> = {
  zhuzhu: {
    definition: zhuzhuDefinition as unknown as ThemeDefinition,
    baseUrl: zhuzhuBase,
    // 第一版只有一张透明 PNG，没有额外表情图。
    // 未来加入 themes/zhuzhu/expressions/happy.png 后在这里登记即可。
    expressions: {},
  },
}

export const DEFAULT_THEME_ID = 'zhuzhu'

export interface LoadedTheme {
  id: string
  definition: ThemeDefinition
  /**
   * 取某个动作应当显示的图片。
   * 缺少对应表情图时回退到基础图 —— 这是 DEV.md §4.5 的硬性要求。
   */
  resolveAsset(action: ActionName): string
}

export function loadTheme(id: string = DEFAULT_THEME_ID): LoadedTheme {
  const entry = REGISTRY[id] ?? REGISTRY[DEFAULT_THEME_ID]
  if (!entry) {
    throw new Error(`找不到主题 "${id}"，且默认主题 "${DEFAULT_THEME_ID}" 未注册`)
  }

  return {
    id: REGISTRY[id] ? id : DEFAULT_THEME_ID,
    definition: entry.definition,
    resolveAsset(action: ActionName): string {
      // 表情图缺失时回退到 base.png：单张 PNG 的第一版，所有动作本来就是同一张图。
      return entry.expressions[action] ?? entry.baseUrl
    },
  }
}
