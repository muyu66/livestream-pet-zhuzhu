<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import ContextMenu from './components/ContextMenu.vue'
import PetCharacter from './components/PetCharacter.vue'
import { AnimationScheduler } from './anim/scheduler'
import {
  emitPetState,
  fetchMenuItems,
  listenAlwaysOnTop,
  listenPetEvents,
  listenPetStateRequest,
  listenShutdown,
  quitApp,
  toggleAlwaysOnTop,
  toggleDebugWindow,
} from './backend'
import { loadTheme } from './theme/registry'
import {
  MENU_QUIT,
  MENU_TOGGLE_TOPMOST,
  type MenuEntry,
  type PetEvent,
} from './types/events'
import type { PetState } from './types/pet-state'

const theme = loadTheme()
const scheduler = new AnimationScheduler(theme.definition)

const currentExpression = computed(() => scheduler.expression.value)
const currentMotion = computed(() => scheduler.motion.value)
const voiceActive = computed(() => scheduler.voiceActive.value)

/**
 * 角色框尺寸（theme.json 的 `stage`）。
 *
 * 窗口与它**等大**（DEV.md §3.1）：两个数对不上都不是小事，所以这里顺手校验一次。
 */
const stageVars = computed(() => ({
  '--pet-stage-width': `${theme.definition.stage.width}px`,
  '--pet-stage-height': `${theme.definition.stage.height}px`,
}))

/**
 * 校验窗口尺寸与角色框尺寸是否相等 —— 按 DEV.md §3.1，这两个数**必须相等**。
 *
 * 两个方向的偏差都要报，但原因完全不同：
 * - 窗口偏小：角色被窗口边整体裁掉，一眼能看出是坏的；
 * - 窗口偏大：多出一圈看不见的透明余量，角色贴不到屏幕边角，而且那圈余量仍然
 *   接收鼠标事件（压到任务栏上时那一块就点不动）。
 *
 * 相等时保持沉默 —— 那是正常状态，不是「没有余量」的警告。
 * 至于动画越界多少像素、有没有被切，用 `python -I tools/fit_check.py` 看，这里不算。
 */
function checkStageFitsWindow() {
  const { width, height } = theme.definition.stage
  const extraX = window.innerWidth - width
  const extraY = window.innerHeight - height

  if (extraX < 0 || extraY < 0) {
    console.error(
      `[pet] 窗口 ${window.innerWidth}×${window.innerHeight} 比角色框 ${width}×${height} 还小，` +
        '角色会被窗口边裁掉。请把 tauri.conf.json 的窗口尺寸改成与 theme.json 的 stage 相等。',
    )
    return
  }

  if (extraX > 0 || extraY > 0) {
    console.warn(
      `[pet] 窗口 ${window.innerWidth}×${window.innerHeight} 比角色框 ${width}×${height} 大 ` +
        `${extraX}×${extraY}px：多出来的透明余量看不见，但会接收鼠标事件（压住任务栏时那块就点不动），` +
        '角色也因此贴不到屏幕边角。两处尺寸应保持相等（DEV.md §3.1）。',
    )
  }
}

/**
 * 角色右键菜单。
 *
 * 菜单项不在这里硬编码，而是每次向后端要一份 `menu_items`：
 * 文案含状态（置顶时是「取消置顶」），自己维护一份副本迟早会跟托盘菜单对不上。
 * 见 DEV.md §3.3——两份菜单共享同一份定义。
 */
const menuVisible = ref(false)
const menuPosition = ref({ x: 0, y: 0 })
const menuItems = ref<MenuEntry[]>([])

async function refreshMenuItems() {
  try {
    menuItems.value = await fetchMenuItems()
  } catch (err) {
    console.error('[pet] 获取菜单项失败:', err)
  }
}

/**
 * 右键菜单挂在窗口根节点而不是角色上：窗口内任何位置右键都要有反应 ——
 * 包括 PNG 轮廓之外的透明像素，以及动画把角色挪开后露出的空白。
 * 挂在角色上的话，用户在这些地方右键会毫无反应，而他并不知道
 * 自己刚才点的是「窗口里但角色外」。
 */
function openMenu(event: MouseEvent) {
  event.preventDefault()
  menuPosition.value = { x: event.clientX, y: event.clientY }
  menuVisible.value = true
  // 菜单可能已经因为托盘切换而过期（本窗口置顶时开着菜单又被托盘切了状态），
  // 每次打开都重新取一次，代价是一次 IPC，比显示错文案划算
  void refreshMenuItems()
}

