<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  emitPetEvent,
  isTauri,
  listenDebug,
  listenPetState,
  requestPetState,
  setDebug,
  toggleDebugWindow,
} from '../backend'
import { loadTheme } from '../theme/registry'
import { EXPRESSION_NAMES, type ExpressionName } from '../types/theme'
import {
  PET_EVENT_KINDS,
  type DebugLevel,
  type DebugTranscript,
  type PetEventKind,
} from '../types/events'
import type { PetState } from '../types/pet-state'

/**
 * 调试窗口（独立 Tauri 窗口，页面是 debug.html）。
 *
 * 它自己**不持有任何动画状态**：表情按钮只是往 `pet://event` 广播一个标准化事件，
 * 角色窗口收到后照常调度 —— 和麦克风链路完全同一条路。所以这里不会出现
 * 「调试能过、真实链路不行」的假通过。
 *
 * 反过来，角色窗口的调度器状态（当前表情 / 第几步 / 动作 / 定时器数）通过
 * `pet://state` 广播过来，
 * 语音链路的原始数据（电平、识别原文）走后端的 `pet://debug`。
 */

const state = ref<PetState | null>(null)
const level = ref<DebugLevel | null>(null)
const transcripts = ref<DebugTranscript[]>([])
/** 独立窗口有的是地方，比原来浮层里的 8 条多留一些，方便回看 */
const MAX_TRANSCRIPTS = 200

const unlisteners: Array<() => void> = []

onMounted(async () => {
  unlisteners.push(await listenPetState((next) => (state.value = next)))
  unlisteners.push(await listenDebug(onDebug))
  // 打开面板才让后端推数据，关掉就停 —— 正常运行时这条链路是静的
  await setDebug(true)
  // 角色窗口只在状态变化时广播，刚打开时可能什么都没变，主动要一份
  await requestPetState()
  window.addEventListener('keydown', onKeydown)
})

onBeforeUnmount(() => {
  for (const off of unlisteners) off()
  window.removeEventListener('keydown', onKeydown)
  void setDebug(false)
})

function onKeydown(event: KeyboardEvent) {
  // Esc 和 D 都能关掉自己。角色窗口那边也是按 D 开关，但调试窗口一出现就拿到焦点，
  // 之后的按键不会传到角色窗口 —— 只有那边认 D 的话，用户按 D 会觉得「关不掉」。
  if (event.key === 'Escape' || event.key === 'd' || event.key === 'D') {
    void toggleDebugWindow()
  }
}

function onDebug(info: DebugTranscript | DebugLevel) {
  if (info.type === 'level') {
    level.value = info
    return
  }
  // 语言/情绪/事件都可能是空串（模型未返回），显示成 ∅ 而不是留白，
  // 否则分不清「没识别出来」和「这一栏根本不显示」
  transcripts.value = [info, ...transcripts.value].slice(0, MAX_TRANSCRIPTS)
}

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

/**
 * 表情按钮 → 标准化事件。
 *
 * 不是直接切表情：先把它翻译成对应的事件，再走和真实链路一样的通道，
 * 这样按钮同时也是对「事件 → 表情映射」的验收（DEV.md §5.5）。
 * 面板里**没有直接切动作的按钮** —— 动作由 theme.json 里表情的序列决定，
 * 想试某个动作就把它写进某个表情的序列里。
 */
const KIND_BY_EXPRESSION: Record<ExpressionName, PetEventKind> = {
  idle: 'idle',
  speaking: 'voice_start',
  happy: 'happy',
  angry: 'angry',
  sad: 'sad',
  surprised: 'surprised',
  laugh: 'laugh',
}

function triggerExpression(expression: ExpressionName) {
  void emitPetEvent(KIND_BY_EXPRESSION[expression])
}

function emitKind(kind: PetEventKind) {
  void emitPetEvent(kind)
}

const lastEventText = computed(() =>
  state.value?.lastEvent ? `${state.value.lastEvent.kind} @ ${state.value.lastEvent.ts}` : '无',
)

// 直接读 theme.json，不另抄一份：配置文件就是唯一真相。
const theme = loadTheme()

/** 当前表情的播放序列，用来一眼看出「现在演到哪儿了」 */
const playlist = computed(() => {
  if (!state.value) return []
  return theme.definition.expressions[state.value.expression]?.motions ?? []
})
</script>

