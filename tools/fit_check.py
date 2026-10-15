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
  - 同一个动作各关键帧的变换函数（`translate3d` / `rotate` / `rotateY` / `scale`…）
    名字与顺序一致吗？不一致时浏览器会放弃逐分量插值、改用矩阵插值，
    下面那个几何检查就不再是对的（它是按逐分量插值算的）。
  - 缓动函数认得出来吗？认不出来的（比如 `steps()`）直接报错 ——
    缓动是几何检查的一部分（要按它采样），看不懂就没法保证算出来的数是对的。
  - 有没有定义了却没人用的动作（只提示，不算错）。

**二、算越界**（把变换真的作用到角色轮廓上）
  动作是**共用**的，任何表情都可能用任何动作，所以每个动作要跟
  「**真的会用到它的**那些表情图」都比一遍（见下面 main 里的说明），取最坏的那张。
  做法是把变换作用到角色轮廓（alpha > 8）上，取变换后的包围盒再和角色框比对。
  比手算靠谱：`transform-origin: 50% 88%` 意味着放大是「往下扎根往上长」，
  越靠上的像素被推得越远，不用轮廓算不出来。

  采样的不只是关键帧：每两个关键帧**之间**还要按缓动曲线取一批中间姿态 ——
  弹性缓动会过冲（`linear(0, 1.05 56%, 1)` 那一段的实际位移比两端的关键帧都大），
  只看关键帧的话，越界量会悄悄变成下限。所以过冲的极值也是量出来的。

变换链（见 src/anim/animations.css）：

    img(朝向镜像) → .pet-breathe(常驻呼吸缩放) → .pet-motion(动作关键帧) → .pet-stage

三层共用同一个原点 (50%, 88%)，所以合并成一个式子：

    p_stage = O + Persp(T + Rot(r) · RotY(y) · (b · (p_img·s + 居中偏移 - O)))

其中 s 是 object-fit: contain 的缩放（图会被居中放进角色框，方框对正图时偏移为 0），
b 是呼吸缩放（动作和呼吸各自独立循环，相位可能撞在一起，按最坏情况取），
等比缩放写在关键帧里的 `scale()`，非等比的（squash / jump 的压扁拉长）写
`scaleX()` / `scaleY()`，所以这里缩放是逐分量的。
`RotY` 与 `Persp` 只有 3D 动作（flip）才有：`rotateY` 把点送进 z 深度，
`perspective(d)` 再按 `1/(1 - z/d)` 做透视除法 —— 近的一半变大、远的一半变小，
这就是翻转看起来有立体的原因。z 只由 x 决定，所以那个除法是逐点算得出来的。

`mirror`（表情的朝向开关）作用在**图**上：以角色框竖直中线为轴把 x 取反。
它先于动作生效，于是「镜像之后再缩放」等价于「把 scaleX 取反」——
这里直接对轮廓做镜像，比改关键帧参数更不容易看错。

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
from functools import lru_cache
from pathlib import Path
from typing import NamedTuple

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


class Easing(NamedTuple):
    """一条缓动曲线。

    `at(t)`：输入进度 0~1 → 输出值（**可以 < 0 或 > 1**，那正是弹性缓动的过冲）。
    `peaks`：输出极值出现在哪些输入进度上（`linear()` 是分段直线，极值一定在折点上）。
    几何检查按这两个采样，所以过冲的部分也是量出来的，不是估的。
    """

    text: str
    at: object  # Callable[[float], float]
    peaks: tuple[float, ...]


def bezier_at(x1: float, y1: float, x2: float, y2: float, t: float) -> float:
    """`cubic-bezier(x1,y1,x2,y2)` 在输入进度 t 处的输出值。

    x(u) 在 0~1 上单调（x1、x2 被限定在 0~1），所以先二分把参数 u 解出来，再取 y(u) ——
    这就是浏览器干的事。y1 / y2 大于 1 时 y(u) 会超过 1，也就是过冲。
    """
    lo, hi = 0.0, 1.0
    for _ in range(30):
        u = (lo + hi) / 2
        x = 3 * (1 - u) ** 2 * u * x1 + 3 * (1 - u) * u * u * x2 + u**3
        if x < t:
            lo = u
        else:
            hi = u
    u = (lo + hi) / 2
    return 3 * (1 - u) ** 2 * u * y1 + 3 * (1 - u) * u * u * y2 + u**3