async function onMenuSelect(id: string) {
  if (id === MENU_QUIT) {
    await quitApp()
    return
  }
  if (id === MENU_TOGGLE_TOPMOST) {
    // 不用手动刷新菜单：后端切换后会推 pet://always-on-top，由那个监听统一处理。
    // 手动再刷一次会和事件抢着写 menuItems，反而容易看到旧文案闪一下。
    try {
      await toggleAlwaysOnTop()
    } catch (err) {
      console.error('[pet] 切换置顶失败:', err)
    }
    return
  }
  console.warn('[pet] 未知菜单项:', id)
}

// ---------------------------------------------------------------- 事件接入

const lastEvent = ref<PetEvent | null>(null)

function applyEvent(event: PetEvent) {
  lastEvent.value = event
  scheduler.dispatch(event.kind)
}

const unlisteners: Array<() => void> = []

onMounted(async () => {
  unlisteners.push(await listenPetEvents(applyEvent))
  unlisteners.push(
    await listenShutdown(() => {
      // 退出前先停掉前端动画任务，避免留下未清理的定时器
      scheduler.dispose()
    }),
  )
  // 从托盘切换置顶时也要刷新文案，否则角色菜单会显示过期的「置顶窗口」
  unlisteners.push(
    await listenAlwaysOnTop(() => {
      void refreshMenuItems()
    }),
  )
  unlisteners.push(await listenPetStateRequest(publishState))
  await refreshMenuItems()
  window.addEventListener('keydown', onKeydown)
  checkStageFitsWindow()
  // 调试窗口可能已经开着（前端热重载时就会这样），补发一份当前状态
  publishState()
})

onBeforeUnmount(() => {
  for (const off of unlisteners) off()
  window.removeEventListener('keydown', onKeydown)
  // 这里**不**动 setDebug：诊断推送的开关归调试窗口管（它自己挂载时开、
  // 卸载时关，窗口被强关时还有后端的 Destroyed 兜底）。角色窗口顺手关一下
  // 看着稳妥，实际会在热重载时把还开着的调试窗口的推送一起关掉 ——
  // 改一行 App.vue 就发现面板不再刷新，排查起来很费劲。
  scheduler.dispose()
})

// ------------------------------------------------------------ 调试窗口入口
//
// 按 D 打开/关闭**独立的调试窗口**（src-tauri 那边创建，页面是 debug.html）。
// 它让阶段 2 可以完全不依赖麦克风验收：手动触发任意表情与任意标准化事件，
// 观察序列推进、排队切换、优先级打断和返回行为；面板里还有一份动作库清单。
//
// 之所以不做成这个窗口里的浮层：角色窗口就是角色大小（160×160），浮层要么遮住角色、
// 要么小到看不全 —— 这两件事正好是本来的问题。

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'd' || event.key === 'D') {
    if (event.target instanceof HTMLInputElement) return
    void toggleDebugWindow()
  }
}

// ------------------------------------------------------ 状态广播（给调试窗口）
//
// 调度器的状态只存在于这个窗口里，调试窗口要靠广播才看得到。
// 只在状态真的变了时发一条 —— 电平那种高频数据走的是后端另一条通道。

const petState = computed<PetState>(() => ({
  expression: currentExpression.value,
  motion: currentMotion.value,
  stepIndex: scheduler.stepIndex.value,
  stepCount: scheduler.stepCount.value,
  voiceActive: voiceActive.value,
  pendingTimers: scheduler.pendingTimers.value,
  lastEvent: lastEvent.value,
}))

function publishState() {
  void emitPetState(petState.value)
}

watch(petState, publishState)
</script>

<template>
  <div class="pet-app" :style="stageVars" data-tauri-drag-region @contextmenu="openMenu">
    <PetCharacter
      :theme="theme"
      :expression="currentExpression"
      :motion="currentMotion"
      @animationend="scheduler.handleAnimationEnd($event)"
    />

    <ContextMenu
      :visible="menuVisible"
      :x="menuPosition.x"
      :y="menuPosition.y"
      :items="menuItems"
      @select="onMenuSelect"
      @close="menuVisible = false"
    />
  </div>
</template>

<style scoped>
/**
 * 角色居中于窗口。
 *
 * 窗口与角色框等大，所以居中之后两者是重合的（见 DEV.md §3.1）。
 * 仍然用 flex 居中而不是让角色框贴着左上角：动画的位移和缩放都是相对角色框的中心算的，
 * 角色框一旦偏移，某一侧就会先被窗口边裁掉。
 */
.pet-app {
  width: 100%;
  height: 100%;
  position: relative;
  display: flex;
  align-items: center;
  justify-content: center;
}
</style>