<template>
  <section class="debug">
    <header class="debug__bar">
      <strong>动画调试</strong>
      <span class="debug__meta">
        {{ state ? '角色窗口: 已连接' : '角色窗口: 等待中…' }}
      </span>
      <button type="button" class="debug__close" @click="toggleDebugWindow()">关闭 (Esc)</button>
    </header>

    <p v-if="!isTauri" class="debug__warn">
      当前不在 Tauri 里运行，收不到角色窗口的数据，按钮也送不出去。请用
      <code>npm run tauri dev</code> 启动。
    </p>

    <div class="debug__body">
      <p class="debug__line">
        当前: <b>{{ state?.expression ?? '—' }}</b>
        · 第 {{ state ? state.stepIndex + 1 : '—' }}/{{ state?.stepCount ?? '—' }} 步
        · 动作 <code>{{ state?.motion ?? '—' }}</code>
        · 人声: {{ state ? (state.voiceActive ? '有' : '无') : '—' }}
        · 定时器: {{ state?.pendingTimers ?? '—' }}
        <span class="debug__hint">
          （最多 2：这一拍的兜底 + 静音超时，静音超时触发后就只剩 1；只增不减就是泄漏）
        </span>
      </p>
      <p class="debug__line">
        序列:
        <template v-for="(m, i) in playlist" :key="`${i}-${m}`">
          <code :class="{ 'debug__step-now': i === state?.stepIndex }">{{ m }}</code>
          <span v-if="i < playlist.length - 1"> → </span>
        </template>
        <code v-if="!playlist.length">—</code>
        <span class="debug__hint">（按时间顺序依次播放，当前那一步高亮）</span>
      </p>
      <p class="debug__line">最近事件: <code>{{ lastEventText }}</code></p>

      <p class="debug__label">表情 <span class="debug__hint">（翻译成事件后广播，与真实链路同路）</span></p>
      <div class="debug__row">
        <button v-for="e in EXPRESSION_NAMES" :key="e" type="button" @click="triggerExpression(e)">
          {{ e }}
        </button>
      </div>

      <p class="debug__label">标准化事件</p>
      <div class="debug__row">
        <button v-for="k in PET_EVENT_KINDS" :key="k" type="button" @click="emitKind(k)">
          {{ k }}
        </button>
      </div>

      <hr class="debug__sep" />

      <p class="debug__label">语音链路</p>
      <p class="debug__line">
        输入电平: <b>{{ level ? dbfs(level.peak) : '—' }}</b> dBFS
        · 有效值 {{ level ? dbfs(level.rms) : '—' }} dBFS
        · 丢弃 {{ level?.dropped ?? 0 }} 块
        · 驱动错误 {{ level?.capture_errors ?? 0 }} 次
      </p>
      <div class="debug__meter">
        <div class="debug__meter-fill" :style="{ width: `${levelPercent(level?.peak ?? 0)}%` }" />
      </div>

      <p class="debug__label">
        识别（模型原始输出，新→旧）
        <button
          v-if="transcripts.length"
          type="button"
          class="debug__clear"
          @click="transcripts = []"
        >
          清空
        </button>
      </p>
      <p v-if="transcripts.length === 0" class="debug__line">
        暂无。对着麦克风说句话；电平一直是 -∞ 说明设备没送音频上来。
      </p>
      <ul v-else class="debug__log">
        <li v-for="(t, i) in transcripts" :key="`${t.audio_secs}-${i}`">
          <span :class="t.segment ? 'debug__seg' : 'debug__interim'">
            {{ t.segment ? '段' : '中' }}
          </span>
          {{ t.audio_secs.toFixed(2) }}s · {{ t.elapsed_ms }}ms ·
          {{ t.lang || '∅' }} · {{ t.emotion || '∅' }} · {{ t.event || '∅' }} ·
          <code>{{ t.text || '（空）' }}</code>
        </li>
      </ul>
    </div>
  </section>
</template>

<style scoped>
.debug {
  display: flex;
  flex-direction: column;
  width: 100%;
  height: 100%;
  padding: 8px 10px;
  color: #e8e8ee;
  font-family: Consolas, 'Courier New', monospace;
  font-size: 12px;
  line-height: 1.6;
}

.debug__bar {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: none;
  padding-bottom: 6px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.14);
}

.debug__bar .debug__meta {
  margin-left: auto;
}

.debug__close,
.debug__clear {
  padding: 2px 8px;
  border: 1px solid rgba(255, 255, 255, 0.18);
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.08);
  color: inherit;
  font: inherit;
  cursor: pointer;
}

.debug__close:hover,
.debug__clear:hover {
  background: rgba(255, 255, 255, 0.18);
}

.debug__warn {
  flex: none;
  margin: 8px 0 0;
  padding: 6px 8px;
  border-radius: 4px;
  background: rgba(248, 113, 113, 0.14);
  border: 1px solid rgba(248, 113, 113, 0.4);
}

/* 内容比窗口高时整体滚动，而不是让某一块自己滚 ——
   调试时最想要的是「一眼看全」，分成两个滚动区反而更难对齐 */
.debug__body {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding-top: 4px;
}

.debug__meta,
.debug__hint {
  color: #9fa3b0;
}

.debug__hint {
  font-size: 11px;
}

.debug__label {
  margin: 10px 0 4px;
  color: #9fa3b0;
}

.debug__line {
  margin: 4px 0;
  color: #c9ccd6;
  word-break: break-all;
}

.debug__row {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.debug__row button {
  padding: 3px 9px;
  border: 1px solid rgba(255, 255, 255, 0.18);
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.08);
  color: inherit;
  font: inherit;
  cursor: pointer;
}

.debug__row button:hover {
  background: rgba(255, 255, 255, 0.18);
}

.debug__sep {
  margin: 12px 0 0;
  border: 0;
  border-top: 1px solid rgba(255, 255, 255, 0.14);
}

/* 动作库清单：名字用等宽字体（要照着它写进 theme.json），其余是给人读的中文 */
/* 序列里当前那一步 */
.debug__step-now {
  color: #12121a;
  background: #7dd3fc;
  border-radius: 3px;
  padding: 0 3px;
}

.debug__meter {
  height: 6px;
  margin: 4px 0 0;
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.1);
  overflow: hidden;
}

.debug__meter-fill {
  height: 100%;
  background: linear-gradient(90deg, #4ade80, #facc15 70%, #f87171);
  transition: width 0.08s linear;
}

.debug__log {
  margin: 0;
  padding: 0;
  list-style: none;
}

.debug__log li {
  padding: 2px 0;
  border-bottom: 1px solid rgba(255, 255, 255, 0.05);
  color: #c9ccd6;
  word-break: break-all;
}

.debug__seg,
.debug__interim {
  display: inline-block;
  width: 1.1em;
  margin-right: 4px;
  border-radius: 3px;
  text-align: center;
  color: #12121a;
}

/* 语音段是权威解码，中间解码只是过程中的近似，用颜色区分强调程度 */
.debug__seg {
  background: #7dd3fc;
}

.debug__interim {
  background: #6b7280;
  color: #e8e8ee;
}

.debug__clear {
  float: right;
  padding: 0 6px;
  font-size: 11px;
}
</style>
