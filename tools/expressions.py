"""把 AI 生成的「表情表」切成一张张独立的透明 PNG。

给一张表情表（一张图上排好 N 个表情），输出可以直接被主题使用的透明 PNG。

    npm run expressions            # 就是下面这条，路径都写好了
    python -I tools/expressions.py themes/zhuzhu/expressions/face.jpg \
        themes/zhuzhu/expressions --ref themes/zhuzhu/base.png --debug

对表情表的要求（不满足的话脚本会给出提示，但通常能切出东西来）：
  * **纯色背景**，实测用洋红最好（`rgb(255,0,255)` 附近）。见下面第 1 条。
  * 网格排布（默认 3x3），格子之间留分隔线，空白格会被自动跳过。
  * 每个格子里画一个表情，从左到右、从上到下按 `--names` 的顺序给名字。

为什么要写脚本而不是手动切：切完还要统一画布、对齐锚点、抠底、去紫边、
归一构图，这几步里任何一步手工做不一致，切换动作时角色就会「跳」。脚本每次都一样。

踩过的坑，都在这段代码里处理掉了：

1. **模型给不了真透明背景。** 让它画「透明背景」，实际经常是白底，或者干脆画上
   棋盘格。所以约定让它画在**纯色背景**上（实测洋红），在这里按颜色抠掉。

2. **不能按「到某个背景色的距离」抠。** 背景并不是均匀的：同一张表里角落是
   (252,1,251)，中间某一格却是 (227,19,227)，绝对距离差到 67，会被误判成角色，
   整格都抠不掉。改用**洋红度**这个不变量 `min(R,B) - G` —— 背景恒在 240 以上，
   而角色（白/黑/灰/黄/红口条）实测最大只有 16，两个数量级的间隔，随便它怎么偏色。
   也不能用「接近白色」当判据：卡通角色的身体本身就是白的，会一起抠没。

3. **JPEG 有振铃。** 分隔线两侧会渗出暗紫色像素，直接当角色保留会在边缘留脏线。
   靠 `--inset` 裁掉分隔线，再抹掉最外圈一条窄带兜住残余。

4. **边缘是半透明的。** 抗锯齿像素是「角色色 × 背景色」的混合，直接二值化会留
   一圈洋红毛边。所以 alpha 用渐变而不是硬阈值，并且对半透明像素做「反混合」
   （unmix）：C = (P - (1-a)·B) / a，把混进去的背景色去掉。这里 B 用**逐像素拟合**
   的背景（对背景像素做平面拟合），因为背景本身有渐变，用一个常数会在半透明处偏色。

5. **每格的底边裁切位置不一致。** 分隔线会把上一行的角色多切掉几个像素，
   于是同一张表里各格的可见高度能差近 10px —— 切换动作时角色会上下跳。
   所以所有格统一裁到**同一个底边**（取各格中最靠上的那个）。

6. **格子尺寸会差 1 像素。** 1024/3 不是整数，逐格 round 出来的宽高不一样，
   而前端是 `object-fit: contain`，尺寸不同 → 渲染大小不同 → 切动作时角色会胀缩。
   所以统一裁到所有格里最小的那个尺寸（居中裁，角色本来就是居中的）。

7. **角色头顶常有浮空装饰。** 惊讶的「!!」、生气的「#」和角色不连通，却比耳朵还高，
   会把内容包围盒的顶边往上拽近 20px。按那个包围盒对齐，切到惊讶时整个角色会
   突然往下掉一截。所以定位只用**和躯干连通的连通块**（`silhouette`），
   装饰留在图里但不参与定位。

8. **生成的角色在画布里更「满」。** AI 画的表情表把身体画得比 base.png 多，
   同样的头宽下内容高出约 4%。而前端是 `object-fit: contain` + 固定窗口，
   内容越高 → 缩放越小 → 角色整体变小，且顶到窗口上沿被裁。
   所以最后一步按 `--ref` 给的参考图（base.png）**归一构图**：算出参考图里
   「头宽占画布的比例」和「内容顶边占画布的比例」，把各格摆成同样的比例。
   这样新素材和旧素材在窗口里渲染出来的大小、位置完全一致，
   换素材不会让角色忽大忽小，动画幅度也不用重调。
"""