EASE_KEYWORDS = {
    "linear": (0.0, 0.0, 1.0, 1.0),
    "ease": (0.25, 0.1, 0.25, 1.0),
    "ease-in": (0.42, 0.0, 1.0, 1.0),
    "ease-out": (0.0, 0.0, 0.58, 1.0),
    "ease-in-out": (0.42, 0.0, 0.58, 1.0),
}


@lru_cache(maxsize=None)
def parse_easing(text: str) -> Easing:
    """把 CSS 里的缓动函数解析成一个可求值的曲线。认不出来就抛 ValueError。

    **认不出来必须报错，不能默默当成 linear**：缓动决定两帧之间的实际位移，
    弹性缓动还会让中间某一刻超出两端 —— 曲线读错，下面算出来的越界就是错的，
    而错的数字比没有数字更糟（人会照着它去调幅度）。
    """
    text = text.strip()
    key = text.lower()

    if not key:
        return parse_easing("ease")  # 省略缓动时 CSS 默认是 ease

    if key in EASE_KEYWORDS:
        x1, y1, x2, y2 = EASE_KEYWORDS[key]
        return Easing(text, lambda t, a=(x1, y1, x2, y2): bezier_at(*a, t), ())

    if key.startswith("cubic-bezier("):
        parts = split_args(find_call(text, "cubic-bezier") or "")
        if len(parts) != 4:
            raise ValueError(f'缓动 "{text}" 不是 4 个数字')
        try:
            coords = [float(v) for v in parts]
        except ValueError as err:
            raise ValueError(f'缓动 "{text}" 里不是 4 个数字') from err
        x1, y1, x2, y2 = coords
        if not (0.0 <= x1 <= 1.0 and 0.0 <= x2 <= 1.0):
            raise ValueError(f'缓动 "{text}" 的 x1/x2 必须在 0~1 之间（x 必须是单调的）')
        return Easing(text, lambda t: bezier_at(x1, y1, x2, y2, t), ())

    if key.startswith("linear("):
        stops: list[tuple[float | None, float]] = []
        for part in split_args(find_call(text, "linear") or ""):
            bits = part.split()
            if not bits or len(bits) > 2:
                raise ValueError(
                    f'缓动 "{text}" 里这一段 "{part.strip()}" 看不懂：'
                    "只支持 `值` 或 `值 位置%` 两种写法（不支持一个值挂两个位置）"
                )
            position = None
            if len(bits) == 2:
                position = to_num(bits[1]) / 100.0 if bits[1].endswith("%") else float(bits[1])
            try:
                stops.append((position, float(bits[0])))
            except ValueError as err:
                raise ValueError(f'缓动 "{text}" 里的 "{bits[0]}" 不是数字') from err
        if len(stops) < 2:
            raise ValueError(f'缓动 "{text}" 至少要有两个停靠点')

        # 没写位置的停靠点：首尾按 0% / 100%，中间的在与它相邻的两个已知位置之间等距
        known = [i for i, (position, _) in enumerate(stops) if position is not None]
        if not known:
            span = len(stops) - 1
            stops = [(i / span, value) for i, (_, value) in enumerate(stops)]
        else:
            if stops[0][0] is None:
                stops[0] = (0.0, stops[0][1])
            if stops[-1][0] is None:
                stops[-1] = (1.0, stops[-1][1])
            previous = 0
            for i in range(1, len(stops)):
                if stops[i][0] is None:
                    continue
                gap = i - previous
                for j in range(1, gap):
                    between = stops[previous][0] + (stops[i][0] - stops[previous][0]) * j / gap
                    stops[previous + j] = (between, stops[previous + j][1])
                previous = i

        ordered = [(position, value) for position, value in stops]
        positions = [position for position, _ in ordered]

        def at(t: float, curve=tuple(ordered)) -> float:
            if t <= curve[0][0]:
                return curve[0][1]
            for (p0, v0), (p1, v1) in zip(curve, curve[1:]):
                if t <= p1:
                    if p1 == p0:
                        return v1
                    return v0 + (v1 - v0) * (t - p0) / (p1 - p0)
            return curve[-1][1]

        return Easing(text, at, tuple(positions))

    raise ValueError(
        f'认不出缓动函数 "{text}"：几何检查要按缓动采样，认不出来就没法保证算出来的数是对的。'
        "支持 cubic-bezier() / linear() / linear / ease / ease-in / ease-out / ease-in-out"
    )


