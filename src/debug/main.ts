import { createApp } from 'vue'
import DebugPanel from '../components/DebugPanel.vue'
import './debug.css'

/**
 * 调试窗口的入口（页面是项目根目录的 debug.html）。
 *
 * 独立于角色窗口的 main.ts：那个入口带的是透明窗口的基线样式和动画样式表，
 * 这个窗口一个都不需要，而这里需要的深色背景与滚动那边也没有。
 */
createApp(DebugPanel).mount('#app')
