import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Tauri 开发时前端由 Vite 提供，生产时打包进 WebView。
// 端口固定为 1420 并与 tauri.conf.json 的 devUrl 保持一致，避免 Tauri 连不上而静默失败。
export default defineConfig({
  plugins: [vue()],

  // Tauri CLI 会注入 TAURI_ 前缀的环境变量，需要一并暴露给前端。
  envPrefix: ['VITE_', 'TAURI_'],

  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // src-tauri 由 Rust 侧自己监听，Vite 重复监听会造成无谓的重启。
      ignored: ['**/src-tauri/**', '**/models/**'],
    },
  },

  build: {
    // 使用系统 WebView2，无需为旧浏览器降级。
    target: 'chrome110',
    sourcemap: false,
  },
})