class Xform(NamedTuple):
    """一帧 `transform:` 的值（按 CSS 的书写顺序）。

    `persp` 是 `perspective()` 的距离，没写就是 None（纯 2D）；
    `rot_y` 只在 3D 翻转里用得上。
    """

    dx: float
    dy: float
    rot: float
    rot_y: float
    kx: float
    ky: float
    persp: float | None

    def is_neutral(self) -> bool:
        return (
            max(abs(self.dx), abs(self.dy), abs(self.rot), abs(self.rot_y),
                abs(self.kx - 1), abs(self.ky - 1)) <= 1e-6
        )


def parse_transform(value: str, params: dict[str, str]) -> Xform:
    """一条 `transform:` 的值 → Xform。

    等比缩放写在 `scale()` 里，非等比的写 `scaleX()` / `scaleY()`；两者都不写就是 1。
    `rotate()` 是平面内旋转，`rotateY()` 是绕纵轴的 3D 翻转，两者可以同时出现。
    """
    dx = dy = rot = rot_y = 0.0
    kx = ky = 1.0
    persp: float | None = None

    tr = find_call(value, "translate3d")
    if tr:
        parts = split_args(tr)
        dx = substitute(parts[0], params, {})
        dy = substitute(parts[1], params, {})

    rt = find_call(value, "rotate")
    if rt is not None:
        rot = substitute(rt, params, {})

    rty = find_call(value, "rotateY")
    if rty is not None:
        rot_y = substitute(rty, params, {})

    pp = find_call(value, "perspective")
    if pp is not None:
        distance = substitute(pp, params, {})
        # 透视距离必须为正，否则投影方向是反的（CSS 里 perspective(0) 等于不投影）
        persp = distance if distance > 1e-6 else None

    sx = find_call(value, "scaleX")
    sy = find_call(value, "scaleY")
    if sx is not None or sy is not None:
        kx = substitute(sx, params, {}) if sx is not None else 1.0
        ky = substitute(sy, params, {}) if sy is not None else 1.0
    else:
        sc = find_call(value, "scale")
        if sc is not None:
            kx = ky = substitute(sc, params, {})

    return Xform(dx, dy, rot, rot_y, kx, ky, persp)


def transform_functions(value: str) -> tuple[str, ...]:
    """这条 transform 里用到的函数名（按书写顺序）。

    浏览器只在**两个关键帧的变换函数序列完全一致**时才逐分量插值；
    不一致（比如一帧写 `scale()`、另一帧写 `scaleX() scaleY()`）会退化成矩阵插值，
    中间过程就完全不是这里算的那个了 —— 所以 main 里会检查每个动作内部是否一致。
    """
    names: list[str] = []
    depth = 0
    for match in re.finditer(r"[a-zA-Z][a-zA-Z0-9]*|[()]", value):
        token = match.group(0)
        if token == "(":
            depth += 1
        elif token == ")":
            depth -= 1
        elif depth == 0:
            names.append(token)
    return tuple(names)


class Frame(NamedTuple):
    """一个关键帧：百分比 + 姿态 + 「从这一帧到下一帧」用的缓动。

    `easing` 为空表示用规则里那条 animation 的缓动；写在这里（关键帧体内的
    `animation-timing-function`）只覆盖自己这一段 —— 弹性缓动就是靠这个逐段换的。
    """

    pct: float
    xform: Xform
    easing: str
    raw: str


