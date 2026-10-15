<script setup lang="ts">
import { computed } from 'vue'
import type { ActionName, ActionParams } from '../types/theme'
import type { LoadedTheme } from '../theme/registry'

/**
 * 角色展示组件。
 *
 * 纯展示：自身不持有任何定时器，也不做状态判断 ——
 * 动画状态由 AnimationScheduler 统一调度，这里只把状态翻译成 class 与 CSS 变量。
 */
const props = defineProps<{
  theme: LoadedTheme
  action: ActionName
}>()

const emit = defineEmits<{
  (e: 'animationend', action: ActionName): void
  (e: 'contextmenu', payload: { x: number; y: number }): void
}>()

/** 当前动作对应的素材（缺表情图时由 registry 回退到 base.png） */
const imageUrl = computed(() => props.theme.resolveAsset(props.action))

/** 把 theme.json 的运动参数搬成 CSS 自定义属性，组件里不写死任何数值 */
const cssVars = computed<Record<string, string>>(() => {
  const params: ActionParams = props.theme.definition.actions[props.action]?.params ?? {}
  const vars: Record<string, string> = {}
  const map: Array<[keyof ActionParams, string]> = [
    ['duration', '--pet-duration'],
    ['tilt', '--pet-tilt'],
    ['drift', '--pet-drift'],
    ['bob', '--pet-bob'],
    ['breath', '--pet-breath'],
    ['shake', '--pet-shake'],
    ['sink', '--pet-sink'],
    ['pop', '--pet-pop'],
  ]
  for (const [key, cssName] of map) {
    const value = params[key]
    if (value !== undefined) vars[cssName] = value
  }
  return vars
})

function onAnimationEnd(event: AnimationEvent) {
  // 呼吸动画挂在子元素上会冒泡到这里，只处理目标为运动层的事件
  if (event.target !== event.currentTarget) return
  emit('animationend', props.action)
}

function onContextMenu(event: MouseEvent) {
  event.preventDefault()
  emit('contextmenu', { x: event.clientX, y: event.clientY })
}
</script>

<template>
  <div class="pet-stage" @contextmenu="onContextMenu">
    <!-- data-tauri-drag-region 让 Tauri 用原生方式拖动窗口，
         比用 JS 计算位移更稳，也不会和右键菜单冲突。 -->
    <div
      class="pet-motion"
      :class="`is-${action}`"
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
.pet-stage {
  width: 100%;
  height: 100%;
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