from __future__ import annotations

import argparse
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

# 洋红度 = min(R,B) - G。实测背景 ≈ 250，角色 ≤ 16（见 base.png 的实测对照），
# 中间全是抗锯齿像素。
#
# alpha 不能用固定阈值切，而要按混合比例反推：边缘像素是
#   P = a·C + (1-a)·B
# 取洋红度（角色 ≈0，背景 ≈250）就得到
#   洋红度(P) ≈ (1-a)·洋红度(B)   =>   a = 1 - 洋红度(P) / 洋红度(B)
# 所以「除以本地的背景洋红度」才是对的。
#
# 早先用固定的 55→150 斜坡，把「实际含 22% 背景」的边缘像素算成了 98.9% 不透明，
# 于是整圈留下一条洋红边（实测不透明区最大洋红度 56，而原图只有 16）。
# 背景洋红度本身还在 208~254 之间漂（同一张表里就有），所以这个除数必须是
# 逐像素的局部值，不能写成常数。
# alpha 低于这个值算背景残留，抹零。留一点点只是给 JPEG 噪声兜底，
# 真正的分隔线残留由 EDGE_TRIM 负责。
ALPHA_FLOOR = 0.06
# 抹掉每格最外圈的多少像素，兜住分隔线残余
EDGE_TRIM = 3
# 内容 alpha 高于这个值才算角色像素
CONTENT_ALPHA = 8
# 背景洋红度低于这个值就认为「这张表不是洋红底」，给出提示
BG_SCORE_MIN = 120