def parse_keyframes(css_text: str, motion: str, params: dict[str, str]) -> list[Frame]:
    """抽出某个动作的关键帧列表。

    注意**只取要的那一个关键帧块**：CSS 里每个动作的 var 默认值是为它自己写的，
    拿别的动作的参数去代入会算出完全错误的位移（早先整份 CSS 一起解析、
    结果所有动作都被最后解析的那个动作的参数污染）。

    `pet-breathe-ambient` 是常驻呼吸层，不属于动作库 —— 它一直在演、不参与序列，
    这里按 worst case 单独代入（见 call site）。
    """
    block = keyframes_block(css_text, motion)
    if block is None:
        return []

    frames = []
    for head, body in re.findall(r"([\d.]+%|\bfrom\b|\bto\b)\s*\{(.*?)\}", block, re.S):
        pct = {"from": 0.0, "to": 100.0}.get(head, to_num(head) if head.endswith("%") else 0.0)
        tm = re.search(r"transform:\s*([^;]+);", body, re.S)
        if not tm:
            continue
        timing = re.search(r"animation-timing-function:\s*([^;]+);", body, re.S)
        frames.append(
            Frame(
                pct,
                parse_transform(tm.group(1), params),
                timing.group(1).strip() if timing else "",
                tm.group(1),
            )
        )
    return frames


def keyframes_block(css_text: str, motion: str) -> str | None:
    """`@keyframes pet-<motion>` 的块内容（找不到返回 None）。"""
    for name, body in re.findall(r"@keyframes\s+pet-([a-z-]+)\s*\{(.*?)\n\}", css_text, re.S):
        if name == motion:
            return body
    return None


def rule_easing(css_text: str, motion: str) -> str:
    """`.pet-motion.is-<动作>` 里 `animation:` 简写中的缓动函数。

    这一条是整个动作的默认缓动，每一段可以用关键帧里的 `animation-timing-function`
    单独覆盖 —— 所以要把两个地方都读进来（缓动决定两帧之间的实际位移，
    读漏了要么算错、要么报错得莫名其妙）。
    """
    match = re.search(r"\.pet-motion\.is-" + re.escape(motion) + r"\s*\{([^}]*)\}", css_text)
    if not match:
        return "ease"
    shorthand = re.search(r"animation\s*:\s*([^;}]+)", match.group(1))
    if not shorthand:
        return "ease"
    for token in split_args(shorthand.group(1)):
        token = token.strip()
        # 简写里逐项认：名字、时长、次数（1 / infinite）都不是缓动，剩下的才是
        if not token or re.fullmatch(r"[\d.]+m?s", token) or token in ("1", "infinite", "both",
                                                                       "forwards", "backwards",
                                                                       "none", "normal",
                                                                       "reverse", "alternate"):
            continue
        if re.fullmatch(r"[\d.]+", token):
            continue
        if token.startswith("pet-"):
            continue
        return token
    return "ease"


class State(NamedTuple):
    """采样出来的一个姿态（关键帧本身，或两帧之间按缓动算出来的中间姿态）。"""

    label: str
    xform: Xform


# 每两个关键帧之间采多少个点。弹性缓动的过冲极值也会被采到（`linear()` 的折点、
# 以及与折点重合的采样点），所以这个数只影响精度到小数点后两位那一位。
SAMPLE_STEPS = 32


def sampled_states(frames: list[Frame], default_easing: str) -> list[State]:
    """关键帧 → 一串姿态：每一帧，加上每段之间按缓动采出来的中间姿态。

    只看关键帧是不够的：弹性缓动会让中间某一刻的位移**超过两端**（过冲），
    那才是越界最大的时刻。这里把那段曲线采出来，过冲的极值和它出现在哪一段都留痕
    （标签里会写「缓动 1.05」），所以报出来的数不是下限。
    """
    states = [State(f"{frames[0].pct:g}%", frames[0].xform)]
    for first, second in zip(frames, frames[1:]):
        easing = parse_easing(first.easing or default_easing)
        span = second.pct - first.pct
        if span <= 0:
            continue
        # 均匀采样 + 折点（linear() 的过冲尖峰就在折点上，正好采到才不失真）
        inputs = {k / SAMPLE_STEPS for k in range(1, SAMPLE_STEPS)} | {
            peak for peak in easing.peaks if 0 < peak < 1
        }
        for t in sorted(inputs):
            progress = easing.at(t)
            state = State(
                f"{first.pct:g}%-{second.pct:g}%"
                + (f" 缓动{progress:.2f}" if progress < -1e-9 or progress > 1 + 1e-9 else ""),
                blend(first.xform, second.xform, progress),
            )
            states.append(state)
    if len(frames) > 1:
        states.append(State(f"{frames[-1].pct:g}%", frames[-1].xform))
    return states


