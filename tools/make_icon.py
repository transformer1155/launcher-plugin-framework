"""生成启动器图标（草方块像素风）→ ui/launcher-ui/assets/app.ico。

ICO 采用 Vista+ 支持的「PNG 内嵌」格式：ICONDIR + 单个 256x256 RGBA PNG。
纯标准库实现（struct + zlib），无需 Pillow。
"""
import random
import struct
import zlib
from pathlib import Path

N = 16  # 逻辑像素网格
S = 16  # 每逻辑像素放大倍数（16 * 16 = 256px）
OUT = Path(__file__).resolve().parent.parent / "ui" / "launcher-ui" / "assets" / "app.ico"


def clamp(v: int) -> int:
    return max(0, min(255, int(v)))


def build_pixels():
    random.seed(20261111)  # 固定种子：同一份代码永远生成同一张图
    grid = []
    for y in range(N):
        row = []
        for x in range(N):
            edge = x in (0, N - 1) or y in (0, N - 1)
            if edge:
                c = (44, 37, 30)  # 深色描边
            elif y < 4:  # 草皮
                j = random.randint(-16, 16)
                c = (clamp(104 + j), clamp(190 + j), clamp(50 + j))
            elif y == 4:  # 草土交界
                c = (84, 148, 38) if random.random() < 0.5 else (132, 95, 66)
            else:  # 泥土
                j = random.randint(-14, 14)
                c = (clamp(132 + j), clamp(95 + j), clamp(66 + j))
            row.append(c + (255,))
        grid.append(row)
    return grid


def png_from_pixels(grid) -> bytes:
    w = h = N * S
    parts = []
    for y in range(N):
        for _ in range(S):
            line = bytearray()
            for x in range(N):
                line += bytes(grid[y][x]) * S
            parts.append(b"\x00" + bytes(line))  # filter 0
    raw = b"".join(parts)

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def wrap_ico(png: bytes) -> bytes:
    # ICONDIRENTRY：宽 / 高为 0 表示 256。
    ico = struct.pack("<HHH", 0, 1, 1)
    ico += struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(png), 6 + 16)
    return ico + png


def main() -> None:
    OUT.parent.mkdir(parents=True, exist_ok=True)
    ico = wrap_ico(png_from_pixels(build_pixels()))
    OUT.write_bytes(ico)
    print(f"图标已生成：{OUT}（{len(ico)} 字节）")


if __name__ == "__main__":
    main()