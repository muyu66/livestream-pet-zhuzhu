import {
  PET_ALWAYS_ON_TOP_CHANNEL,
  PET_DEBUG_CHANNEL,
  PET_EVENT_CHANNEL,
  PET_SHUTDOWN_CHANNEL,
  isMenuEntry,
  isPetDebug,
  isPetEventKind,
} from './types/events'
import type { MenuEntry, PetDebug, PetEvent, PetEventKind } from './types/events'
import {
  PET_STATE_CHANNEL,
  PET_STATE_REQUEST_CHANNEL,
  isPetState,
} from './types/pet-state'
import type { PetState } from './types/pet-state'

/**
 * 与 Rust 后端的薄封装。
 *
 * 之所以单独一层：`npm run dev` 在纯浏览器里跑时没有 Tauri 运行时，
 * 直接调用会抛错。这里统一做能力探测，让前端动画可以脱离 Rust 单独验证
 * （阶段 2 的验收就需要这个能力）。
 */
export const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

type Unlisten = () => void

/** 订阅标准化角色事件；返回取消订阅函数 */
export async function listenPetEvents(handler: (event: PetEvent) => void): Promise<Unlisten> {
  if (!isTauri) return () => {}

  const { listen } = await import('@tauri-apps/api/event')
  return listen<unknown>(PET_EVENT_CHANNEL, (incoming) => {
    const payload = incoming.payload
    if (payload && typeof payload === 'object' && 'kind' in payload) {
      const kind = (payload as { kind: unknown }).kind
      if (isPetEventKind(kind)) {
        handler({ kind, ts: (payload as { ts?: number }).ts ?? Date.now() })
      }
    }
  })
}

/** 接收后端退出前的清理通知，让前端先停掉全部动画任务 */
export async function listenShutdown(handler: () => void): Promise<Unlisten> {
  if (!isTauri) return () => {}
  const { listen } = await import('@tauri-apps/api/event')
  return listen(PET_SHUTDOWN_CHANNEL, () => handler())
}

/**
 * 请求退出应用。
 * 放在 Rust 侧统一处理：停止麦克风采集 → 结束音频线程 → 关闭应用。
 */
export async function quitApp(): Promise<void> {
  if (!isTauri) {
    console.warn('[pet] 非 Tauri 环境，退出请求被忽略')
    return
  }
  const { invoke } = await import('@tauri-apps/api/core')
  await invoke('exit_app')
}

/**
 * 取当前菜单项。菜单文案含状态（置顶 / 取消置顶），所以每次都要重新问后端，
 * 不能在浏览器里写死一份。纯浏览器环境下退化成静态列表，方便单独验动画。
 */
export async function fetchMenuItems(): Promise<MenuEntry[]> {
  if (!isTauri) return BROWSER_MENU_ITEMS

  const { invoke } = await import('@tauri-apps/api/core')
  const raw = await invoke<unknown>('menu_items')
  if (!Array.isArray(raw)) return BROWSER_MENU_ITEMS

  const items = raw.filter(isMenuEntry)
  // 后端返回了东西但一条都不合法，说明契约变了；宁可报出来也不要静默显示空菜单
  if (items.length === 0) {
    console.error('[pet] 后端返回的菜单项无法解析:', raw)
    return BROWSER_MENU_ITEMS
  }
  return items
}

/** 切换置顶，返回切换后的状态 */
export async function toggleAlwaysOnTop(): Promise<boolean | null> {
  if (!isTauri) {
    console.warn('[pet] 非 Tauri 环境，置顶请求被忽略')
    return null
  }
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<boolean>('toggle_always_on_top')
}

/** 订阅置顶状态变化（可能由托盘菜单触发） */
export async function listenAlwaysOnTop(
  handler: (value: boolean) => void,
): Promise<Unlisten> {
  if (!isTauri) return () => {}
  const { listen } = await import('@tauri-apps/api/event')
  return listen<unknown>(PET_ALWAYS_ON_TOP_CHANNEL, (incoming) => {
    if (typeof incoming.payload === 'boolean') handler(incoming.payload)
  })
}