def blend(first: Xform, second: Xform, progress: float) -> Xform:
    """两个姿态之间按进度取值（逐分量 —— 浏览器在变换函数一致时就是这么插值的）。"""

    def mix(a: float, b: float) -> float:
        return a + (b - a) * progress

    # 透视距离是「长度」不是「姿态」：缺省（None）时按 2D 处理，两边都有才插值
    persp = None
    if first.persp is not None and second.persp is not None:
        persp = mix(first.persp, second.persp)
    elif first.persp is not None or second.persp is not None:
        persp = first.persp if first.persp is not None else second.persp

    return Xform(
        mix(first.dx, second.dx),
        mix(first.dy, second.dy),
        mix(first.rot, second.rot),
        mix(first.rot_y, second.rot_y),
        mix(first.kx, second.kx),
        mix(first.ky, second.ky),
        persp,
    )


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
    frames: list[Frame],
    default_easing: str,
    breath: float,
    stage_w: int,
    stage_h: int,
    mirror: bool = False,
) -> tuple[tuple[float, float, float, float], str]:
    """这个轮廓在整段动画里越出角色框多少：返回 (左, 上, 右, 下) 与最坏的那一瞬。

    走的是变形链的完整顺序（见文件头的说明）：镜像 → 缩放（含呼吸）→ 绕 Y 翻转 →
    平面内旋转 → 位移 → 透视投影。
    """
    img_w, img_h = img_size
    # object-fit: contain —— 取较小的那个比例，图居中放进角色框
    s = min(stage_w / img_w, stage_h / img_h)
    offset = np.array([(stage_w - img_w * s) / 2, (stage_h - img_h * s) / 2])
    # transform-origin: 50% 88%，是相对 .pet-motion（也就是角色框）算的
    o = np.array([stage_w / 2, stage_h * ORIGIN_Y])
    base = pts * s + offset - o
    if mirror:
        # 朝向开关：镜像挂在图上，以角色框的竖直中线为轴 —— 相对原点 o 就是把 x 取反。
        # 等价于「把 scaleX 取反」，但写成轮廓镜像更不容易看错。
        base[:, 0] *= -1

    states = sampled_states(frames, default_easing)
    best = (0.0, 0.0, 0.0, 0.0)
    at = ""
    # 呼吸缩放与动作缩放相乘；两者各自独立循环，相位可能恰好撞在一起，按最坏情况取
    for b in (1.0, 1.0 + breath):
        for label, shape in states:
            x = base[:, 0] * shape.kx * b
            y = base[:, 1] * shape.ky * b
            z_depth = None
            if shape.rot_y:
                # 绕 Y 轴翻转：图是平的，所有点的 z 一开始都是 0，所以 z 只由 x 决定。
                # 在这一步先算出深度，后面平面内旋转不会改变它。
                rad_y = np.deg2rad(shape.rot_y)
                z_depth = -np.sin(rad_y) * x
                x = np.cos(rad_y) * x
            if shape.rot:
                rad = np.deg2rad(shape.rot)
                c, sn = np.cos(rad), np.sin(rad)
                x, y = x * c - y * sn, x * sn + y * c
            x = x + shape.dx
            y = y + shape.dy
            if z_depth is not None and shape.persp is not None:
                # perspective(d)：w = 1 - z/d，坐标除以 w —— 近的一半变大，远的一半变小
                w = 1.0 - z_depth / shape.persp
                if np.any(np.abs(w) < 1e-3):
                    # 相机平面（w <= 0）：这个透视距离已经小到无法成像了，不去假装算得出来
                    return (-1.0, -1.0, -1.0, -1.0), (
                        f"{label} 的透视距离 {shape.persp:g}px 太小："
                        "轮廓会被投到相机后面（w <= 0），几何检查算不了"
                    )
                x, y = x / w, y / w
            px = o[0] + x
            py = o[1] + y
            clip = (
                max(0.0, -px.min()),  # 左越界
                max(0.0, -py.min()),  # 上越界
                max(0.0, px.max() - stage_w),  # 右越界
                max(0.0, py.max() - stage_h),  # 下越界
            )
            if max(clip) > max(best):
                best = clip
                at = (
                    f"{label} 呼吸x{b:.3f} 位移({shape.dx:g},{shape.dy:g})"
                    f" 转{shape.rot:g}deg 绕Y{shape.rot_y:g}deg 缩放({shape.kx:g},{shape.ky:g})"
                )
    return best, at


