<script setup lang="ts">
import { computed } from 'vue'
import type { ExpressionName, MotionName, MotionParams } from '../types/theme'
import type { LoadedTheme } from '../theme/registry'

/**
 * 角色展示组件。
 *
 * 纯展示：自身不持有任何定时器，也不做状态判断 ——
 * 播到第几步、下一步是什么由 AnimationScheduler 决定，这里只把状态翻译成
 * 图片、class 与 CSS 变量。
 *
 * **表情与动作在这里是分开的两个入参**，这就是这次重构最直观的地方：
 * `expression` 决定用哪张图，`motion` 决定这一拍怎么动。一张图配上不同的动作，
 * 或者同一段动画给好几个表情用，都不需要复制任何东西（DEV.md §4.3）。
 */
const props = defineProps<{
  theme: LoadedTheme
  /** 当前表情：决定显示哪张图 */
  expression: ExpressionName
  /** 当前这一拍的动作名：决定 class 与运动参数 */
  motion: MotionName
}>()

const emit = defineEmits<{
  /** 这一拍的动作演完了（冒泡上来的呼吸动画已被过滤掉） */
  (e: 'animationend', motion: MotionName): void
}>()

/** 当前表情对应的素材（缺表情图时由 registry 回退到 base.png） */
const imageUrl = computed(() => props.theme.resolveImage(props.expression))

/** 动作参数名 → CSS 自定义属性名的映射。加一个旋钮这里就要加一行（见 theme.ts）。*/
const CSS_VARS: Array<[keyof MotionParams, string]> = [
  ['tilt', '--pet-tilt'],
  ['drift', '--pet-drift'],
  ['bob', '--pet-bob'],
  ['breath', '--pet-breath'],
  ['shake', '--pet-shake'],
  ['sink', '--pet-sink'],
  ['pop', '--pet-pop'],
  ['squash', '--pet-squash'],
]

/**
 * 把这一拍动作的参数搬成 CSS 自定义属性，组件里不写死任何数值。
 *
 * `--pet-duration` 由 `durationMs` 推导（数字 + "ms"）—— **时长的唯一来源就是它**，
 * 关键帧里各处的 var() 默认值只是为了让 CSS 单独看也说得通。
 * 调度器的兜底定时器用的是同一个 durationMs，两边不会漂。
 */
const cssVars = computed<Record<string, string>>(() => {
  const config = props.theme.definition.motions[props.motion]
  const vars: Record<string, string> = {}
  if (!config) {
    // 配置里没有这个动作名（validateTheme 会先报错，这里只是不让它炸在模板上）
    console.error(`[pet] 动作 "${props.motion}" 不在 theme.json 的 motions 里，这一拍不会有动画`)
    return vars
  }

  vars['--pet-duration'] = `${config.durationMs}ms`
  for (const [key, cssName] of CSS_VARS) {
    const value = config.params?.[key]
    if (value !== undefined) vars[cssName] = value
  }
  return vars
})

function onAnimationEnd(event: AnimationEvent) {
  // 呼吸动画挂在子元素上会冒泡到这里，只处理目标为运动层的事件
  if (event.target !== event.currentTarget) return
  // 回传**自己渲染的那个动作名**：事件可能来自上一拍（已经演完、事件还在队列里），
  // 名字对不上时调度器会丢掉它。见 scheduler.ts 的 handleAnimationEnd。
  emit('animationend', props.motion)
}
</script>

<template>
  <div class="pet-stage">
    <!-- data-tauri-drag-region 让 Tauri 用原生方式拖动窗口，
         比用 JS 计算位移更稳，也不会和右键菜单冲突。 -->
    <div
      class="pet-motion"
      :class="`is-${motion}`"
      :style="cssVars"
      data-tauri-drag-region
      @animationend="onAnimationEnd"
    >
      <div class="pet-breathe" data-tauri-drag-region>
        <img class="pet-image" :src="imageUrl" :alt="theme.definition.name" draggable="false" />
      </div>
    </div>
  </div>
</template>

<style scoped>
/*
 * 角色框：尺寸来自 theme.json 的 `stage`，由 App.vue 注入下面这两个变量。
 *
 * 窗口与它等大（DEV.md §3.1），所以这两个数现在等于窗口尺寸；但仍然显式写死，
 * 因为角色看起来多大由 theme.json 说了算，不该由窗口反过来决定。
 * flex: none 是必须的 —— 否则在 flex 容器里会被压缩回窗口大小。
 */
.pet-stage {
  width: var(--pet-stage-width, 100%);
  height: var(--pet-stage-height, 100%);
  flex: none;
  display: flex;
  align-items: flex-end;
  justify-content: center;
  /* 角色之外的区域保持完全透明 */
  background: transparent;
}

.pet-motion,
.pet-breathe {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: flex-end;
  justify-content: center;
}
</style>
