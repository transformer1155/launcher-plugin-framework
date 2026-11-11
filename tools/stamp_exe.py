"""给已编译的 exe 注入图标与版本信息（纯标准库，无需 windres / rc.exe）。

用法：python tools/stamp_exe.py <exe路径>

背景：本机是 x86_64-pc-windows-gnu 工具链且没有 windres / rc.exe，
winres / embed-resource 这类构建期方案跑不起来；改用 Windows 自带的
BeginUpdateResourceW / UpdateResourceW / EndUpdateResourceW 在编译后注入。

每次重新编译并拷贝 exe 后都要重跑一次（资源不在 target 产物里）。
若 exe 正在运行，注入会失败——先关掉启动器。
"""
import ctypes
import re
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ICO = ROOT / "ui" / "launcher-ui" / "assets" / "app.ico"
WORKSPACE_TOML = ROOT / "ui" / "Cargo.toml"

RT_ICON = 3
RT_GROUP_ICON = 14
RT_VERSION = 16
LANG_US = 0x0409

FILE_DESCRIPTION = "Aether Launcher"
PRODUCT_NAME = "Aether Launcher"
ORIGINAL_FILENAME = "AetherLauncher.exe"
COMPANY_NAME = "launcher-plugin-framework"
LEGAL_COPYRIGHT = "MIT License"


def read_version() -> str:
    text = WORKSPACE_TOML.read_text(encoding="utf-8")
    m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    return m.group(1) if m else "0.1.0"


def utf16(s: str) -> bytes:
    return s.encode("utf-16-le") + b"\x00\x00"


def pad4(b: bytearray) -> bytearray:
    while len(b) % 4:
        b.append(0)
    return b


def parse_ico(data: bytes):
    """返回 (图像数据列表, GROUP_ICON 二进制)。"""
    _, itype, count = struct.unpack_from("<HHH", data, 0)
    assert itype == 1, "不是图标文件"
    images = []
    group = bytearray(struct.pack("<HHH", 0, 1, count))
    off = 6
    for i in range(count):
        w, h, cc, r, planes, bits, size, img_off = struct.unpack_from("<BBBBHHII", data, off)
        off += 16
        images.append(data[img_off : img_off + size])
        # GRPICONDIRENTRY：ICONDIRENTRY 的 dwImageOffset(4B) 换成资源 id nID(2B)。
        group += struct.pack("<BBBBHHIH", w, h, cc, r, planes, bits, size, i + 1)
    return images, bytes(group)


def build_version_info(version: str) -> bytes:
    """手工构造 VS_VERSIONINFO 资源（含 StringFileInfo / VarFileInfo）。"""
    v = tuple(int(x) for x in version.split("."))[:4]
    while len(v) < 4:
        v += (0,)
    fixed = struct.pack(
        "<IIIIIIIIIIIII",
        0xFEEF04BD,  # dwSignature
        0x00010000,  # dwStrucVersion
        (v[0] << 16) | v[1],  # dwFileVersionMS/LS
        (v[2] << 16) | v[3],
        (v[0] << 16) | v[1],  # dwProductVersionMS/LS
        (v[2] << 16) | v[3],
        0x0000003F,  # dwFileFlagsMask
        0,  # dwFileFlags
        0x00040004,  # dwFileOS = VOS_NT_WINDOWS32
        1,  # dwFileType = VFT_APP
        0,  # dwFileSubtype
        0,  # dwFileDateMS
        0,  # dwFileDateLS
    )

    strings = [
        ("FileDescription", FILE_DESCRIPTION),
        ("ProductName", PRODUCT_NAME),
        ("FileVersion", version),
        ("ProductVersion", version),
        ("OriginalFilename", ORIGINAL_FILENAME),
        ("CompanyName", COMPANY_NAME),
        ("LegalCopyright", LEGAL_COPYRIGHT),
    ]
    table = bytearray(struct.pack("<HHH", 0, 0, 1) + utf16("040904b0"))
    pad4(table)
    for key, val in strings:
        s = bytearray(struct.pack("<HHH", 0, len(utf16(val)) // 2, 1) + utf16(key))
        pad4(s)
        s += utf16(val)
        pad4(s)
        s[0:2] = struct.pack("<H", len(s))
        table += s
    table[0:2] = struct.pack("<H", len(table))

    sfi = bytearray(struct.pack("<HHH", 0, 0, 1) + utf16("StringFileInfo"))
    pad4(sfi)
    sfi += table
    sfi[0:2] = struct.pack("<H", len(sfi))

    vfi = bytearray(struct.pack("<HHH", 0, 4, 1) + utf16("VarFileInfo"))
    pad4(vfi)
    vfi += struct.pack("<I", 0x04B00409)  # Translation：lang 0x0409 + codepage 1200
    vfi[0:2] = struct.pack("<H", len(vfi))

    root = bytearray(struct.pack("<HHH", 0, 52, 0) + utf16("VS_VERSION_INFO"))
    pad4(root)
    root += fixed + sfi + vfi
    root[0:2] = struct.pack("<H", len(root))
    return bytes(root)


def stamp(exe: Path) -> None:
    version = read_version()
    images, group = parse_ico(ICO.read_bytes())
    ver = build_version_info(version)

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.BeginUpdateResourceW.argtypes = (ctypes.c_wchar_p, ctypes.c_bool)
    k32.BeginUpdateResourceW.restype = ctypes.c_void_p
    k32.UpdateResourceW.argtypes = (
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_ushort,
        ctypes.c_void_p,
        ctypes.c_uint,
    )
    k32.UpdateResourceW.restype = ctypes.c_bool
    k32.EndUpdateResourceW.argtypes = (ctypes.c_void_p, ctypes.c_bool)
    k32.EndUpdateResourceW.restype = ctypes.c_bool

    h = k32.BeginUpdateResourceW(str(exe), False)
    if not h:
        raise OSError(
            f"BeginUpdateResourceW 失败（error={ctypes.get_last_error()}，"
            "exe 是否正在运行或被杀软锁定？）"
        )
    ok = True

    def upd(rtype: int, name: int, data: bytes) -> None:
        nonlocal ok
        if not k32.UpdateResourceW(
            h, ctypes.c_void_p(rtype), ctypes.c_void_p(name), LANG_US, data, len(data)
        ):
            ok = False

    for i, img in enumerate(images):
        upd(RT_ICON, i + 1, img)
    upd(RT_GROUP_ICON, 1, group)
    upd(RT_VERSION, 1, ver)

    if not k32.EndUpdateResourceW(h, not ok):
        raise OSError(
            f"EndUpdateResourceW 失败（error={ctypes.get_last_error()}，写入已放弃）"
        )
    if not ok:
        raise OSError("部分 UpdateResourceW 失败，写入已放弃")
    print(f"已注入图标 + 版本信息 {version}：{exe}")


def main() -> None:
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    exe = Path(sys.argv[1])
    if not exe.is_file():
        print(f"目标不存在：{exe}")
        sys.exit(1)
    stamp(exe)


if __name__ == "__main__":
    main()