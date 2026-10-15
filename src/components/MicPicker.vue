<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  chooseAudioDevice,
  closeMicPicker,
  fetchAudioState,
  listInputDevices,
  listenAudioState,
  listenLevel,
  setMeter,
} from '../backend'
import { levelPercent } from '../audio/level'
import type { AudioChoice, AudioDevice, DeviceChoice } from '../types/audio'
import type { DebugLevel } from '../types/events'

/**
 * 麦克风选择框（独立 Tauri 窗口，页面是 mic-picker.html）。
 *
 * **点一行就立刻生效**：把选择写进用户配置，并把后端的采集线程换成那只设备。
 * 没有「确定 / 取消」这一步，也没有「试听」这一路 —— 选中的那只本就在这里
 * 显示实时电平，点一下就知道它到底有没有声音。代价是模型要重新加载，
 * 实测一秒出头，比多一套试听链路的复杂度划算得多。
 *
 * 窗口**选完不关**：用户正要看着电平确认这只麦有声音，关掉就看不到了。
 * 只有「关闭」按钮和 Esc 才关。
 */

const devices = ref<AudioDevice[]>([])
const audio = ref<AudioChoice | null>(null)
/** 正在执行切换命令（含停掉旧线程，最长三秒）—— 这期间列表置灰，防止连点 */
const busy = ref(false)
const level = ref<DebugLevel | null>(null)
/** 最近两秒内收到过电平。电平停了但状态还是「聆听中」= 设备不出数据了 */
const levelFresh = ref(false)
const loadError = ref('')

const unlisteners: Array<() => void> = []
let lastLevelAt = 0
let ticker: number | undefined

/** 「不监听」这一行的 id。设备 id 来自 cpal，永远不会是它 */
const OFF_ID = 'off'

/** 当前生效的行：选中的设备 id，或 OFF_ID */
const activeId = computed(() => {
  const choice = audio.value
  if (!choice) return ''
  if (choice.device) return choice.device.id
  // device 为 null 有两种含义（不监听 / 还没选过），这两种都不该高亮到某台设备上
  return OFF_ID
})

/**
 * 配置里记着、但这次枚举没找出来的那只设备。
 *
 * 单独显示它（而不是让那行凭空消失）是因为「我明明选过麦克风，列表里却没有」
 * 是最难自己想明白的一种情况 —— 用户会以为程序把它忘了。把它列出来、
 * 标明「已断开」，再点一下就重连，整个过程才是自洽的。
 */
const missingDevice = computed<DeviceChoice | null>(() => {
  const choice = audio.value?.device
  if (!choice) return null
  return devices.value.some((d) => d.id === choice.id) ? null : choice
})

onMounted(async () => {
  unlisteners.push(
    await listenAudioState((next) => {
      const wasFailed = audio.value?.status.kind === 'failed'
      audio.value = next
      // 设备列表只在挂载时枚举过一次（见 refreshDevices 的说明）。
      // 掉线恰恰是「列表已经过时」的信号 —— 这时候重列一次，
      // 用户看到的才是插拔之后的真实情况。
      if (next.status.kind === 'failed' && !wasFailed) void refreshDevices()
    }),
  )
  unlisteners.push(
    await listenLevel((next) => {
      level.value = next
      lastLevelAt = Date.now()
      levelFresh.value = true
    }),
  )

  window.addEventListener('keydown', onKeydown)
  // 回到这个窗口时重新枚举：插上/拔掉设备之后，用户第一反应就是点回这个框，
  // 那一瞬间把列表刷新掉，就不用再教他「要关掉重开」
  window.addEventListener('focus', onFocus)

  // 先要状态再列设备：高亮哪一行取决于状态，反过来不需要
  audio.value = await fetchAudioState()
  await refreshDevices()

  // 打开才推电平：它是 10 Hz 的高频数据，没人看的时候不该走 IPC
  await setMeter(true)

  // 电平停了但状态还写着「聆听中」，只可能是设备不出数据了。
  // 光看那条电平条会一直停在最后一个值上，看起来像「还在响」——
  // 这个定时器就是用来把它戳穿的。
  ticker = window.setInterval(() => {
    levelFresh.value = Date.now() - lastLevelAt < 2000
  }, 500)
})

onBeforeUnmount(() => {
  for (const off of unlisteners) off()
  window.removeEventListener('keydown', onKeydown)
  window.removeEventListener('focus', onFocus)
  if (ticker !== undefined) window.clearInterval(ticker)
  void setMeter(false)
})

