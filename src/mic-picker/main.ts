import { createApp } from 'vue'
import MicPicker from '../components/MicPicker.vue'
import './mic-picker.css'

/**
 * 麦克风选择框的入口（页面是项目根目录的 mic-picker.html）。
 *
 * 与调试窗口同样是独立入口：那个窗口的样式与角色窗口完全不同，
 * 这个框又只放几行设备，三边共用一份基线只会互相迁就。
 */
createApp(MicPicker).mount('#app')
