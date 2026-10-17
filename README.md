# Livestream Pet - zhuzhu

![platform](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D6)
![tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)
![vue](https://img.shields.io/badge/Vue-3-4FC08D?logo=vuedotjs&logoColor=white)
![rust](https://img.shields.io/badge/Rust-1.77%2B-000000?logo=rust&logoColor=white)

<img src="themes/zhuzhu/expressions/idle.png" width="200" alt="奇魔猪">

一只住在桌面上的小猪。根据你的声音做着不同的动作，一起陪伴你的直播。

---

## 现在能做什么

| 能力                 | 说明                                                                                                                                                                                |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **透明桌宠窗口**     | 无边框、透明背景、160×160 与角色框**等大**，可以拖到屏幕任意角落，位置与置顶状态退出时记住                                                                                          |
| **表情 + 动作两层**  | 7 个表情（idle / speaking / happy / angry / sad / surprised / laugh）+ 20 个可复用动作（sway / hop / jump / flip / squash / pop …），一个表情由一串动作按序演出来，加表情不用改引擎 |
| **本地语音情绪分析** | 麦克风 → Silero VAD → SenseVoice（int8）→ 情绪稳定化 → 动画事件，全流程离线                                                                                                         |
| **麦克风选择框**     | 菜单里选设备，列表带**实时电平**，选中的那只当场验证有没有声音；选择写进配置，重启自动用同一只                                                                                      |
| **调试窗口**         | 按 `D` 打开：当前演到第几步、每个动作序列、定时器有没有泄漏、原始识别文本与情绪标签、输入电平                                                                                       |
| **托盘 + 右键菜单**  | 选择麦克风 / 置顶窗口 / 退出，角色右键菜单与托盘菜单是同一份定义                                                                                                                    |

## 快速开始

### 环境要求

- Windows 10/11（走 WASAPI 采集，依赖系统 WebView2，Win11 自带）
- Rust 1.77+（MSVC 工具链）
- Node.js 20.19+ 或 22.12+（Vite 8 的要求；用 `tools/cdp_probe.mjs` 排查界面时需要 22+）

### 拉取模型

模型约 230 MB，**不入库**，必须单独下载：

```bash
npm run fetch-models
```

脚本是 bash，Windows 上请在 Git Bash 或 WSL 里执行。默认走 `hf-mirror.com` 镜像，支持断点续传；
能直连 HuggingFace 的话用 `HF_ENDPOINT` 覆盖。

### 跑起来

```bash
npm install
npm run tauri dev
```

首次启动会弹出麦克风选择框（配置里还没有这项）：选一只麦克风，或者选「不监听」——
后者会把语气分析整套停掉，程序仍然正常运行，只是不再跟着声音做表情。

### 打包

```bash
npm run tauri build
```

> 当前进度停在阶段 4 之前（见 [DEV.md](DEV.md) 的「开发阶段与验收标准」）：能构建，
> 但安装包体积、运行时依赖与重复启动场景还没验收过。

## 它是怎么工作的

```
   ┌─ Rust 侧 ──────────────────────────────────────────────┐
   │  cpal 采集 → VAD → SenseVoice → 情绪稳定化 → 标准化事件 │
   └───────────────────────────┬────────────────────────────┘
                               │ pet://event（Tauri 事件通道）
   ┌─ 前端 ────────────────────▼────────────────────────────┐
   │  动画调度器 → 表情（换图）+ 动作序列（CSS keyframes）   │
   └────────────────────────────────────────────────────────┘
```

音频采集回调里**只做入队，不做推理**：模型在独立的 `pet-audio` 线程上加载与运行，
推理卡顿不会传导到动画上。前端拿到的始终是精简后的事件，原始音频不出进程。

### 三份配置，各管一段

| 文件                                               | 管什么                                        | 谁读 | 怎么部署                                          |
| -------------------------------------------------- | --------------------------------------------- | ---- | ------------------------------------------------- |
| `src-tauri/config/audio.json`                      | 检测参数：VAD 阈值、模型路径、稳定化窗口      | Rust | 跟 EXE 放同级（缺了就用内置默认值，程序照常启动） |
| `themes/<主题>/theme.json`                         | 表现参数：事件→表情映射、动作序列、角色框尺寸 | 前端 | **随包发布**，构建期由 Vite 打进前端              |
| `%APPDATA%/com.livestreampet.zhuzhu/settings.json` | **用户状态**：窗口位置、置顶、选了哪只麦克风  | 双方 | 不发布、不入库                                    |

前两份是**配置**（只读、改完重启），第三份是**用户状态**，程序只覆盖自己那两节，
用户手写的键一律原样保留 —— 见 [DEV.md](DEV.md) 3.1。

模型走同一套「跟 EXE 同级」的规则（`models/`），两者都可以用环境变量
`LIVESTREAM_PET_MODELS_DIR` / `LIVESTREAM_PET_CONFIG_DIR` 指到别处。

## 目录结构

```
src/                    前端（Vue 3 + TS）
  App.vue               角色窗口：菜单、拖拽、键盘
  components/           PetCharacter / DebugPanel / MicPicker / ContextMenu
  anim/                 动作关键帧（animations.css）与调度器（scheduler.ts）
  theme/registry.ts     theme.json 的加载与校验
  backend.ts            所有 invoke / listen 的唯一出口（带 isTauri 守卫）
  types/                事件、主题、音频的类型与运行时守卫
src-tauri/src/          Rust 侧
  app.rs                装配层：窗口、状态机、音频线程生死的唯一决策处
  audio/                capture（cpal）/ vad / asr / worker / emotion
  commands.rs           前端能调的命令
  menu.rs tray.rs       菜单定义与系统托盘
  window_state.rs       用户配置数据库（读写 + 迁移 + 41 项单元测试）
  events.rs             前后端事件通道名与负载类型
themes/zhuzhu/          主题素材：base.png + expressions/ + theme.json
tools/                  自查工具（见下）
models/                 语音模型（gitignore，由 fetch-models 下载）
src-tauri/vendor/       vendor 过的 sherpa-onnx（上游丢了 lang/emotion/event 字段）
```

## 开发

### 常用命令

| 命令                  | 作用                                                         |
| --------------------- | ------------------------------------------------------------ |
| `npm run tauri dev`   | 启动应用（前端热重载，Rust 改动自动重编重启）                |
| `npm run dev`         | **只起前端**，在浏览器里调界面与动画；后端数据与菜单是假数据 |
| `npm run typecheck`   | `vue-tsc --noEmit`                                           |
| `cargo test --lib`    | Rust 单元测试（在 `src-tauri/` 下跑）                        |
| `npm run fit`         | 动画越界校验，见下                                           |
| `npm run cdp -- ...`  | 远程调试探针，见下                                           |
| `npm run expressions` | 从 `face.jpg` 批量生成表情 PNG                               |

### 三个自查工具

界面问题不能靠「看一眼屏幕」判断：一个白窗口可能是渲染错了、资源没加载，也可能是后端
**压根卡住了** —— 三者要修的地方完全不同。

```bash
# 1. 动画越界 + 动作库校验：改角色尺寸、窗口尺寸或动作幅度后必跑
npm run fit

# 2. 进到页面内部看第一手状态（DOM / 计算样式 / 直接调后端命令）
WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" npm run tauri dev
npm run cdp -- list                       # 三个窗口分别是哪个页面
npm run cdp -- dump debug.html             # readyState / 根节点长度 / 背景色
npm run cdp -- eval http://localhost:1420/ "window.__TAURI_INTERNALS__.invoke('audio_state')"
npm run cdp -- shot http://localhost:1420/ C:\Temp\pet.png

# 3. 抠底与基线对齐（表情素材）
npm run expressions
```

`fit` 会把每条 keyframe 的变换真的作用到角色轮廓上（含缓动过冲与 3D 翻转的透视投影），
报出每个动作被窗口边切掉多少像素，同时校验动作名、关键帧、缓动是否对得上 —— 名字打错
等于整段动画被浏览器丢掉，看着就是「没反应」。排查方法见 [DEV.md](DEV.md) 3.5。
