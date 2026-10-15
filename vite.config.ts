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
    rollupOptions: {
      // 两个入口：角色窗口（透明桌宠）与调试窗口（独立工具窗）。
      // Tauri 用 WebviewUrl::App("debug.html") 打开后者，缺了这个入口
      // 开发时能跑（Vite 直接服务该文件），打包后调试窗口会是白屏。
      //
      // 用相对路径而不是 path.resolve(__dirname, ...)：这份配置要把
      // `npm run typecheck` 也过一遍（tsconfig 的 include 里有它），而 tsconfig
      // 没有 @types/node，`__dirname` / `node:url` 都会直接报错。
      // 入口相对的是运行目录，也就是 `npm run build` / `tauri build` 的工程根目录。
      input: {
        main: 'index.html',
        debug: 'debug.html',
      },
    },
  },
})
