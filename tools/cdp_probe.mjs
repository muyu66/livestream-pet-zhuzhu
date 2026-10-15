#!/usr/bin/env node
/**
 * 看不见界面时的取证工具：连 Tauri 的 WebView2 内部，直接读窗口里到底发生了什么。
 *
 * 为什么需要它：界面问题（空白窗口、按钮没反应、命令不返回）如果只能靠「看一眼屏幕」
 * 判断，就永远说不清是渲染错了、资源没加载、还是后端卡住了。这里通过 WebView2 自带的
 * DevTools 协议（CDP）拿到的是**页面内部的第一手状态**：DOM、计算样式、以及直接调用
 * 后端命令的返回值。
 *
 * 前置条件：**应用必须以远程调试端口启动**（该端口只在排查时开，不写进 tauri.conf.json，
 * 否则每次运行都会敞着一个调试端口）：
 *
 *   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" npm run tauri dev
 *
 * 用法（端口可用 CDP_PORT 覆盖）：
 *
 *   node tools/cdp_probe.mjs list
 *       列出所有页面：标题 + URL。
 *       URL 是 about:blank 而标题对得上，通常表示 webview 建出来了但页面从未加载。
 *
 *   node tools/cdp_probe.mjs dump <URL关键字>
 *       打印该页面的 readyState、根节点内容长度、body 背景色、可见文字 ——
 *       「窗口是白的」这类问题一眼就能判：背景色不是预期的深色 / 内容长度是 0。
 *
 *   node tools/cdp_probe.mjs eval <URL关键字> <JS表达式>
 *       在页面里求值（自动等 Promise，返回值按值回传）。表达式里可以直接调后端：
 *         window.__TAURI_INTERNALS__.invoke('menu_items')
 *       判断某条命令是不是卡住了，套一层超时即可（不会真的把进程拖住）：
 *         Promise.race([window.__TAURI_INTERNALS__.invoke('toggle_debug_window')
 *           .then(v => 'RET:' + v), new Promise(r => setTimeout(() => r('TIMEOUT'), 5000))])
 *
 *   node tools/cdp_probe.mjs shot <URL关键字> <输出.png>
 *       把页面当前画面存成 PNG。想看动画的某一帧时，先用 eval 把动画冻住：
 *         eval localhost:1420 "(() => { const el = document.querySelector('.pet-motion');
 *           const a = el.getAnimations()[0]; a.pause(); a.currentTime = 500; return a.animationName })()"
 *       然后 shot 出去看。没有屏幕时，「这一帧是不是真的翻过去了」就变成一张能看的图。
 *
 * 依赖：Node ≥ 22（用内置的全局 WebSocket，不需要任何 npm 包）。
 */

import { writeFileSync } from 'node:fs'

const PORT = process.env.CDP_PORT ?? '9222'
const ENDPOINT = `http://127.0.0.1:${PORT}/json`
const EVAL_TIMEOUT_MS = 10_000

const USAGE = `用法:
  node tools/cdp_probe.mjs list
  node tools/cdp_probe.mjs dump <URL关键字>
  node tools/cdp_probe.mjs eval <URL关键字> <JS表达式>
  node tools/cdp_probe.mjs shot <URL关键字> <输出.png>

应用要用远程调试端口启动:
  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=${PORT}" npm run tauri dev`

async function fetchTargets() {
  let res
  try {
    res = await fetch(ENDPOINT)
  } catch {
    // 这是最常见的失败：忘了带远程调试端口启动，或者应用已经退出了
    throw new Error(
      `连不上 ${ENDPOINT}。\n` +
        '多半是应用没带远程调试端口启动，或者已经退出了。\n\n' +
        `  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=${PORT}" npm run tauri dev`,
    )
  }
  if (!res.ok) throw new Error(`${ENDPOINT} 返回 HTTP ${res.status}`)
  return res.json()
}

/** 按 URL 选页面：先找完全相同的，再退回子串匹配。
 *
 * 顺序很重要：角色窗口是 `http://localhost:1420/`，调试窗口是 `.../debug.html`，
 * 拿 `localhost:1420` 做子串会把两个都匹配上，再取到哪个就看你运气了。
 * 匹配到多个时明确提示，不让人以为自己问的是角色窗口、实际问的却是调试窗口。
 */
function pickTarget(targets, filter) {
  const pages = targets.filter((t) => t.type === 'page')

  const exact = pages.find((t) => t.url === filter)
  if (exact) return exact

  const hits = pages.filter((t) => t.url.includes(filter))
  if (hits.length > 1) {
    console.error(
      `提示: "${filter}" 匹配到 ${hits.length} 个页面，本次用第一个：${hits[0].url}\n` +
        '      要精确指定请给完整 URL（例如 http://localhost:1420/）。\n',
    )
  }
  if (hits.length > 0) return hits[0]

  const available = pages.map((t) => `  ${t.title}  ${t.url}`).join('\n')
  throw new Error(`没有 URL 含 "${filter}" 的页面。当前页面:\n${available || '  (一个都没有)'}`)
}