/**
 * 重新枚举输入设备。
 *
 * **不做轮询**：枚举要过一遍 WASAPI 端点，而且设备插拔本来就是低频事件。
 * 刷新的时机只有三个：挂载、窗口重新获得焦点、以及刚被报告「设备用不了」时。
 * 想立刻刷新也可以点标题栏那个「刷新」。
 */
async function refreshDevices() {
  try {
    devices.value = await listInputDevices()
    loadError.value = ''
  } catch (err) {
    loadError.value = `枚举麦克风失败：${String(err)}`
    console.error('[pet] 枚举输入设备失败:', err)
  }
}

function onFocus() {
  void refreshDevices()
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    void close()
  }
}

/**
 * 关闭窗口。
 *
 * 走命令而不是 `getCurrentWindow().close()`：关窗的语义在 Rust 侧 ——
 * 手上没有可用麦克风时（还没选过、或选的设备连不上），把它关掉就等于选了「不监听」，
 * 那份决定要写进用户配置。
 */
async function close() {
  await closeMicPicker()
}

function isActive(id: string): boolean {
  return activeId.value === id
}

/** 正在选这只，但还没听到（模型加载 + 开流，一两秒） */
function isConnecting(device: DeviceChoice | null): boolean {
  const choice = audio.value
  if (!choice || choice.status.kind !== 'connecting') return false
  return (choice.device?.id ?? OFF_ID) === (device?.id ?? OFF_ID)
}

/** 这一行正显示着电平条：只有「正在聆听」且就是它的时候 */
function showsMeter(device: DeviceChoice | null): boolean {
  const choice = audio.value
  if (!choice || choice.status.kind !== 'listening') return false
  return (choice.device?.id ?? '') === (device?.id ?? '')
}

async function pick(device: DeviceChoice | null) {
  if (busy.value) return

  // 已经听着的就是它 —— 再点一次只会白停一次线程、白加载一遍模型
  const choice = audio.value
  if (choice && choice.status.kind === 'listening' && (choice.device?.id ?? OFF_ID) === (device?.id ?? OFF_ID)) {
    return
  }

  busy.value = true
  loadError.value = ''
  try {
    const next = await chooseAudioDevice(device)
    if (next) audio.value = next
  } catch (err) {
    loadError.value = `切换麦克风失败：${String(err)}`
    console.error('[pet] 切换麦克风失败:', err)
  } finally {
    busy.value = false
  }
}

/** 底部状态行：把「现在到底处于什么状态」说清楚，而不是让用户对着没声音发呆 */
const statusText = computed(() => {
  if (loadError.value) return loadError.value
  const choice = audio.value
  if (!choice) return ''
  switch (choice.status.kind) {
    case 'off':
      return '不监听：语气分析不启用。角色不会跟着声音做表情。'
    case 'connecting':
      return `正在连接「${choice.device?.name ?? '设备'}」…（加载模型约一秒）`
    case 'listening':
      return levelFresh.value
        ? '正在聆听。对着麦克风说句话，上面那条电平应该跟着跳。'
        : '正在聆听，但这只麦克风没有送来任何声音。'
    case 'need_choice':
      return '还没选过麦克风。选一只开始分析，或者选「不监听」。'
    case 'failed':
      return `没能用上：${choice.status.reason}`
  }
})

/** 状态行的语气：失败时标红，静止状态标灰 */
const statusTone = computed(() => {
  const kind = audio.value?.status.kind
  if (loadError.value || kind === 'failed') return 'bad'
  if (kind === 'listening' && levelFresh.value) return 'good'
  return 'muted'
})
</script>

