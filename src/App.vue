<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import ContextMenu from './components/ContextMenu.vue'
import PetCharacter from './components/PetCharacter.vue'
import { AnimationScheduler } from './anim/scheduler'
import {
  fetchMenuItems,
  listenAlwaysOnTop,
  listenDebug,
  listenPetEvents,
  listenShutdown,
  quitApp,
  setDebug,
  toggleAlwaysOnTop,
} from './backend'
import { loadTheme } from './theme/registry'
import {
  MENU_QUIT,
  MENU_TOGGLE_TOPMOST,
  PET_EVENT_KINDS,
  type DebugLevel,
  type DebugTranscript,
  type MenuEntry,
  type PetEvent,
  type PetEventKind,
} from './types/events'
import { ACTION_NAMES, type ActionName } from './types/theme'

const theme = loadTheme()
const scheduler = new AnimationScheduler(theme.definition)

const currentAction = computed(() => scheduler.current.value)
const voiceActive = computed(() => scheduler.voiceActive.value)

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

function openMenu(payload: { x: number; y: number }) {
  menuPosition.value = payload
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
  unlisteners.push(await listenDebug(onDebug))
  await refreshMenuItems()
  window.addEventListener('keydown', onKeydown)
})

onBeforeUnmount(() => {
  for (const off of unlisteners) off()
  window.removeEventListener('keydown', onKeydown)
  // 别把后端的推送开关留在打开状态
  void setDebug(false)
  scheduler.dispose()
})

// ------------------------------------------------------------ 调试面板(验收用)
//
// 按 D 打开。它让阶段 2 可以完全不依赖麦克风验收：
// 手动触发任意动作与任意标准化事件，观察调度、优先级、冷却和返回行为。

const debugVisible = ref(false)

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'd' || event.key === 'D') {
    if (event.target instanceof HTMLInputElement) return
    debugVisible.value = !debugVisible.value
  }
}

// -------------------------------------------------------- 实时诊断（语音链路的现场）
//
// 动画调试只能告诉你「表现层收到什么」，看不到「模型到底听到了什么」。
// 这几项补上后半段：原始识别文本、语言、模型给的原始情绪/事件 token、
// 推理耗时，以及输入电平 —— 全是数字静音时，识别不出来就是必然的，
// 光看动画是发现不了的。

const level = ref<DebugLevel | null>(null)
/** 最近若干条识别结果，新的在前 */
const transcripts = ref<DebugTranscript[]>([])
const MAX_TRANSCRIPTS = 8

function onDebug(info: DebugTranscript | DebugLevel) {
  if (info.type === 'level') {
    level.value = info
    return
  }
  // 语言/情绪/事件都可能是空串（模型未返回），显示成 ∅ 而不是留白，
  // 否则分不清「没识别出来」和「这一栏根本不显示」
  transcripts.value = [info, ...transcripts.value].slice(0, MAX_TRANSCRIPTS)
}

/** 面板开着才让后端推数据；关掉就停，正常运行不背这份开销 */
watch(debugVisible, (visible) => {
  void setDebug(visible)
})

/** 线性幅度 → dBFS。-60 以下按静音显示，避免 -inf 刷屏 */
function dbfs(amplitude: number): string {
  if (amplitude <= 0) return '-∞'
  return (20 * Math.log10(amplitude)).toFixed(1)
}

/** dBFS 映射到 0–100 的条形宽度，-60 dBFS 以下算空 */
function levelPercent(amplitude: number): number {
  if (amplitude <= 0) return 0
  const db = 20 * Math.log10(amplitude)
  return Math.max(0, Math.min(100, ((db + 60) / 60) * 100))
}

function triggerAction(action: ActionName) {
  // 直接走事件通道，保证调试路径和真实路径是同一套逻辑
  const kindByAction: Record<ActionName, PetEventKind> = {
    idle: 'idle',
    speaking: 'voice_start',
    happy: 'happy',
    angry: 'angry',
    sad: 'sad',
    surprised: 'surprised',
    laugh: 'laugh',
  }
  applyEvent({ kind: kindByAction[action], ts: Date.now() })
}

function emitKind(kind: PetEventKind) {
  applyEvent({ kind, ts: Date.now() })
}
</script>