def motions_of(theme: dict) -> dict[str, dict]:
    """theme.json 的 `motions`，去掉 `_说明` 这类注释键（JSON 没有注释，用键名兜）。"""
    return {name: cfg for name, cfg in (theme.get("motions") or {}).items() if not name.startswith("_")}


def expressions_of(theme: dict) -> dict[str, dict]:
    """theme.json 的 `expressions`，同样去掉注释键。"""
    return {
        name: cfg for name, cfg in (theme.get("expressions") or {}).items() if not name.startswith("_")
    }


def check_config(
    theme: dict, css_text: str
) -> tuple[list[str], list[str]]:
    """返回 (错误, 提示)。错误会让脚本以非 0 退出。"""
    errors: list[str] = []
    notes: list[str] = []

    motions: dict[str, dict] = motions_of(theme)
    expressions: dict[str, dict] = expressions_of(theme)

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

    # 朝向开关必须是真正的布尔值：写成字符串 "false" 在 JS 里是 truthy，
    # 角色会悄悄反着站，而配置看起来「写的就是 false」—— 这种错人眼发现不了
    for field, value in [
        ("mirror（主题级默认朝向）", theme.get("mirror", None)),
        *[(f'表情 "{name}" 的 mirror', cfg.get("mirror", None)) for name, cfg in expressions.items()],
    ]:
        if value is None:
            continue
        if not isinstance(value, bool):
            errors.append(
                f'{field} 必须是 true / false，现在是 {json.dumps(value)}'
                '（字符串 "false" 也会被当成 true，角色朝向会反）'
            )

    # 首尾必须是中性姿态：动作演完到下一步之间可能差一帧，停在半路会闪一下
    for name, cfg in motions.items():
        frames = parse_keyframes(css_text, name, cfg.get("params") or {})
        if not frames:
            continue
        for frame in frames:
            if frame.pct not in (0.0, 100.0):
                continue
            if not frame.xform.is_neutral():
                errors.append(
                    f'动作 "{name}" 的 {frame.pct:g}% 不是中性姿态（位移/旋转/缩放不为 0/1）：'
                    "序列每次换步都从这一帧开始或结束，角色会闪一下"
                )

        # 一个动作内部各帧的变换函数必须完全一致：浏览器只在函数序列一致时逐分量插值，
        # 不一致就退化成矩阵插值（中间过程完全不同，甚至会出现关键帧里没有的斜切），
        # 而几何检查是按逐分量插值算的 —— 那样报出来的数就不作数了。
        signatures: dict[tuple[str, ...], list[str]] = {}
        for frame in frames:
            signatures.setdefault(transform_functions(frame.raw), []).append(f"{frame.pct:g}%")
        if len(signatures) > 1:
            detail = "；".join(
                f"{'+'.join(names) or '(空)'}（{'/'.join(pcts)}）"
                for names, pcts in signatures.items()
            )
            errors.append(
                f'动作 "{name}" 各关键帧用的变换函数不一样：{detail}。'
                "函数序列不一致时浏览器会改用矩阵插值，几何检查（按逐分量算）就对不上了 —— "
                "统一成同一套函数、同一个顺序（见 animations.css 文件头的第 1 条）"
            )

    for motion in sorted(set(motions) - used):
        notes.append(f'动作 "{motion}" 定义了但没有表情用它')

    return errors, notes