<template>
  <div class="picker">
    <header class="picker__head">
      <h1 class="picker__title">选择麦克风</h1>
      <button type="button" class="picker__refresh" title="重新枚举输入设备" @click="refreshDevices">
        刷新
      </button>
      <button type="button" class="picker__close" @click="close">关闭</button>
    </header>

    <ul class="picker__list" :class="{ 'picker__list--busy': busy }">
      <!-- 「不监听」永远在最前，也永远可选：它和「没找到设备」是两回事 -->
      <li>
        <button
          type="button"
          class="row"
          :class="{ 'row--active': isActive(OFF_ID) }"
          @click="pick(null)"
        >
          <span class="row__dot" />
          <span class="row__name">不监听</span>
          <span class="row__note">语气分析不启用</span>
        </button>
      </li>

      <li v-for="device in devices" :key="device.id">
        <button
          type="button"
          class="row"
          :class="{ 'row--active': isActive(device.id) }"
          :data-device-id="device.id"
          @click="pick({ id: device.id, name: device.name })"
        >
          <span class="row__dot" />
          <span class="row__name" :title="device.name">{{ device.name }}</span>
          <span v-if="device.is_default" class="row__badge">默认</span>
          <span v-if="isConnecting({ id: device.id, name: device.name })" class="row__note">
            连接中…
          </span>
          <span v-else-if="showsMeter({ id: device.id, name: device.name })" class="row__meter">
            <span
              class="row__meter-fill"
              :class="{ 'row__meter-fill--stale': !levelFresh }"
              :style="{ width: `${levelPercent(level?.peak ?? 0)}%` }"
            />
          </span>
        </button>
      </li>

      <!-- 记着、但这次没枚举到的那只：说出来，别让它静悄悄地消失 -->
      <li v-if="missingDevice">
        <button
          type="button"
          class="row row--missing"
          :class="{ 'row--active': isActive(missingDevice.id) }"
          @click="pick(missingDevice)"
        >
          <span class="row__dot" />
          <span class="row__name" :title="missingDevice.name">{{ missingDevice.name }}</span>
          <span class="row__note">已断开</span>
        </button>
      </li>

      <li v-if="devices.length === 0" class="picker__empty">
        没有找到任何麦克风。检查一下设备是不是被别的程序占着，或者选「不监听」。
      </li>
    </ul>

    <footer class="picker__foot" :class="`picker__foot--${statusTone}`">
      {{ statusText }}
    </footer>
  </div>
</template>

<style scoped>
/*
 * 定尺寸小框：360×460（见 commands.rs 的 PICKER_WINDOW_SIZE）。
 * 这里用 flex 让列表吃掉剩余高度，设备多的时候列表内部滚动，
 * 头尾两行始终在位 —— 状态行滚没了就等于没有状态行。
 */
.picker {
  display: flex;
  flex-direction: column;
  height: 100%;
  padding: 12px;
  gap: 10px;
  font:
    13px / 1.5 system-ui,
    'Microsoft YaHei',
    sans-serif;
}

.picker__head {
  display: flex;
  align-items: center;
  gap: 6px;
}

.picker__title {
  flex: 1;
  margin: 0;
  font-size: 14px;
  font-weight: 600;
}

.picker__refresh,
.picker__close {
  padding: 3px 10px;
  border: 1px solid rgba(255, 255, 255, 0.18);
  border-radius: 6px;
  background: transparent;
  color: inherit;
  font: inherit;
  cursor: pointer;
}

.picker__refresh:hover,
.picker__close:hover {
  background: rgba(255, 255, 255, 0.08);
}

.picker__list {
  flex: 1;
  overflow-y: auto;
  margin: 0;
  padding: 0;
  list-style: none;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px;
}

/* 切换期间置灰：命令本身要停旧线程，连点只会排起一堆等待 */
.picker__list--busy {
  opacity: 0.55;
  pointer-events: none;
}

.row {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 8px 10px;
  border: 0;
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  background: transparent;
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}

.row:hover {
  background: rgba(255, 255, 255, 0.05);
}

.row--active {
  background: rgba(74, 222, 128, 0.12);
}

.row__dot {
  flex: none;
  width: 8px;
  height: 8px;
  border: 1px solid rgba(255, 255, 255, 0.4);
  border-radius: 50%;
}

.row--active .row__dot {
  border-color: #4ade80;
  background: #4ade80;
}

.row__name {
  flex: 1;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

/* 记着但当下不在的那只：虚线边框表示「这一项不是此刻枚举出来的」 */
.row--missing {
  border: 1px dashed rgba(248, 113, 113, 0.5);
  border-radius: 0;
}

.row--missing .row__name {
  color: rgba(255, 255, 255, 0.7);
}

.row--missing .row__dot {
  border-color: #f87171;
}

.row__badge {
  flex: none;
  padding: 1px 5px;
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.12);
  font-size: 11px;
  color: rgba(255, 255, 255, 0.75);
}

.row__note {
  flex: none;
  font-size: 11px;
  color: rgba(255, 255, 255, 0.5);
}

.row__meter {
  flex: none;
  width: 72px;
  height: 6px;
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.1);
  overflow: hidden;
}

.row__meter-fill {
  display: block;
  height: 100%;
  background: linear-gradient(90deg, #4ade80, #facc15 70%, #f87171);
  transition: width 0.08s linear;
}

/* 两秒没数据 → 变暗。停在最后一个值上的条比空条更骗人 */
.row__meter-fill--stale {
  opacity: 0.25;
}

.picker__empty {
  padding: 10px;
  color: rgba(255, 255, 255, 0.55);
}

.picker__foot {
  min-height: 34px;
  font-size: 12px;
  color: rgba(255, 255, 255, 0.6);
}

.picker__foot--good {
  color: #4ade80;
}

.picker__foot--bad {
  color: #f87171;
}
</style>
