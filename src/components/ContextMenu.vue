<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'

/**
 * 角色右键菜单。
 *
 * DEV.md §3.3：角色右键用前端自定义菜单，托盘用 Tauri 原生菜单，
 * 两者只共享菜单定义与退出逻辑，不要求同一种 UI 实现。
 * 因此这里只负责渲染和选中，真正的退出动作由父组件调用后端。
 */
export interface MenuItem {
  id: string
  label: string
}

const props = defineProps<{
  visible: boolean
  x: number
  y: number
  items: MenuItem[]
}>()

const emit = defineEmits<{
  (e: 'select', id: string): void
  (e: 'close'): void
}>()

const root = ref<HTMLElement | null>(null)

function onSelect(id: string) {
  emit('select', id)
  emit('close')
}

function onDocumentPointerDown(event: PointerEvent) {
  if (!props.visible) return
  if (root.value && !root.value.contains(event.target as Node)) {
    emit('close')
  }
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') emit('close')
}

onMounted(() => {
  // 用捕获阶段，保证点在别处一定能把菜单收起来
  document.addEventListener('pointerdown', onDocumentPointerDown, true)
  document.addEventListener('keydown', onKeydown)
})

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', onDocumentPointerDown, true)
  document.removeEventListener('keydown', onKeydown)
})

// 菜单贴近窗口边缘时回拉，避免被裁掉
const style = ref<Record<string, string>>({})
watch(
  () => [props.visible, props.x, props.y] as const,
  () => {
    if (!props.visible) return
    const width = 132
    const height = 44 + props.items.length * 30
    const left = Math.max(2, Math.min(props.x, window.innerWidth - width - 2))
    const top = Math.max(2, Math.min(props.y, window.innerHeight - height - 2))
    style.value = { left: `${left}px`, top: `${top}px` }
  },
  { immediate: true },
)
</script>

<template>
  <Transition name="menu-fade">
    <div v-if="visible" ref="root" class="pet-menu" :style="style" role="menu">
      <button
        v-for="item in items"
        :key="item.id"
        class="pet-menu__item"
        type="button"
        role="menuitem"
        @click="onSelect(item.id)"
      >
        {{ item.label }}
      </button>
    </div>
  </Transition>
</template>

<style scoped>
.pet-menu {
  position: fixed;
  z-index: 100;
  min-width: 128px;
  padding: 4px;
  border-radius: 8px;
  background: rgba(28, 28, 32, 0.94);
  border: 1px solid rgba(255, 255, 255, 0.12);
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
  backdrop-filter: blur(8px);
  font-family: 'Microsoft YaHei', system-ui, sans-serif;
  /* 角色窗口本身可拖动，菜单需要自己接收指针事件 */
  pointer-events: auto;
}

.pet-menu__item {
  display: block;
  width: 100%;
  padding: 6px 12px;
  border: 0;
  border-radius: 5px;
  background: transparent;
  color: #f2f2f5;
  font-size: 13px;
  line-height: 1.4;
  text-align: left;
  cursor: default;
}

.pet-menu__item:hover {
  background: rgba(255, 255, 255, 0.14);
}

.menu-fade-enter-active,
.menu-fade-leave-active {
  transition: opacity 0.1s ease;
}

.menu-fade-enter-from,
.menu-fade-leave-to {
  opacity: 0;
}
</style>
