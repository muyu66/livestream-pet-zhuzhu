/**
 * 输入电平的显示换算。
 *
 * 调试面板与麦克风选择框共用这一份：两处都在回答同一个问题
 * ——「这只麦克风到底有没有在出信号」，换算方式当然也只能有一份。
 * 两个窗口的样式表是分开的（一个深色工具窗、一个小选择框），
 * 但「-60 dBFS 以下算静音」这种判断不该有两份。
 */

/** 线性幅度 → dBFS 文本。-60 以下按静音显示，避免 -∞ 刷屏 */
export function dbfs(amplitude: number): string {
  if (amplitude <= 0) return '-∞'
  return (20 * Math.log10(amplitude)).toFixed(1)
}

/** dBFS 映射到 0–100 的条形宽度，-60 dBFS 以下算空 */
export function levelPercent(amplitude: number): number {
  if (amplitude <= 0) return 0
  const db = 20 * Math.log10(amplitude)
  return Math.max(0, Math.min(100, ((db + 60) / 60) * 100))
}
