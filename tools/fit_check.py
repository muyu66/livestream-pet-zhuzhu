"""检查动作库：配置里的动作名对得上吗？每个动作会越出角色框多少像素？

    python -I tools/fit_check.py [--stage 160] [--window 160x160] [--samples 1200]

角色框和窗口两个数默认从 `themes/*/theme.json` 的 `stage` 与
`src-tauri/tauri.conf.json` 的窗口尺寸里读 —— 两者**必须相等**（DEV.md §3.1），
让脚本自己读就不可能出现「参数写的是新尺寸、文件里还是旧尺寸」这种自欺欺人，
两个数不相等时它也会直接指出来。

它干两件事：

**一、校验配置**（这部分不看像素，只看名字和数字）
  - 每个表情序列里的动作名，在 `motions` 里存在吗？
  - 每个动作名在 CSS 里有同名的 `@keyframes pet-<动作>` 和 `.pet-motion.is-<动作>` 吗？
  - 序列里相邻两步同名吗（含循环列表首尾相接的那一对）？同名的话类名没变，
    CSS 动画不会重播，那一步看起来是「停着不动」。
  - 每条关键帧的首尾是不是中性姿态？动作演完到下一步之间可能差一帧，
    停在半路会闪一下。
  - 缓动函数过冲吗（`cubic-bezier` 的第二个/第四个数 > 1）？过冲会让中间某一刻的
    位移超过关键帧上写的值，而下面这个几何检查只按关键帧取值 ——
    那样它报的数就悄悄变成下限了。
  - 有没有定义了却没人用的动作（只提示，不算错）。

**二、算越界**（把变换真的作用到角色轮廓上）
  和旧版不同的是：动作现在是**共用**的，任何表情都可能用任何动作，
  所以每个动作要跟**所有表情图**都比一遍，取最坏的那张 —— 只按「自己的」表情算
  会漏掉最坏情况。做法是把每条关键帧的变换作用到角色轮廓（alpha > 8）上，
  取变换后的包围盒再和角色框比对。比手算靠谱：`transform-origin: 50% 88%`
  意味着放大是「往下扎根往上长」，越靠上的像素被推得越远，不用轮廓算不出来。

变换链（见 src/anim/animations.css）：

    img → .pet-breathe(常驻呼吸缩放) → .pet-motion(动作关键帧) → .pet-stage

三层共用同一个原点 (50%, 88%)，所以合并成一个式子：

    p_stage = O + T + Rot(r) · (b · (p_img·s + 居中偏移 - O))

其中 s 是 object-fit: contain 的缩放（图会被居中放进角色框，方框对正图时偏移为 0），
b 是呼吸缩放（动作和呼吸各自独立循环，相位可能撞在一起，按最坏情况取），
等比缩放写在关键帧里的 `scale()`，非等比的（squash / jump 的压扁拉长）写
`scaleX()` / `scaleY()`，所以这里缩放是逐分量的。

CSS 里 `transform: translate() rotate() scale()` 是从右往左作用到点上的，
所以要的是 `T + Rot(k(p - O))`，不是 `R·k·p + T`。

角色在窗口里是**居中**的（App.vue 的 .pet-app），所以两侧可用余量相等，
能留多少由越界最大的那个方向决定；四个方向的越界互不通用。

**注意**：窗口与角色框等大是当前的设计，不是漏改 —— 越界的部分就是会被切掉
（见 DEV.md §3.1）。所以这里报的是「切掉多少」，而不是「够不够」。
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
CSS = ROOT / "src" / "anim" / "animations.css"
THEME_DIR = ROOT / "themes" / "zhuzhu"
THEME = THEME_DIR / "theme.json"
TAURI_CONF = ROOT / "src-tauri" / "tauri.conf.json"
# 在 88% 高度处变换：放大时下方像素几乎不动，上方像素被推得最远
ORIGIN_Y = 0.88
# 常驻呼吸层的缩放幅度：关键帧里没给 --pet-breath 时的默认值（见 animations.css）
AMBIENT_BREATH = "0.02"

NUMERIC = re.compile(r"^[0-9+\-*/(). ]+$")
COMMENT = re.compile(r"/\*.*?\*/", re.S)


def strip_comments(css_text: str) -> str:
    """去掉 CSS 注释再解析。

    animations.css 的注释里就写着 `cubic-bezier(a, b, c, d)` 当例子，
    不 strip 的话缓动检查会把它当成真的缓动函数，`float("a")` 直接把脚本弄崩。
    """
    return COMMENT.sub("", css_text)


def find_call(value: str, fn: str) -> str | None:
    """取出 `fn(...)` 的括号内容，按括号配对而不是找第一个 ')'。

    因为里面可能套着 `calc()` 和 `var()`（例如
    `scale(calc(1 + var(--pet-breath, 0.02)))`），
    用 `[^)]*` 会在第一个 ')' 处截断。
    """
    at = value.find(fn + "(")
    if at < 0:
        return None
    depth = 0
    for i in range(at + len(fn), len(value)):
        if value[i] == "(":
            depth += 1
        elif value[i] == ")":
            depth -= 1
            if depth == 0:
                return value[at + len(fn) + 1 : i]
    return None


def to_num(value: str) -> float:
    value = value.strip()
    for unit in ("px", "deg", "ms", "s", "%"):
        if value.endswith(unit):
            return float(value[: -len(unit)])
    return float(value)


def eval_expr(expr: str) -> float:
    expr = expr.strip()
    if expr.startswith("calc(") and expr.endswith(")"):
        expr = expr[len("calc(") : -1].strip()
    if not NUMERIC.match(expr):
        # 带单位的裸值，如 0deg / -26px（单位由调用处的函数名决定，这里只取值）
        return to_num(expr)
    return float(eval(expr, {"__builtins__": {}}, {}))  # noqa: S307 - 上面已限定字符集


def substitute(expr: str, params: dict[str, str], defaults: dict[str, str]) -> float:
    """把 var(--pet-x, 默认值) / calc(...) 换成数字。"""

    def repl(match: re.Match[str]) -> str:
        key, fallback = match.group(1), match.group(2)
        return str(to_num(params.get(key, defaults.get(key, fallback))))

    return eval_expr(re.sub(r"var\(--pet-([a-z]+),\s*([^)]+)\)", repl, expr))


def split_args(text: str) -> list[str]:
    """按顶层逗号切分。`var(--pet-drift, 14px)` 里的逗号不算分隔符。"""
    parts, depth, current = [], 0, []
    for ch in text:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch == "," and depth == 0:
            parts.append("".join(current))
            current = []
            continue
        current.append(ch)
    parts.append("".join(current))
    return parts


def parse_transform(value: str, params: dict[str, str]) -> tuple[float, float, float, float, float]:
    """一条 `transform:` 的值 → (dx, dy, 旋转角度, scaleX, scaleY)。

    等比缩放写在 `scale()` 里，非等比的写 `scaleX()` / `scaleY()`；两者都不写就是 1。
    """
    dx = dy = rot = 0.0
    kx = ky = 1.0

    tr = find_call(value, "translate3d")
    if tr:
        parts = split_args(tr)
        dx = substitute(parts[0], params, {})
        dy = substitute(parts[1], params, {})

    rt = find_call(value, "rotate")
    if rt is not None:
        rot = substitute(rt, params, {})

    sx = find_call(value, "scaleX")
    sy = find_call(value, "scaleY")
    if sx is not None or sy is not None:
        kx = substitute(sx, params, {}) if sx is not None else 1.0
        ky = substitute(sy, params, {}) if sy is not None else 1.0
    else:
        sc = find_call(value, "scale")
        if sc is not None:
            kx = ky = substitute(sc, params, {})

    return dx, dy, rot, kx, ky


def parse_keyframes(css_text: str, motion: str, params: dict[str, str]) -> list[tuple[float, tuple]]:
    """抽出某个动作的 [ (百分比, (dx, dy, 旋转, scaleX, scaleY)) ]。

    注意**只取要的那一个关键帧块**：CSS 里每个动作的 var 默认值是为它自己写的，
    拿别的动作的参数去代入会算出完全错误的位移（早先整份 CSS 一起解析、
    结果所有动作都被最后解析的那个动作的参数污染）。

    `pet-breathe-ambient` 是常驻呼吸层，不属于动作库 —— 它一直在演、不参与序列，
    这里按 worst case 单独代入（见 call site）。
    """
    block = None
    for name, body in re.findall(r"@keyframes\s+pet-([a-z-]+)\s*\{(.*?)\n\}", css_text, re.S):
        if name == motion:
            block = body
            break
    if block is None:
        return []

    frames = []
    for head, body in re.findall(r"([\d.]+%|\bfrom\b|\bto\b)\s*\{(.*?)\}", block, re.S):
        pct = {"from": 0.0, "to": 100.0}.get(head, to_num(head) if head.endswith("%") else 0.0)
        tm = re.search(r"transform:\s*([^;]+);", body, re.S)
        if not tm:
            continue
        frames.append((pct, parse_transform(tm.group(1), params)))
    return frames


def silhouette_points(path: Path, samples: int) -> tuple[np.ndarray, tuple[int, int]]:
    """角色轮廓上的点（图像坐标系，像素中心）+ 图像尺寸。"""
    with Image.open(path) as handle:
        image = handle.convert("RGBA")
        size = image.size
        alpha = np.asarray(image)[..., 3]
    ys, xs = np.nonzero(alpha > 8)
    if len(xs) == 0:
        raise SystemExit(f"{path} 是空的")
    step = max(1, len(xs) // samples)
    return np.stack([xs[::step], ys[::step]], axis=1).astype(np.float64), size


def extremes(
    pts: np.ndarray,
    img_size: tuple[int, int],
    frames: list[tuple[float, tuple]],
    breath: float,
    stage_w: int,
    stage_h: int,
) -> tuple[tuple[float, float, float, float], str]:
    """这个轮廓在所有关键帧下越出角色框多少：返回 (左, 上, 右, 下) 与最坏的那一帧。"""
    img_w, img_h = img_size
    # object-fit: contain —— 取较小的那个比例，图居中放进角色框
    s = min(stage_w / img_w, stage_h / img_h)
    offset = np.array([(stage_w - img_w * s) / 2, (stage_h - img_h * s) / 2])
    # transform-origin: 50% 88%，是相对 .pet-motion（也就是角色框）算的
    o = np.array([stage_w / 2, stage_h * ORIGIN_Y])
    base = pts * s + offset - o

    best = (0.0, 0.0, 0.0, 0.0)
    at = ""
    # 呼吸缩放与动作缩放相乘；两者各自独立循环，相位可能恰好撞在一起，按最坏情况取
    for b in (1.0, 1.0 + breath):
        for pct, (dx, dy, rot, kx, ky) in frames:
            rad = np.deg2rad(rot)
            c, sn = np.cos(rad), np.sin(rad)
            v = base * np.array([kx * b, ky * b])
            x = v[:, 0] * c - v[:, 1] * sn
            y = v[:, 0] * sn + v[:, 1] * c
            px = o[0] + dx + x
            py = o[1] + dy + y
            clip = (
                max(0.0, -px.min()),  # 左越界
                max(0.0, -py.min()),  # 上越界
                max(0.0, px.max() - stage_w),  # 右越界
                max(0.0, py.max() - stage_h),  # 下越界
            )
            if max(clip) > max(best):
                best = clip
                at = f"{pct:g}% 呼吸x{b:.3f} 位移({dx:g},{dy:g}) 转{rot:g}deg 缩放({kx:g},{ky:g})"
    return best, at


def check_config(
    theme: dict, css_text: str
) -> tuple[list[str], list[str]]:
    """返回 (错误, 提示)。错误会让脚本以非 0 退出。"""
    errors: list[str] = []
    notes: list[str] = []

    motions: dict[str, dict] = {
        name: cfg for name, cfg in (theme.get("motions") or {}).items() if not name.startswith("_")
    }
    expressions: dict[str, dict] = {
        name: cfg
        for name, cfg in (theme.get("expressions") or {}).items()
        if not name.startswith("_")
    }

    keyframes = set(re.findall(r"@keyframes\s+pet-([a-z-]+)\s*\{", css_text))
    bound = set(re.findall(r"\.pet-motion\.is-([a-z-]+)\s*\{", css_text))

    # 动作名 = 关键帧后缀 = is-<名> 的规则，三者必须一致
    for name, cfg in motions.items():
        if not re.fullmatch(r"[a-z][a-z-]*", name):
            errors.append(f'动作名 "{name}" 不合法：只允许小写字母和短横线')
        if name not in keyframes:
            errors.append(f'动作 "{name}" 没有 @keyframes pet-{name}')
        if name not in bound:
            errors.append(f'动作 "{name}" 没有 .pet-motion.is-{name} 规则')

    # 光有规则不够：规则里 animation 的**第一个词**才是真正被播放的关键帧名
    # （第三项曾在 `1 both` 换成 `名字 var(--pet-duration) …` 时暴露过这个破洞：
    #  is-breathe 写成了 pet-breathe-deep，那条规则整条被浏览器丢掉 ——
    #  CSS 语法没错、控制台不吭声，动作就是不动，而上面两条检查全绿）
    for name, body in re.findall(r"\.pet-motion\.is-([a-z-]+)\s*\{([^}]*)\}", css_text):
        match = re.search(r"animation\s*:\s*([^\s;]+)", body)
        if not match:
            errors.append(f"规则 .pet-motion.is-{name} 里没有 animation")
            continue
        used = match.group(1)
        if used != f"pet-{name}":
            errors.append(
                f'规则 .pet-motion.is-{name} 播的是 "{used}"，不是 "pet-{name}"：'
                "名字对不上，这条规则会被浏览器整条丢掉，那一步看着完全不动"
            )
        if used.startswith("pet-") and used[4:] not in keyframes:
            errors.append(f'规则 .pet-motion.is-{name} 引用的 "{used}" 没有对应的 @keyframes')
        duration = cfg.get("durationMs")
        if not isinstance(duration, (int, float)) or duration <= 0:
            errors.append(f'动作 "{name}" 的 durationMs 不是正数：{duration!r}')
        for key in (cfg.get("params") or {}):
            if key.startswith("_"):
                errors.append(f'动作 "{name}" 的 params 里有注释键 "{key}"：那是"数据"，会被当成 CSS 变量名')

    # 关键帧里出现了却没登记的动作（多半是改名只改了一半）
    for name in sorted(keyframes - set(motions) - {"breathe-ambient"}):
        notes.append(f'CSS 里有 @keyframes pet-{name}，但 motions 里没登记它（没人能用）')

    used: set[str] = set()
    for name, cfg in expressions.items():
        playlist = cfg.get("motions") or []
        if not playlist:
            errors.append(f'表情 "{name}" 一个动作都没有')
        for motion in playlist:
            if motion not in motions:
                errors.append(
                    f'表情 "{name}" 用到了动作 "{motion}"，但 motions 里没有它'
                    f'（available: {" / ".join(sorted(motions))}）'
                )
            used.add(motion)

        for i, motion in enumerate(playlist):
            previous = playlist[-1] if (i == 0 and cfg.get("loop")) else (playlist[i - 1] if i else None)
            if previous == motion:
                where = "首尾相接处" if i == 0 else "相邻两步"
                errors.append(
                    f'表情 "{name}" 的{where}都是动作 "{motion}"：类名没变，CSS 动画不会重播，'
                    "那一步看起来是停着不动的。中间插一个 hold，或换成别的动作。"
                )

    # 首尾必须是中性姿态：动作演完到下一步之间可能差一帧，停在半路会闪一下
    for name, cfg in motions.items():
        frames = parse_keyframes(css_text, name, cfg.get("params") or {})
        if not frames:
            continue
        for pct, (dx, dy, rot, kx, ky) in frames:
            if pct not in (0.0, 100.0):
                continue
            if max(abs(dx), abs(dy), abs(rot), abs(kx - 1), abs(ky - 1)) > 1e-6:
                errors.append(
                    f'动作 "{name}" 的 {pct:g}% 不是中性姿态（位移/旋转/缩放不为 0/1）：'
                    "序列每次换步都从这一帧开始或结束，角色会闪一下"
                )

    for motion in sorted(set(motions) - used):
        notes.append(f'动作 "{motion}" 定义了但没有表情用它')

    return errors, notes


def check_easing(css_text: str) -> list[str]:
    """缓动函数不许过冲 —— 过冲会让几何检查报出的越界变成下限。

    `cubic-bezier(x1, y1, x2, y2)` 的 y1 / y2 是输出值，> 1 就意味着中间某一刻
    会超过这段起止值；而几何检查只按关键帧取值，算不到那段过冲。
    """
    errors = []
    for whole in re.findall(r"cubic-bezier\(([^)]*)\)", css_text):
        numbers = [float(v) for v in whole.split(",")]
        if len(numbers) != 4:
            continue
        if numbers[1] > 1 or numbers[3] > 1:
            errors.append(
                f"缓动 cubic-bezier({whole}) 的 y 超过 1，会过冲："
                "几何检查只按关键帧取值，多出来的那一截它会漏算。"
                "余弹请用多加关键帧实现（见 hop），别用缓动过冲。"
            )
    return errors


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--stage", type=int, default=None, help="角色框边长，默认读 theme.json 的 stage")
    ap.add_argument("--window", default=None, help="窗口尺寸 WxH，默认读 tauri.conf.json")
    ap.add_argument("--samples", type=int, default=1200, help="每张图取多少个轮廓点")
    args = ap.parse_args(argv)

    theme = json.loads(THEME.read_text(encoding="utf-8"))
    stage = theme["stage"]
    stage_w = args.stage or stage["width"]
    stage_h = args.stage or stage["height"]

    if args.window:
        win_w, win_h = (int(v) for v in args.window.lower().split("x"))
        win_from = "--window"
    else:
        windows = json.loads(TAURI_CONF.read_text(encoding="utf-8"))["app"]["windows"]
        main_window = next(w for w in windows if w["label"] == "main")
        win_w, win_h = main_window["width"], main_window["height"]
        win_from = "tauri.conf.json"

    css_text = strip_comments(CSS.read_text(encoding="utf-8"))

    # 输出只用 ASCII 与常用中文：控制台默认是 GBK，箭头/对勾这类符号会直接
    # UnicodeEncodeError 崩掉（⇒ 就崩过一次），排查脚本不该死在打印上。
    print(f"角色框 {stage_w}x{stage_h}（theme.json）"
          f"  窗口 {win_w}x{win_h}（{win_from}）  原点 y={ORIGIN_Y:.0%}")

    errors, notes = check_config(theme, css_text)
    errors += check_easing(css_text)
    if errors:
        print(f"\n配置有问题（{len(errors)} 处）：")
        for line in errors:
            print(f"  - {line}")
    if notes:
        print(f"\n提示（{len(notes)} 条，不算错）：")
        for line in notes:
            print(f"  - {line}")

    motions = {
        name: cfg for name, cfg in (theme.get("motions") or {}).items() if not name.startswith("_")
    }
    expressions = {
        name: cfg
        for name, cfg in (theme.get("expressions") or {}).items()
        if not name.startswith("_")
    }

    # 每个动作只跟「**真的会用到它的**那些表情」的图比 —— 动作虽然共用（谁都能写进序列），
    # 但实际会被切掉多少，取决于此刻配出来的那些组合。
    # 拿所有图无脑叉乘会虚报：比如 hop 根本没被笑那个表情用，按笑的图算出来的数
    # 谁也看不见，却会让人误以为 hop 的幅度得调小。
    # 代价是「以后把某个动作加进别的表情」时要重跑一次 —— 上面那句提示会说明这点。
    users: dict[str, list[str]] = {name: [] for name in motions}
    for name, cfg in expressions.items():
        for motion in cfg.get("motions") or []:
            if motion in users:
                users[motion].append(name)

    silhouettes: dict[str, tuple[np.ndarray, tuple[int, int]]] = {}
    base_path = THEME_DIR / theme["assets"]["base"]

    def silhouette(expression: str) -> tuple[np.ndarray, tuple[int, int]] | None:
        """按需加载某个表情的轮廓（同一张图只读一次）"""
        if expression in silhouettes:
            return silhouettes[expression]
        cfg = expressions.get(expression, {})
        path = THEME_DIR / (cfg.get("image") or theme["assets"]["base"])
        if not path.exists():
            print(f"\n表情 {expression} 的图 {path.name} 不存在（运行时它会回退到基础图）")
            path = base_path
        if not path.exists():
            return None
        silhouettes[expression] = silhouette_points(path, args.samples)
        return silhouettes[expression]

    print("\n下表是每个动作的轮廓越出角色框多少像素 —— 窗口与角色框等大时，这就是会被切掉的量。")
    print("每个动作只按「会用到它的表情」的图来算，所以报告的是真的会上屏的那种组合。")
    print("把某个动作加进别的表情之后，它的数可能会变，重跑一次即可。\n")

    header = f"{'动作':<10}{'左':>7}{'上':>7}{'右':>7}{'下':>7}   表情 / 最坏关键帧"
    print(header)
    print("-" * len(header))

    worst = {"left": 0.0, "top": 0.0, "right": 0.0, "bottom": 0.0}
    for name, cfg in motions.items():
        params = cfg.get("params") or {}
        frames = parse_keyframes(css_text, name, params)
        if not frames:
            print(f"{name:<10} 找不到关键帧，跳过")
            continue
        breath = to_num(params.get("breath", AMBIENT_BREATH))

        # 没有表情在用这个动作时，只能按基础图估一个数（并标出来）
        tested = users[name] or ["<没人用，按 base.png 估>"]
        best = (0.0, 0.0, 0.0, 0.0)
        at = ""
        which = ""
        for expression in tested:
            loaded = silhouette(expression)
            if loaded is None:
                continue
            clip, frame = extremes(loaded[0], loaded[1], frames, breath, stage_w, stage_h)
            if max(clip) > max(best) or max(best) == 0:
                best = clip
                at = frame
                which = expression
        for key, value in zip(("left", "top", "right", "bottom"), best):
            worst[key] = max(worst[key], value)
        # 一点都不越界时不报「最坏的那一帧」：那时候谁都不越界，写出来只会让人以为
        # 那个表情/关键帧有什么特别之处
        detail = f"{which} / {at}" if max(best) > 0 else "完全不越界"
        print(f"{name:<10}{best[0]:>7.1f}{best[1]:>7.1f}{best[2]:>7.1f}{best[3]:>7.1f}"
              f"   {detail}")

    if not silhouettes:
        print("一张表情图都没读到，没法算越界")
        return 1

    print(f"\n合计最大越界：左 {worst['left']:.1f} 上 {worst['top']:.1f} "
          f"右 {worst['right']:.1f} 下 {worst['bottom']:.1f}")

    # 角色在窗口里居中，所以两侧可用余量相等；能留多少由越界最大的那个方向决定
    margin_x = max(worst["left"], worst["right"])
    margin_y = max(worst["top"], worst["bottom"])
    need_w = np.ceil(stage_w + 2 * margin_x)
    need_h = np.ceil(stage_h + 2 * margin_y)

    have_x = (win_w - stage_w) / 2
    have_y = (win_h - stage_h) / 2

    if win_w == stage_w and win_h == stage_h:
        print(f"窗口与角色框等大（{win_w}x{win_h}）—— 这是当前的设计（DEV.md §3.1），"
              "越界即被切掉，不是配置错误。")
    elif have_x <= 0 or have_y <= 0:
        print(f"窗口 {win_w}x{win_h} 比角色框 {stage_w}x{stage_h} 还小：角色会被整体裁掉，"
              "两边应该相等。")
    else:
        print(f"窗口 {win_w}x{win_h} 比角色框大，每边留出 左右{have_x:.1f}/上下{have_y:.1f}px"
              "（角色居中）—— 这圈余量看不见，但会接收鼠标事件，"
              "而且角色因此贴不到屏幕边角（见 DEV.md §3.1）。")

    # 真正会被切掉多少 = 越界量 - 可用余量。窗口偏大时这些数会是 0。
    clip = {
        "左": worst["left"] - have_x,
        "上": worst["top"] - have_y,
        "右": worst["right"] - have_x,
        "下": worst["bottom"] - have_y,
    }
    clipped = {side: value for side, value in clip.items() if value > 0.05}
    if not clipped:
        print("当前窗口下没有任何像素被切到（想一点都不切，窗口至少要 "
              f"{need_w:.0f}x{need_h:.0f}，现在就是够的）")
    else:
        detail = "、".join(f"{side} {value:.1f}px" for side, value in clipped.items())
        print(f"当前窗口下会被切掉：{detail}")
        print(f"不想切有两个办法：窗口加到至少 {need_w:.0f}x{need_h:.0f}"
              "（等于把余量加回来，见 DEV.md §3.1），或把对应动作的振幅调小"
              "（theme.json 的 motions —— 推荐这个）。")

    if errors:
        print(f"\n有 {len(errors)} 处配置问题，先修掉再跑（见上面的列表）。")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