<template>
  <div class="pet-app">
    <PetCharacter
      :theme="theme"
      :action="currentAction"
      @animationend="scheduler.handleAnimationEnd($event)"
      @contextmenu="openMenu"
    />

    <ContextMenu
      :visible="menuVisible"
      :x="menuPosition.x"
      :y="menuPosition.y"
      :items="menuItems"
      @select="onMenuSelect"
      @close="menuVisible = false"
    />

    <section v-if="debugVisible" class="pet-debug">
      <header class="pet-debug__header">
        <strong>动画调试</strong>
        <span class="pet-debug__meta">
          当前: <b>{{ currentAction }}</b>
          · 人声: {{ voiceActive ? '有' : '无' }}
          · 定时器: {{ scheduler.pendingTimers }}
        </span>
      </header>

      <p class="pet-debug__label">动作</p>
      <div class="pet-debug__row">
        <button v-for="a in ACTION_NAMES" :key="a" type="button" @click="triggerAction(a)">
          {{ a }}
        </button>
      </div>

      <p class="pet-debug__label">标准化事件</p>
      <div class="pet-debug__row">
        <button v-for="k in PET_EVENT_KINDS" :key="k" type="button" @click="emitKind(k)">
          {{ k }}
        </button>
      </div>

      <p class="pet-debug__last">
        最近事件: <code>{{ lastEvent ? `${lastEvent.kind} @ ${lastEvent.ts}` : '无' }}</code>
      </p>

      <hr class="pet-debug__sep" />

      <p class="pet-debug__meta">
        输入电平: <b>{{ level ? dbfs(level.peak) : '—' }}</b> dBFS
        · 有效值 {{ level ? dbfs(level.rms) : '—' }} dBFS
        · 丢弃 {{ level?.dropped ?? 0 }} 块
        · 驱动错误 {{ level?.capture_errors ?? 0 }} 次
      </p>
      <div class="pet-debug__meter">
        <div class="pet-debug__meter-fill" :style="{ width: `${levelPercent(level?.peak ?? 0)}%` }" />
      </div>

      <p class="pet-debug__label">识别（模型原始输出，新→旧）</p>
      <p v-if="transcripts.length === 0" class="pet-debug__meta">
        暂无。对着麦克风说句话；电平一直是 -∞ 说明设备没送音频上来。
      </p>
      <ul v-else class="pet-debug__log">
        <li v-for="(t, i) in transcripts" :key="`${t.audio_secs}-${i}`">
          <span :class="t.segment ? 'pet-debug__seg' : 'pet-debug__interim'">
            {{ t.segment ? '段' : '中' }}
          </span>
          {{ t.audio_secs.toFixed(2) }}s · {{ t.elapsed_ms }}ms ·
          {{ t.lang || '∅' }} · {{ t.emotion || '∅' }} · {{ t.event || '∅' }} ·
          <code>{{ t.text || '（空）' }}</code>
        </li>
      </ul>
    </section>
  </div>
</template>

<style scoped>
.pet-app {
  width: 100%;
  height: 100%;
  position: relative;
}

.pet-debug {
  position: fixed;
  left: 6px;
  bottom: 6px;
  right: 6px;
  /* 加进诊断区之后内容多了不少，窗口本身只有 320×320，
     留太少高度就得一直滚，反而看不见电平的实时变化 */
  max-height: 72%;
  overflow: auto;
  padding: 8px 10px;
  border-radius: 8px;
  background: rgba(18, 18, 22, 0.9);
  border: 1px solid rgba(255, 255, 255, 0.14);
  color: #e8e8ee;
  font-family: Consolas, 'Courier New', monospace;
  font-size: 11px;
  line-height: 1.5;
}

.pet-debug__header {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin-bottom: 6px;
}

.pet-debug__meta {
  color: #9fa3b0;
}

.pet-debug__label {
  margin: 6px 0 3px;
  color: #9fa3b0;
}

.pet-debug__row {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.pet-debug__row button {
  padding: 2px 7px;
  border: 1px solid rgba(255, 255, 255, 0.18);
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.08);
  color: inherit;
  font: inherit;
  cursor: pointer;
}

.pet-debug__row button:hover {
  background: rgba(255, 255, 255, 0.18);
}

.pet-debug__last {
  margin: 6px 0 0;
  color: #9fa3b0;
  word-break: break-all;
}

.pet-debug__sep {
  margin: 8px 0 6px;
  border: 0;
  border-top: 1px solid rgba(255, 255, 255, 0.14);
}

.pet-debug__meter {
  height: 5px;
  margin: 4px 0 8px;
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.1);
  overflow: hidden;
}

.pet-debug__meter-fill {
  height: 100%;
  background: linear-gradient(90deg, #4ade80, #facc15 70%, #f87171);
  transition: width 0.08s linear;
}

.pet-debug__log {
  margin: 0;
  padding: 0;
  list-style: none;
}

.pet-debug__log li {
  padding: 1px 0;
  color: #c9ccd6;
  word-break: break-all;
}

.pet-debug__seg,
.pet-debug__interim {
  display: inline-block;
  width: 1.1em;
  margin-right: 4px;
  border-radius: 3px;
  text-align: center;
  color: #12121a;
}

/* 语音段是权威解码，中间解码只是过程中的近似，用颜色区分强调程度 */
.pet-debug__seg {
  background: #7dd3fc;
}

.pet-debug__interim {
  background: #6b7280;
  color: #e8e8ee;
}
</style>