/**
 * 订阅诊断信息。只有后端那边的开关打开时才会有数据 ——
 * 正常运行时这条通道是静的，不会白走 IPC。
 */
export async function listenDebug(handler: (info: PetDebug) => void): Promise<Unlisten> {
  if (!isTauri) return () => {}
  const { listen } = await import('@tauri-apps/api/event')
  return listen<unknown>(PET_DEBUG_CHANNEL, (incoming) => {
    if (isPetDebug(incoming.payload)) handler(incoming.payload)
  })
}

/** 开关后端的诊断信息推送，返回后端确认的状态 */
export async function setDebug(enabled: boolean): Promise<boolean | null> {
  if (!isTauri) return null
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<boolean>('set_debug', { enabled })
}

/**
 * 开关调试窗口，返回窗口此后的状态（true = 现在开着）。
 *
 * 调试界面是**独立的 Tauri 窗口**而不是角色窗口里的浮层：
 * 角色窗口只有角色框那么大，浮层要么遮住角色、要么小到看不全，
 * 而独立窗口可以随便拉大，且不会跟角色抢那点空间。
 */
export async function toggleDebugWindow(): Promise<boolean | null> {
  if (!isTauri) {
    // 纯浏览器里没有第二个窗口可开，说清楚而不是静默失败
    console.warn('[pet] 非 Tauri 环境，调试窗口不可用（请用 npm run tauri dev）')
    return null
  }
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<boolean>('toggle_debug_window')
}

/** 广播角色窗口自己的动画状态快照，给调试窗口看 */
export async function emitPetState(state: PetState): Promise<void> {
  if (!isTauri) return
  const { emit } = await import('@tauri-apps/api/event')
  await emit(PET_STATE_CHANNEL, state)
}

/** 订阅角色窗口的状态快照（调试窗口用） */
export async function listenPetState(handler: (state: PetState) => void): Promise<Unlisten> {
  if (!isTauri) return () => {}
  const { listen } = await import('@tauri-apps/api/event')
  return listen<unknown>(PET_STATE_CHANNEL, (incoming) => {
    if (isPetState(incoming.payload)) handler(incoming.payload)
  })
}

/** 请角色窗口补发一份当前状态（调试窗口刚打开时用，见 pet-state.ts） */
export async function requestPetState(): Promise<void> {
  if (!isTauri) return
  const { emit } = await import('@tauri-apps/api/event')
  await emit(PET_STATE_REQUEST_CHANNEL)
}

/** 角色窗口侧：响应补发请求 */
export async function listenPetStateRequest(handler: () => void): Promise<Unlisten> {
  if (!isTauri) return () => {}
  const { listen } = await import('@tauri-apps/api/event')
  return listen(PET_STATE_REQUEST_CHANNEL, () => handler())
}

/**
 * 触发一个标准化事件（调试窗口的按钮走这里）。
 *
 * 走的是和真实链路**同一条** `pet://event` 通道：调试窗口直接广播，
 * 角色窗口已经在监听它，所以不需要为此新增任何后端命令，
 * 也不可能出现「调试能过、真实链路不行」这种只在一条路上成立的假通过。
 */
export async function emitPetEvent(kind: PetEventKind): Promise<void> {
  if (!isTauri) {
    console.warn('[pet] 非 Tauri 环境，事件无法送达角色窗口')
    return
  }
  const { emit } = await import('@tauri-apps/api/event')
  await emit(PET_EVENT_CHANNEL, { kind, ts: Date.now() } satisfies PetEvent)
}

/** 纯浏览器模式下的菜单，仅用于脱离 Rust 验证动画时也能右键 */
const BROWSER_MENU_ITEMS: MenuEntry[] = [
  { id: 'toggle-topmost', label: '置顶窗口' },
  { id: 'quit', label: '退出' },
]