def parse_args(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(description="把表情表切成透明 PNG")
    p.add_argument("sheet", type=Path, help="表情表图片")
    p.add_argument("outdir", type=Path, help="输出目录")
    p.add_argument("--rows", type=int, default=3)
    p.add_argument("--cols", type=int, default=3)
    p.add_argument(
        "--names",
        default="idle,speaking,happy,angry,sad,surprised,laugh",
        help="按从左到右、从上到下的顺序给每格命名；用 - 跳过空白格",
    )
    p.add_argument("--inset", type=int, default=8, help="每格四周裁掉多少像素，去掉分隔线")
    p.add_argument(
        "--no-normalize",
        action="store_true",
        help="不做底边对齐（只想看看原始效果时才用）",
    )
    p.add_argument(
        "--ref",
        type=Path,
        default=None,
        help="构图参考图（base.png）。给了就按它的「头宽/画布」「顶边/画布」比例归一各格",
    )
    p.add_argument(
        "--debug",
        nargs="?",
        type=Path,
        const=Path(tempfile.gettempdir()) / "expr-check.png",
        default=None,
        help="额外输出一张对齐校验图；不带路径就写到系统临时目录",
    )
    return p.parse_args(argv)


def magenta_score(rgb: np.ndarray) -> np.ndarray:
    """洋红程度。背景远大于 0，角色的白/黑/灰/黄/红口条都接近 0。"""
    return np.minimum(rgb[..., 0], rgb[..., 2]).astype(np.int32) - rgb[..., 1].astype(np.int32)


def fit_background(rgb: np.ndarray, score: np.ndarray) -> np.ndarray:
    """逐像素估计背景色。

    背景自带渐变（某张表里从 (252,1,251) 到 (227,19,227)），用一个常数去反混合，
    半透明的那一圈会偏色。这里拿背景像素做一个平面拟合，贴着渐变走。
    背景像素太少（整格几乎都是角色）时退回中位数。
    """
    h, w, _ = rgb.shape
    mask = score > 200
    ys, xs = np.nonzero(mask)
    if len(ys) < 50:
        return np.broadcast_to(np.median(rgb[mask], axis=0), rgb.shape)

    design = np.stack([np.ones(len(ys)), xs, ys], axis=1).astype(np.float64)
    coef, *_ = np.linalg.lstsq(design, rgb[ys, xs].astype(np.float64), rcond=None)

    gy, gx = np.mgrid[0:h, 0:w]
    return coef[0] + coef[1] * gx[..., None] + coef[2] * gy[..., None]


def cut_cell(rgb: np.ndarray, r: int, c: int, rows: int, cols: int, inset: int) -> np.ndarray:
    h, w, _ = rgb.shape
    y0 = int(r * h / rows) + inset
    y1 = int((r + 1) * h / rows) - inset
    x0 = int(c * w / cols) + inset
    x1 = int((c + 1) * w / cols) - inset
    return rgb[y0:y1, x0:x1]


def key_out(cell: np.ndarray) -> np.ndarray:
    """按洋红度抠出 RGBA，边缘做反混合去掉混进去的背景色。"""
    score = magenta_score(cell)
    rgb_f = cell.astype(np.float64)

    bg = fit_background(rgb_f, score)
    # 本地的背景洋红度。除以它才是真正的混合比例，用常数会留洋红边。
    bg_score = np.maximum(magenta_score(np.clip(bg, 0, 255)), 1.0)

    alpha = 1.0 - score / bg_score
    alpha = np.clip(alpha, 0.0, 1.0)
    alpha[alpha < ALPHA_FLOOR] = 0.0

    # 抹掉最外圈：分隔线的振铃就在这一圈，它和角色不连通，
    # 抹掉不会伤到角色本身（角色边缘离格边至少几十像素）
    alpha[:EDGE_TRIM, :] = 0.0
    alpha[-EDGE_TRIM:, :] = 0.0
    alpha[:, :EDGE_TRIM] = 0.0
    alpha[:, -EDGE_TRIM:] = 0.0

    out = np.zeros((*cell.shape[:2], 4), dtype=np.uint8)
    # 反混合：P = a·C + (1-a)·B  =>  C = (P - (1-a)·B) / a
    # 只对真正半透明的像素做；纯色区域 a=1，除以 1 是恒等变换，没必要动
    semi = (alpha > 0.02) & (alpha < 0.98)
    a = alpha[..., None]
    unmixed = (rgb_f - (1.0 - a) * bg) / np.maximum(a, 1e-6)
    rgb = np.where(semi[..., None], unmixed, rgb_f)
    out[..., :3] = np.clip(rgb, 0, 255).astype(np.uint8)
    out[..., 3] = (alpha * 255).astype(np.uint8)
    return out


def silhouette(rgba: np.ndarray, threshold: int = CONTENT_ALPHA) -> np.ndarray:
    """角色主体的连通区域（布尔掩码）。

    表情表头顶常浮着装饰：惊讶的「!!」、生气的「#」。它们和角色不连通，
    却比耳朵还高，会把内容包围盒的顶边往上拽近 20px（实测 14 vs 34）。
    按那个包围盒对齐，切到惊讶时整个角色会突然往下掉一截。
    所以从最宽的那一行（一定是躯干）向外涨满，只认和躯干连通的部分。
    装饰照样留在输出图里（本来就是表情的一部分），只是不参与定位。
    """
    mask = rgba[..., 3] > threshold
    rows = mask.sum(axis=1)
    if rows.max() == 0:
        return mask

    y = int(rows.argmax())
    label = np.zeros_like(mask)
    label[max(0, y - 4) : y + 5, :] = mask[max(0, y - 4) : y + 5, :]
    # 逐像素涨满。格子只有几百像素宽，实测 7 格共 0.3 s，不值得为它写并查集。
    for _ in range(mask.shape[0] + mask.shape[1]):
        grown = label.copy()
        grown[1:, :] |= label[:-1, :]
        grown[:-1, :] |= label[1:, :]
        grown[:, 1:] |= label[:, :-1]
        grown[:, :-1] |= label[:, 1:]
        grown &= mask
        if grown.sum() == label.sum():
            break
        label = grown
    return label


def mask_bbox(mask: np.ndarray) -> tuple[int, int, int, int] | None:
    ys, xs = np.nonzero(mask)
    if len(xs) == 0:
        return None
    return int(xs.min()), int(ys.min()), int(xs.max()), int(ys.max())


def head_span(mask: np.ndarray) -> tuple[int, int] | None:
    """主体上部 30% 的左右边界，用它代表「头」的位置和宽度。

    不用整个内容的宽度定位：那会被手臂姿势带偏（某一格拍手，宽度就比别格窄 12px），
    各格的头才是同一颗头，眼睛跟的是头。
    """
    box = mask_bbox(mask)
    if box is None:
        return None
    _, y0, _, y1 = box
    band = y0 + max(1, int((y1 - y0) * 0.3))
    nonzero = np.nonzero(mask[y0:band, :].any(axis=0))[0]
    if len(nonzero) == 0:
        return None
    return int(nonzero.min()), int(nonzero.max())


def head_width(mask: np.ndarray) -> int:
    span = head_span(mask)
    return 0 if span is None else span[1] - span[0] + 1


def uniform_crop(cells: dict[str, np.ndarray]) -> dict[str, np.ndarray]:
    """把所有格居中裁到同一个最小尺寸。

    「居中」是有依据的：实测各格角色水平居中误差 ≤1px，所以居中等价于对齐角色。
    """
    h = min(c.shape[0] for c in cells.values())
    w = min(c.shape[1] for c in cells.values())
    out = {}
    for name, c in cells.items():
        top = (c.shape[0] - h) // 2
        left = (c.shape[1] - w) // 2
        out[name] = c[top : top + h, left : left + w]
    return out


def frame_cells(cells: dict[str, np.ndarray], ref: Path) -> dict[str, np.ndarray]:
    """把各格摆成参考图的构图比例，输出同一尺寸的方画布。

    前端是固定窗口 + `object-fit: contain`，所以「角色在画布里占多大」直接决定
    最终渲染大小。生成的表情表比 base.png 画得满，不归一的话换上表情图角色会变小、
    还会在弹跳时顶出窗口。

    只对齐两个比例就够了 —— 头占画布多宽、内容顶边在画布多高处：
      * 头宽决定缩放，进而决定整个角色的大小；
      * 顶边决定上方留白，也就是弹跳（surprised 向上 30px）和放大（1.12 倍，
        原点在 88% 高度处，越靠上顶出去越多）时够不够顶。

    各格头宽实测差 3px（274~277），取最大的那个定画布，宁可留白多一点，
    这样没有一格会被上面的裁切吃掉。
    """
    ref_img = Image.open(ref).convert("RGBA")
    # 参考图只用来取比例，和分辨率无关；base.png 是 1170px，整幅跑连通块太慢，
    # 隔几个像素抽一次样即可（实测比例误差 < 1%，换算到 337 画布上不到 1 像素）。
    step = max(1, -(-max(ref_img.size) // 400))
    ref_arr = np.asarray(ref_img)[::step, ::step]
    ref_mask = silhouette(ref_arr)
    ref_box = mask_bbox(ref_mask)
    if ref_box is None:
        raise SystemExit(f"参考图 {ref} 里没有不透明内容")

    # 画布是方的，contain 会按长边缩放，所以比例的分母取画布长边。
    # 注意除数是**降采样后**的边长：量的和除的必须是同一张图。
    side = float(max(ref_arr.shape[:2]))
    head_ratio = head_width(ref_mask) / side
    top_ratio = ref_box[1] / side
    if head_ratio <= 0:
        raise SystemExit(f"参考图 {ref} 的上部量不出宽度，换一张参考图")

    canvas = int(np.ceil(max(head_width(silhouette(c)) for c in cells.values()) / head_ratio))
    top = round(canvas * top_ratio)
    visible = canvas - top
    print(
        f"构图参考 {ref.name}：头宽占画布 {head_ratio:.4f}，内容顶边占 {top_ratio:.4f}\n"
        f"  -> 统一画布 {canvas}x{canvas}，内容顶边 y={top}，可见高度 {visible}"
    )

    out: dict[str, np.ndarray] = {}
    for name, rgba in cells.items():
        mask = silhouette(rgba)
        box = mask_bbox(mask)
        span = head_span(mask)
        assert box is not None and span is not None
        # 水平：头中心对到画布中线
        head_cx = (span[0] + span[1]) / 2
        left = int(round(canvas / 2 - head_cx))
        # 垂直：主体顶边对到 y=top，底边对齐画布底边（多出来的身体裁掉，
        # 和 base.png 一样只露到肩膀）
        y0 = box[1]
        rows = min(visible, rgba.shape[0] - y0)

        dst = np.zeros((canvas, canvas, 4), dtype=np.uint8)
        sx0, sx1 = 0, rgba.shape[1]
        dx0 = left
        if dx0 < 0:
            sx0 -= dx0
            dx0 = 0
        sx1 = min(sx1, sx0 + (canvas - dx0))
        dst[top : top + rows, dx0 : dx0 + (sx1 - sx0)] = rgba[y0 : y0 + rows, sx0:sx1]
        out[name] = dst
    return out


def check_sheet(rgb: np.ndarray, rows: int, cols: int) -> None:
    """开工前先看一眼这张表是不是照约定画的，不对劲就直接说清楚。

    这些检查都只是**提示**，不拦截：抠底算法对渐变背景是稳的，
    真正的失败模式（白底、棋盘格）会表现为「整格都抠不掉」，那时下面的
    内容占比会明显异常。
    """
    h, w, _ = rgb.shape
    score = magenta_score(rgb)
    border = np.concatenate([score[0], score[-1], score[:, 0], score[:, -1]])
    border_med = float(np.median(border))
    print(f"表情表 {w}x{h}，四边洋红度中位 {border_med:.0f}（洋红底应 > 200）")
    if border_med < BG_SCORE_MIN:
        print(
            "  ! 四边不像纯洋红底。抠底靠的是「洋红度」这个不变量，"
            "换成白底/浅色底/棋盘格会整格抠不掉。建议让模型把背景画成 rgb(255,0,255)。"
        )

    for r in range(1, rows):
        y = int(h * r / rows)
        window = score[max(0, y - 8) : y + 9]
        best = float((window > BG_SCORE_MIN).mean(axis=1).max())
        if best < 0.6:
            print(f"  ! 第 {r} 条**行**分割线附近只有 {best:.0%} 是背景，格子可能没对齐")

    for c in range(1, cols):
        x = int(w * c / cols)
        window = score[:, max(0, x - 8) : x + 9]
        best = float((window > BG_SCORE_MIN).mean(axis=0).max())
        if best < 0.6:
            print(f"  ! 第 {c} 条**列**分割线附近只有 {best:.0%} 是背景，格子可能没对齐")


def main(argv: list[str]) -> int:
    args = parse_args(argv)

    sheet = Image.open(args.sheet).convert("RGB")
    rgb = np.asarray(sheet)
    check_sheet(rgb, args.rows, args.cols)

    names = [n.strip() for n in args.names.split(",")]
    needed = args.rows * args.cols
    if len(names) < needed:
        names += ["-"] * (needed - len(names))

    cells: dict[str, np.ndarray] = {}
    empty: list[int] = []
    for r in range(args.rows):
        for c in range(args.cols):
            name = names[r * args.cols + c]
            rgba = key_out(cut_cell(rgb, r, c, args.rows, args.cols, args.inset))
            if name in ("", "-"):
                empty.append(r * args.cols + c + 1)
                continue
            if mask_bbox(silhouette(rgba)) is None:
                print(f"  ! 第 {r * args.cols + c + 1} 格（{name}）抠出来是空的，跳过")
                empty.append(r * args.cols + c + 1)
                continue
            cells[name] = rgba
    if empty:
        print(f"跳过的格子（空白或未命名）：{empty}")

    if not cells:
        print("没有切出任何一格，检查 --names 和 --inset", file=sys.stderr)
        return 1

    cells = uniform_crop(cells)
    size = next(iter(cells.values())).shape[:2]
    print(f"统一画布 {size[1]}x{size[0]}（各格居中裁到最小尺寸）")
    print(f"{'名字':<10} {'主体宽x高':<12} {'头宽':<6} {'占格':<10} 主体包围盒")
    for name, rgba in cells.items():
        mask = silhouette(rgba)
        x0, y0, x1, y1 = mask_bbox(mask)
        print(
            f"{name:<10} {f'{x1 - x0 + 1}x{y1 - y0 + 1}':<12} {head_width(mask):<6} "
            f"{f'{100 * (x1 - x0 + 1) / size[1]:.0f}%x{100 * (y1 - y0 + 1) / size[0]:.0f}%':<10} "
            f"({x0},{y0})-({x1},{y1})"
        )

    # 统一底边：取所有格子里最靠上的那个主体底部。
    # 不这么做的话，被分隔线多切了几像素的那一格会比别格矮出一截，切动作时角色会上下跳。
    bottoms = [mask_bbox(silhouette(x))[3] for x in cells.values()]
    bottom = min(bottoms) if not args.no_normalize else max(bottoms)
    print(f"统一基线：主体底 y={bottom}（各格原始底边 {sorted(bottoms)}）")

    if args.ref:
        cells = frame_cells(cells, args.ref)
        size = next(iter(cells.values())).shape[:2]
        bottom = min(mask_bbox(silhouette(x))[3] for x in cells.values())
        print(f"归一后：画布 {size[1]}x{size[0]}，主体底 y={bottom}")
        for name, rgba in cells.items():
            mask = silhouette(rgba)
            x0, y0, x1, y1 = mask_bbox(mask)
            print(
                f"  {name:<10} 主体 {x1 - x0 + 1}x{y1 - y0 + 1} 顶 y={y0} 头宽 {head_width(mask)} "
                f"占画布 {100 * head_width(mask) / size[1]:.0f}%"
            )

    args.outdir.mkdir(parents=True, exist_ok=True)
    aligned: list[tuple[str, np.ndarray]] = []
    for name, rgba in cells.items():
        fixed = rgba.copy()
        if bottom + 1 < fixed.shape[0]:
            fixed[bottom + 1 :, :, 3] = 0
        Image.fromarray(fixed, "RGBA").save(args.outdir / f"{name}.png")
        aligned.append((name, fixed))
    print(f"已写出 {len(aligned)} 张到 {args.outdir}")

    if args.debug:
        make_debug_sheet(aligned, bottom, args.debug)
        print(f"校验图 -> {args.debug}")

    return 0


def make_debug_sheet(aligned: list[tuple[str, np.ndarray]], bottom: int, path: Path) -> None:
    """把各格并排**合成**到深色底上，并标出统一基线。

    用深色底是因为角色本身是白的：白底上既看不出边缘抠得干不干净，
    也看不出各格的脚底有没有对齐。

    注意必须真的做 alpha 合成。`RGBA.convert("RGB")` 只是丢掉 alpha 通道，
    不会合成 —— 直接那样输出的话，透明像素会原样带着背景色显示出来，
    看上去像没抠干净，其实是这张预览图在骗人。
    """
    h, w = next(iter(aligned))[1].shape[:2]
    gap = 10
    canvas = np.zeros((h + 2 * gap, len(aligned) * w + (len(aligned) + 1) * gap, 3), dtype=np.float64)
    canvas[...] = 24.0

    line_y = gap + bottom
    canvas[line_y : line_y + 2, :] = (0, 220, 220)

    for i, (_, rgba) in enumerate(aligned):
        x = gap + i * (w + gap)
        patch = canvas[gap : gap + h, x : x + w]
        a = rgba[..., 3:4].astype(np.float64) / 255.0
        canvas[gap : gap + h, x : x + w] = rgba[..., :3] * a + patch * (1.0 - a)

    Image.fromarray(canvas.astype(np.uint8), "RGB").save(path)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