/** 在指定页面里求值。返回值的类型不限，由调用方决定怎么打印。 */
function evaluate(wsUrl, expression) {
  return send(wsUrl, 'Runtime.evaluate', {
    // awaitPromise：表达式返回 Promise 时等它 settle，而不是回一个 Promise 对象
    // returnByValue：把结果序列化回来，否则只能拿到一个 objectId
    expression,
    awaitPromise: true,
    returnByValue: true,
  }).then((msg) => {
    const details = msg.result?.exceptionDetails
    if (details) {
      const text = details.exception?.description ?? details.text ?? JSON.stringify(details)
      throw new Error(`页面里抛出异常:\n${text}`)
    }
    return msg.result?.result?.value
  })
}

/** 往页面发一条 CDP 命令，把 result 原样返回（求值用 Runtime.evaluate，截图用 Page.*）。 */
function send(wsUrl, method, params) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(wsUrl)
    const id = 1
    const timer = setTimeout(() => {
      ws.close()
      reject(
        new Error(
          `CDP 超时（${EVAL_TIMEOUT_MS}ms）。表达式里如果调了后端命令，` +
            '用 Promise.race 自己加超时，才能看出「命令卡住」而不是「工具卡住」。',
        ),
      )
    }, EVAL_TIMEOUT_MS)

    const finish = (fn, arg) => {
      clearTimeout(timer)
      try {
        ws.close()
      } catch {
        /* 关不关得上都不影响结果 */
      }
      fn(arg)
    }

    ws.addEventListener('open', () => {
      ws.send(JSON.stringify({ id, method, params }))
    })

    ws.addEventListener('message', (event) => {
      const msg = JSON.parse(event.data)
      if (msg.id !== id) return
      if (msg.error) return finish(reject, new Error(JSON.stringify(msg.error)))
      finish(resolve, msg)
    })

    ws.addEventListener('error', () => {
      finish(
        reject,
        new Error(
          `连接断开: ${wsUrl}\n` +
            '如果这个表达式本身就是要关窗口或退出应用，断开是正常的 —— 页面已经没了，\n' +
            '拿不到返回值。用 list 看窗口在不在即可确认结果。',
        ),
      )
    })
  })
}

/**
 * 截图：把页面当前画面写成 PNG。
 *
 * 为什么需要它：动画「看起来对不对」以前只能靠人眼，而这里没有屏幕。
 * 配合 `getAnimations()[0].pause()` + `currentTime = X` 可以把动画**冻在某一帧**上，
 * 于是「翻转动作在第 500ms 是不是真的侧身了」变成一张能看的图，而不是一个推理。
 */
async function screenshot(wsUrl) {
  const msg = await send(wsUrl, 'Page.captureScreenshot', { format: 'png' })
  const data = msg.result?.data
  if (!data) throw new Error(`截图失败: ${JSON.stringify(msg)}`)
  return Buffer.from(data, 'base64')
}

/** 一条命令问完「这个窗口现在长什么样」——最常用的检查，所以做成子命令 */
const DUMP_EXPRESSION = `JSON.stringify({
  ready: document.readyState,
  appLen: (document.querySelector('#app') || document.body).innerHTML.length,
  bg: getComputedStyle(document.body).backgroundColor,
  buttons: document.querySelectorAll('button').length,
  text: document.body.innerText.replace(/\\n+/g, ' | ').slice(0, 300)
})`

async function main() {
  const [cmd, ...args] = process.argv.slice(2)

  if (!cmd || cmd === '-h' || cmd === '--help') {
    console.log(USAGE)
    return 0
  }

  const targets = await fetchTargets()

  if (cmd === 'list') {
    const pages = targets.filter((t) => t.type === 'page')
    if (pages.length === 0) console.log('(没有任何页面)')
    for (const t of pages) console.log(`${t.title}\t${t.url}`)
    return 0
  }

  if (cmd === 'dump') {
    const [filter] = args
    if (!filter) throw new Error('dump 需要 URL 关键字，例如: dump debug.html')
    const target = pickTarget(targets, filter)
    const raw = await evaluate(target.webSocketDebuggerUrl, DUMP_EXPRESSION)
    console.log(JSON.stringify(JSON.parse(raw), null, 2))
    return 0
  }

  if (cmd === 'eval') {
    const [filter, expression] = args
    if (!filter || !expression) {
      throw new Error('eval 需要 URL 关键字和表达式，例如:\n' +
        `  eval debug.html "document.title"\n` +
        `  eval localhost:1420 "window.__TAURI_INTERNALS__.invoke('menu_items')"`)
    }
    const target = pickTarget(targets, filter)
    const value = await evaluate(target.webSocketDebuggerUrl, expression)
    console.log(typeof value === 'string' ? value : JSON.stringify(value, null, 2))
    return 0
  }

  if (cmd === 'shot') {
    const [filter, out] = args
    if (!filter || !out) {
      throw new Error('shot 需要 URL 关键字和输出文件，例如:\n' +
        '  shot localhost:1420 C:\\Temp\\pet.png')
    }
    const target = pickTarget(targets, filter)
    const png = await screenshot(target.webSocketDebuggerUrl)
    writeFileSync(out, png)
    console.log(`${out}\t${png.length} 字节`)
    return 0
  }

  throw new Error(`未知子命令: ${cmd}\n\n${USAGE}`)
}

main()
  .then((code) => process.exit(code))
  .catch((err) => {
    console.error(`错误: ${err.message}`)
    process.exit(1)
  })