def check_easings(theme: dict, css_text: str) -> list[str]:
    """每一条缓动都得能解析出来 —— 缓动是几何检查的一部分。

    这里以前管的是「不许过冲」（那时几何检查只看关键帧，过冲会让位移超过关键帧上的值，
    越界量就悄悄变成下限）。现在中间过程是按缓动曲线采样的，过冲的极值也量得到，
    所以弹簧曲线放开了；取而代之的这一条是**读不懂就别装懂**：
    认不出的缓动（比如 `steps()`）会让几何检查在瞎猜，那种数字比没有数字更糟。
    """
    errors: list[str] = []
    places: dict[str, str] = {}
    for name, cfg in motions_of(theme).items():
        places[f'动作 "{name}" 的 animation 简写'] = rule_easing(css_text, name)
        for frame in parse_keyframes(css_text, name, cfg.get("params") or {}):
            if frame.easing:
                places[f'动作 "{name}" 的 {frame.pct:g}% 关键帧'] = frame.easing

    for where, text in places.items():
        try:
            parse_easing(text)
        except ValueError as err:
            errors.append(f"{where}：{err}")
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
    errors += check_easings(theme, css_text)
    if errors:
        print(f"\n配置有问题（{len(errors)} 处）：")
        for line in errors:
            print(f"  - {line}")
        # 名字对不上 / 缓动认不出来时，下面那张表的数都是错的（或者压根算不出来），
        # 干脆不打印 —— 错的数比没有数更容易误导人。
        print("\n先修掉这些再跑：它们会让下面的几何检查失去意义。")
        return 1
    if notes:
        print(f"\n提示（{len(notes)} 条，不算错）：")
        for line in notes:
            print(f"  - {line}")

    motions: dict[str, dict] = motions_of(theme)
    expressions: dict[str, dict] = expressions_of(theme)

    # 每个动作只跟「**真的会用到它的**那些表情」的图比 —— 动作虽然共用（谁都能写进序列），
    # 但实际会被切掉多少，取决于此刻配出来的那些组合。
    # 拿所有图无脑叉乘会虚报：比如 hop 根本没被笑那个表情用，按笑的图算出来的数
    # 谁也看不见，却会让人误以为 hop 的幅度得调小。
    # 代价是「以后把某个动作加进别的表情」时要重跑一次 —— 上面那句提示会说明这点。
    #
    # 存的是 (表情, 是否镜像)：朝向开关是**按表情**生效的，同一个动作配给
    # 镜像和没镜像的两个表情，切掉的量是不一样的（镜像本身就换了一边），
    # 所以这一对必须一起算，不能只记表情名。
    users: dict[str, list[tuple[str, bool]]] = {name: [] for name in motions}
    for name, cfg in expressions.items():
        mirrored = cfg.get("mirror", theme.get("mirror", False))
        for motion in cfg.get("motions") or []:
            if motion in users:
                users[motion].append((name, bool(mirrored)))

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

    header = f"{'动作':<10}{'左':>7}{'上':>7}{'右':>7}{'下':>7}   表情(朝向) / 最坏的那一瞬"
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
        easing = rule_easing(css_text, name)

        # 没有表情在用这个动作时，只能按基础图估一个数（并标出来）
        tested = users[name] or [(f"<没人用，按 base.png 估>", bool(theme.get("mirror", False)))]
        best = (0.0, 0.0, 0.0, 0.0)
        at = ""
        which = ""
        for expression, mirrored in tested:
            loaded = silhouette(expression if not expression.startswith("<") else "")
            if loaded is None:
                continue
            clip, frame = extremes(
                loaded[0], loaded[1], frames, easing, breath, stage_w, stage_h, mirrored
            )
            if max(clip) > max(best) or max(best) == 0:
                best = clip
                at = frame
                which = f"{expression}{'(镜像)' if mirrored else ''}"
        for key, value in zip(("left", "top", "right", "bottom"), best):
            worst[key] = max(worst[key], value)
        # 一点都不越界时不报「最坏的那一瞬」：那时候谁都不越界，写出来只会让人以为
        # 那个表情/那一帧有什么特别之处
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
