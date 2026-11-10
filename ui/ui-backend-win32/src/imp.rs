//! Win32 GDI 后端（**手写 FFI**，不依赖 `windows-sys`）。
//!
//! # 为什么手写 FFI
//!
//! 本机环境的 `windows-sys` 编译失败：它需要 `dlltool` 生成 import library，
//! 而本机 rustup 自带的 `dlltool` 有 `CreateProcess` 缺陷（实测）。因此
//! 直接 `#[link(name = "user32")]` + `extern "system"` 手写绑定 —— 已实测
//! 可以成功链接、创建窗口、GDI 绘制。
//!
//! # 已实现
//!
//! - **无边框窗口**：`WS_POPUP` + 自绘标题栏（顶栏承担拖动/最小化/最大化/关闭）。
//!   不用 `WS_OVERLAPPEDWINDOW` 的经典系统标题栏 —— 那是 XP 时代观感，
//!   而且 `SetWindowRgn` 会让 DWM 回退到经典窗口装饰。
//! - **双缓冲**：`WM_PAINT` 先把整帧画进内存 DC，最后一次 `BitBlt` 上屏；
//!   并拦截 `WM_ERASEBKGND`，杜绝"先擦白、再逐条画"造成的闪烁。
//! - `WM_PAINT` → 遍历 `DrawList` 用 GDI 绘制（含圆角、裁剪栈）
//! - 鼠标/键盘事件 → `ui_core::Event`（含 `TrackMouseEvent` 的 MouseLeave）
//! - DPI 感知与文本测量缓存
//!
//! # 已知限制
//!
//! - GDI 不写 alpha 通道，半透明色只能与背景预混；`BlurRegion` 是 tint 近似。
//! - ClearType 亚像素：使用默认系统渲染。

#![allow(
    non_camel_case_types,
    non_snake_case,
    dead_code,
    clippy::upper_case_acronyms
)]

use std::collections::HashMap;

use ui_core::backend::Backend;
use ui_core::draw::{DrawList, DrawOp, TextAlign};
use ui_core::event::{Event, Key, MouseButton};
use ui_core::layout::{Rect, Size};

// ---------------------------------------------------------------------------
// FFI 类型与常量
// ---------------------------------------------------------------------------

type HWND = isize;
type HINSTANCE = isize;
type HBRUSH = isize;
type HDC = isize;
type HICON = isize;
type HCURSOR = isize;
type HPEN = isize;
type HFONT = isize;
type LPARAM = isize;
type WPARAM = usize;
type LRESULT = isize;
type UINT = u32;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Msg {
    hwnd: HWND,
    message: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
    time: u32,
    pt_x: i32,
    pt_y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RectC {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct WndClassW {
    style: UINT,
    lpfn_wnd_proc: Option<extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: HINSTANCE,
    h_icon: HICON,
    h_cursor: HCURSOR,
    hbr_background: HBRUSH,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
}

#[repr(C)]
struct PaintStruct {
    hdc: HDC,
    f_erase: i32,
    rc_paint: RectC,
    f_restore: i32,
    f_inc_update: i32,
    reserved: [u8; 32],
}

#[repr(C)]
struct LogFontW {
    height: i32,
    width: i32,
    escapement: i32,
    orientation: i32,
    weight: i32,
    italic: u8,
    underline: u8,
    strike_out: u8,
    char_set: u8,
    out_precision: u8,
    clip_precision: u8,
    quality: u8,
    pitch_and_family: u8,
    face_name: [u16; 32],
}

/// `TrackMouseEvent` 的参数结构：用来在鼠标离开客户区时收到 `WM_MOUSELEAVE`。
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TrackMouseEvent {
    cb_size: u32,
    dw_flags: u32,
    hwnd_track: HWND,
    dw_hover_time: u32,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(c: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: UINT,
        class_name: *const u16,
        window_name: *const u16,
        style: UINT,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HWND,
        menu: isize,
        instance: HINSTANCE,
        param: *mut core::ffi::c_void,
    ) -> HWND;
    fn DestroyWindow(h: HWND) -> i32;
    fn ShowWindow(h: HWND, cmd: i32) -> i32;
    fn UpdateWindow(h: HWND) -> i32;
    fn GetMessageW(m: *mut Msg, h: HWND, min: UINT, max: UINT) -> i32;
    fn PeekMessageW(m: *mut Msg, h: HWND, min: UINT, max: UINT, remove: UINT) -> i32;
    fn TranslateMessage(m: *const Msg) -> i32;
    fn DispatchMessageW(m: *const Msg) -> LRESULT;
    fn PostQuitMessage(code: i32);
    fn DefWindowProcW(h: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT;
    fn BeginPaint(h: HWND, ps: *mut PaintStruct) -> HDC;
    fn EndPaint(h: HWND, ps: *const PaintStruct) -> i32;
    fn GetClientRect(h: HWND, r: *mut RectC) -> i32;
    fn InvalidateRect(h: HWND, r: *const RectC, erase: i32) -> i32;
    fn GetDC(h: HWND) -> HDC;
    fn ReleaseDC(h: HWND, hdc: HDC) -> i32;
    fn LoadCursorW(inst: HINSTANCE, name: *const u16) -> HCURSOR;
    fn LoadIconW(inst: HINSTANCE, name: *const u16) -> HICON;
    fn SetCursor(c: HCURSOR) -> HCURSOR;
    fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
    fn GetSystemMetrics(index: i32) -> i32;
    fn ScreenToClient(h: HWND, p: *mut Point) -> i32;
    fn GetKeyState(vk: i32) -> i16;
    fn GetTickCount64() -> u64;
    // ---- 无边框窗口 / 双缓冲 / 输入焦点 ----
    fn GetWindowRect(h: HWND, r: *mut RectC) -> i32;
    fn IsZoomed(h: HWND) -> i32;
    fn ReleaseCapture() -> i32;
    fn SendMessageW(h: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT;
    fn PostMessageW(h: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> i32;
    fn SetFocus(h: HWND) -> HWND;
    fn TrackMouseEvent(t: *mut TrackMouseEvent) -> i32;
    // ---- 窗口圆角 / 动画心跳 ----
    fn SetWindowRgn(h: HWND, rgn: isize, redraw: i32) -> i32;
    fn SetTimer(h: HWND, id: usize, elapse: u32, callback: *const core::ffi::c_void) -> usize;
    fn KillTimer(h: HWND, id: usize) -> i32;
    fn SetForegroundWindow(h: HWND) -> i32;
}

// `shell32`：托盘图标 + 文件拖放。
#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(cmd: u32, data: *mut NotifyIconDataW) -> i32;
    fn DragAcceptFiles(h: HWND, accept: i32);
    fn DragQueryFileW(hdrop: isize, index: u32, buf: *mut u16, cch: u32) -> u32;
    fn DragFinish(hdrop: isize);
}

/// `NOTIFYICONDATAW`（Win32 托盘图标结构，字段顺序与大小不可变）。
#[repr(C)]
struct NotifyIconDataW {
    cb_size: u32,
    hwnd: HWND,
    uid: u32,
    u_flags: u32,
    u_callback_message: u32,
    h_icon: HICON,
    sz_tip: [u16; 128],
    dw_state: u32,
    dw_state_mask: u32,
    sz_info: [u16; 256],
    u_version: u32,
    sz_info_title: [u16; 64],
    dw_info_flags: u32,
    guid_item: [u8; 16],
    h_balloon_icon: HICON,
}

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> isize;
    fn GetProcAddress(module: isize, name: *const u8) -> isize;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateSolidBrush(color: u32) -> HBRUSH;
    fn DeleteObject(obj: isize) -> i32;
    fn CreatePen(style: i32, width: i32, color: u32) -> HPEN;
    fn SelectObject(hdc: HDC, obj: isize) -> isize;
    fn Rectangle(hdc: HDC, left: i32, top: i32, right: i32, bottom: i32) -> i32;
    fn SetTextColor(hdc: HDC, color: u32) -> u32;
    fn SetBkMode(hdc: HDC, mode: i32) -> i32;
    fn TextOutW(hdc: HDC, x: i32, y: i32, s: *const u16, count: i32) -> i32;
    fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strike_out: u32,
        char_set: u32,
        out_precision: u32,
        clip_precision: u32,
        quality: u32,
        pitch_and_family: u32,
        face: *const u16,
    ) -> HFONT;
    fn GetTextExtentPoint32W(hdc: HDC, s: *const u16, count: i32, size: *mut SizeC) -> i32;
    fn SetBkColor(hdc: HDC, color: u32) -> u32;
    // ---- 截图相关（验证真实字体渲染） ----
    fn CreateCompatibleDC(hdc: HDC) -> HDC;
    fn CreateCompatibleBitmap(hdc: HDC, w: i32, h: i32) -> isize;
    fn BitBlt(
        dst: HDC,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        src: HDC,
        sx: i32,
        sy: i32,
        rop: u32,
    ) -> i32;
    fn GetDIBits(
        hdc: HDC,
        bmp: isize,
        start: u32,
        lines: u32,
        bits: *mut u8,
        info: *mut BitmapInfo,
        usage: u32,
    ) -> i32;
    fn DeleteDC(hdc: HDC) -> i32;
    // ---- 圆角 / 裁剪 / 私有字体 / 32bpp 半透明 ----
    fn RoundRect(hdc: HDC, l: i32, t: i32, r: i32, b: i32, ellipse_w: i32, ellipse_h: i32) -> i32;
    fn CreateRoundRectRgn(l: i32, t: i32, r: i32, b: i32, w: i32, h: i32) -> isize;
    fn SaveDC(hdc: HDC) -> i32;
    fn RestoreDC(hdc: HDC, saved: i32) -> i32;
    fn IntersectClipRect(hdc: HDC, l: i32, t: i32, r: i32, b: i32) -> i32;
    fn AddFontResourceExW(name: *const u16, flags: u32, reserved: *mut core::ffi::c_void) -> i32;
    fn RemoveFontResourceExW(name: *const u16, flags: u32, reserved: *mut core::ffi::c_void)
        -> i32;
    fn GetStockObject(index: i32) -> isize;
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SizeC {
    cx: i32,
    cy: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_pels: i32,
    y_pels: i32,
    clr_used: u32,
    clr_important: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RgbQuad {
    blue: u8,
    green: u8,
    red: u8,
    reserved: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BitmapInfo {
    header: BitmapInfoHeader,
    colors: [RgbQuad; 1],
}

const BI_RGB: u32 = 0;
const SRCCOPY: u32 = 0x00CC_0020;
const DIB_RGB_COLORS: u32 = 0;

const PM_REMOVE: UINT = 0x0001;
const WM_DESTROY: UINT = 0x0002;
const WM_SIZE: UINT = 0x0005;
const WM_CLOSE: UINT = 0x0010;
const WM_ERASEBKGND: UINT = 0x0014;
const WM_PAINT: UINT = 0x000F;
const WM_KEYDOWN: UINT = 0x0100;
const WM_CHAR: UINT = 0x0102;
const WM_MOUSEMOVE: UINT = 0x0200;
const WM_LBUTTONDOWN: UINT = 0x0201;
const WM_LBUTTONUP: UINT = 0x0202;
const WM_RBUTTONDOWN: UINT = 0x0204;
const WM_RBUTTONUP: UINT = 0x0205;
const WM_MOUSEWHEEL: UINT = 0x020A;
const WM_MOUSELEAVE: UINT = 0x02A3;
/// 文件拖放到窗口（拖拽安装整合包 / .jar）。
const WM_DROPFILES: UINT = 0x0233;
/// 托盘图标回调消息。
const WM_TRAY: UINT = 0x8001;
/// 系统级拖动：把客户区按下伪装成"按在标题栏上"。
const WM_NCLBUTTONDOWN: UINT = 0x00A1;
const HTCAPTION: WPARAM = 2;
const TME_LEAVE: u32 = 0x0002;

const CS_HREDRAW: UINT = 0x0002;
const CS_VREDRAW: UINT = 0x0001;
/// 无边框：系统标题栏（XP 观感）由自绘顶栏取代。
const WS_POPUP: UINT = 0x8000_0000;
const WS_MINIMIZEBOX: UINT = 0x0002_0000;
const WS_MAXIMIZEBOX: UINT = 0x0001_0000;
const WS_EX_APPWINDOW: UINT = 0x0004_0000;
const SW_HIDE: i32 = 0;
const SW_SHOW: i32 = 5;
const SW_MINIMIZE: i32 = 6;
const SW_MAXIMIZE: i32 = 3;
const SW_RESTORE: i32 = 9;
const SW_SHOWNORMAL: i32 = 1;
const TRANSPARENT: i32 = 1;
const IDC_ARROW: usize = 32512;
const IDI_APPLICATION: usize = 32512;

const VK_BACK: i32 = 0x08;
const VK_RETURN: i32 = 0x0D;
const VK_ESCAPE: i32 = 0x1B;
const VK_DELETE: i32 = 0x2E;
const VK_TAB: i32 = 0x09;
const VK_HOME: i32 = 0x24;
const VK_END: i32 = 0x23;
const VK_LEFT: i32 = 0x25;
const VK_RIGHT: i32 = 0x27;
const VK_UP: i32 = 0x26;
const VK_DOWN: i32 = 0x28;
const VK_F5: i32 = 0x74;

// ---- 系统托盘 ----
const NIM_ADD: u32 = 0;
const NIM_DELETE: u32 = 2;
const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const TRAY_UID: u32 = 0x4C50;

// ---- 圆角窗口 / 动画心跳 / 系统 backdrop ----
const WM_TIMER: UINT = 0x0113;
const TIMER_ANIM: usize = 0x4C1;
const FR_PRIVATE: u32 = 0x10;
const NULL_BRUSH: i32 = 5;
const PS_SOLID: i32 = 0;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
const DWMWCP_DONOTROUND: u32 = 1;
const DWMWCP_ROUND: u32 = 2;
const DWMSBT_NONE: u32 = 1;
const DWMSBT_TRANSIENTWINDOW: u32 = 3; // Acrylic
const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_DISABLED: u32 = 0;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    gradient_color: u32, // ABGR
    animation_id: u32,
}

#[repr(C)]
struct WinCompAttrData {
    attribute: u32,
    data: *mut core::ffi::c_void,
    size: u32,
}

type DwmSetAttrFn = unsafe extern "system" fn(HWND, u32, *const core::ffi::c_void, u32) -> i32;
type SetCompAttrFn = unsafe extern "system" fn(HWND, *mut WinCompAttrData) -> i32;

/// 未文档化 / 新版 API 动态取地址：GNU 工具链的导入库不一定有这些符号，
/// 直接 `#[link]` 会在链接期失败。取不到就静默降级（功能缺失，不影响启动）。
static mut DWM_SET_ATTR: usize = 0;
static mut SET_COMP_ATTR: usize = 0;

unsafe fn load_optional_apis() {
    let u = wide("user32.dll");
    let hu = LoadLibraryW(u.as_ptr());
    if hu != 0 {
        SET_COMP_ATTR =
            GetProcAddress(hu, c"SetWindowCompositionAttribute".as_ptr().cast::<u8>()) as usize;
    }
    let d = wide("dwmapi.dll");
    let hd = LoadLibraryW(d.as_ptr());
    if hd != 0 {
        DWM_SET_ATTR = GetProcAddress(hd, c"DwmSetWindowAttribute".as_ptr().cast::<u8>()) as usize;
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// 系统托盘 / 拖拽安装
// ---------------------------------------------------------------------------

unsafe fn tray_icon_data(hwnd: HWND) -> NotifyIconDataW {
    let mut d: NotifyIconDataW = core::mem::zeroed();
    d.cb_size = core::mem::size_of::<NotifyIconDataW>() as u32;
    d.hwnd = hwnd;
    d.uid = TRAY_UID;
    d.u_flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    d.u_callback_message = WM_TRAY;
    d.h_icon = LoadIconW(0, IDI_APPLICATION as *const u16);
    let tip = wide("Minecraft 启动器");
    for (i, c) in tip.iter().take(127).enumerate() {
        d.sz_tip[i] = *c;
    }
    d
}

unsafe fn tray_add(hwnd: HWND) {
    let mut d = tray_icon_data(hwnd);
    Shell_NotifyIconW(NIM_ADD, &mut d);
}

unsafe fn tray_delete(hwnd: HWND) {
    let mut d = tray_icon_data(hwnd);
    Shell_NotifyIconW(NIM_DELETE, &mut d);
}

unsafe fn restore_window(hwnd: HWND) {
    ShowWindow(hwnd, SW_SHOWNORMAL);
    ShowWindow(hwnd, SW_RESTORE);
    SetForegroundWindow(hwnd);
}

/// 取出 `WM_DROPFILES` 拖进来的全部路径（UTF-16 → UTF-8）。
unsafe fn collect_dropped_files(hdrop: isize) -> Vec<String> {
    let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, core::ptr::null_mut(), 0);
    let mut paths = Vec::new();
    for i in 0..count.min(64) {
        let mut buf = [0u16; 512];
        let n = DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
        if n > 0 {
            paths.push(String::from_utf16_lossy(
                &buf[..(n as usize).min(buf.len())],
            ));
        }
    }
    DragFinish(hdrop);
    paths
}

/// 读取当前修饰键状态。
///
/// 用 `GetKeyState`：返回值的**高位**表示"当前是否按下"，低位是"上次消息时是否按下"。
/// 这里取高位（当前状态）。
unsafe fn current_mods() -> ui_core::event::Modifiers {
    const VK_CONTROL: i32 = 0x11;
    const VK_SHIFT: i32 = 0x10;
    const VK_MENU: i32 = 0x12;
    let down = |vk: i32| GetKeyState(vk) < 0; // 高位为 1 → 按下
    ui_core::event::Modifiers {
        ctrl: down(VK_CONTROL),
        shift: down(VK_SHIFT),
        alt: down(VK_MENU),
    }
}

/// Windows COLORREF 是 `0x00BBGGRR`；我们的 `Color` 是 `0xAARRGGBB`。
fn to_colorref(argb: u32) -> u32 {
    let r = (argb >> 16) & 0xFF;
    let g = (argb >> 8) & 0xFF;
    let b = argb & 0xFF;
    (b << 16) | (g << 8) | r
}

/// GDI 的 `Rectangle`/`RoundRect` 不写 alpha 通道，半透明色只能**先与背景预混**
/// 成不透明色再画（软件后端才是真正的 alpha 合成）。
fn premix(argb: u32, bg: u32) -> u32 {
    let a = (argb >> 24) & 0xFF;
    if a == 0xFF {
        return argb;
    }
    let mix = |shift: u32| -> u32 {
        let s = (argb >> shift) & 0xFF;
        let d = (bg >> shift) & 0xFF;
        ((s * a + d * (255 - a)) / 255) & 0xFF
    };
    0xFF00_0000 | (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

/// 圆角填充。brush 与 pen 同色，避免 GDI 在边缘画出 1px 异色描边。
unsafe fn fill_round_gdi(hdc: HDC, rect: RectC, argb: u32, radius: i32) {
    let brush = CreateSolidBrush(to_colorref(argb));
    let pen = CreatePen(PS_SOLID, 1, to_colorref(argb));
    let old_brush = SelectObject(hdc, brush);
    let old_pen = SelectObject(hdc, pen);
    let d = (radius.max(0) * 2).max(1);
    RoundRect(hdc, rect.left, rect.top, rect.right, rect.bottom, d, d);
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    DeleteObject(brush);
    DeleteObject(pen);
}

/// 圆角描边：用 NULL_BRUSH 让 GDI 只画边、不填内部。
unsafe fn stroke_round_gdi(hdc: HDC, rect: RectC, argb: u32, radius: i32, thickness: i32) {
    let pen = CreatePen(PS_SOLID, thickness.max(1), to_colorref(argb));
    let null_brush = GetStockObject(NULL_BRUSH);
    let old_brush = SelectObject(hdc, null_brush);
    let old_pen = SelectObject(hdc, pen);
    let d = (radius.max(0) * 2).max(1);
    RoundRect(hdc, rect.left, rect.top, rect.right, rect.bottom, d, d);
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    DeleteObject(pen);
}

/// 应用窗口级效果：整体圆角 + 系统 backdrop（磨玻璃）。
///
/// 圆角用 `SetWindowRgn` 真正裁掉四角（全 Windows 版本可用），并叠加 Win11
/// 的 `DWMWA_WINDOW_CORNER_PREFERENCE`（更平滑的系统阴影）。
/// 磨玻璃用两代 API：Win10 的 `WCA_ACCENT_POLICY` 亚克力 + Win11 的
/// `DWMWA_SYSTEMBACKDROP_TYPE`。两者都不可用时，UI 侧仍会用半透明 tint 近似。
unsafe fn apply_window_effects(hwnd: HWND, style: &ui_core::StyleConfig) {
    // ★ region 坐标相对**窗口矩形**左上角（含非客户区），不是 `GetClientRect`。
    // 旧的 `WS_OVERLAPPEDWINDOW` 有标题栏，用客户区尺寸会裁掉窗口右下角；
    // 现在是无边框 popup，两者一致，但用窗口矩形仍然是正确语义。
    let mut rc: RectC = core::mem::zeroed();
    GetWindowRect(hwnd, &mut rc);
    let w = (rc.right - rc.left).max(1);
    let h = (rc.bottom - rc.top).max(1);
    // 最大化时不要裁角：否则屏幕四角会被切掉一块。
    let radius = if IsZoomed(hwnd) != 0 {
        0
    } else {
        style.window_radius.clamp(0, w.min(h) / 2)
    };
    if radius > 0 {
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, radius * 2, radius * 2);
        if rgn != 0 {
            // 注意：调用成功后 region 归系统所有，**不要** DeleteObject。
            SetWindowRgn(hwnd, rgn, 1);
        }
    } else {
        SetWindowRgn(hwnd, 0, 1);
    }

    if DWM_SET_ATTR != 0 {
        let f: DwmSetAttrFn = core::mem::transmute(DWM_SET_ATTR);
        let corner: u32 = if radius > 0 {
            DWMWCP_ROUND
        } else {
            DWMWCP_DONOTROUND
        };
        f(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const u32 as *const core::ffi::c_void,
            4,
        );
        let backdrop: u32 = if style.glass.enabled {
            DWMSBT_TRANSIENTWINDOW
        } else {
            DWMSBT_NONE
        };
        f(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const u32 as *const core::ffi::c_void,
            4,
        );
    }

    if SET_COMP_ATTR != 0 {
        let f: SetCompAttrFn = core::mem::transmute(SET_COMP_ATTR);
        let t = style.glass.tint.0;
        let (a, r, g, b) = (
            (t >> 24) & 0xFF,
            (t >> 16) & 0xFF,
            (t >> 8) & 0xFF,
            t & 0xFF,
        );
        let mut accent = AccentPolicy {
            state: if style.glass.enabled {
                ACCENT_ENABLE_ACRYLICBLURBEHIND
            } else {
                ACCENT_DISABLED
            },
            flags: 2,
            gradient_color: (a << 24) | (b << 16) | (g << 8) | r,
            animation_id: 0,
        };
        let mut data = WinCompAttrData {
            attribute: WCA_ACCENT_POLICY,
            data: &mut accent as *mut AccentPolicy as *mut core::ffi::c_void,
            size: core::mem::size_of::<AccentPolicy>() as u32,
        };
        f(hwnd, &mut data);
    }
}

// ---------------------------------------------------------------------------
// 共享状态（窗口过程是 C 回调，需要访问 Rust 侧的帧数据）
// ---------------------------------------------------------------------------

/// 一帧要绘制的内容与事件缓冲。
struct Shared {
    /// 最近一帧的绘制指令（在 `WM_PAINT` 里被消费）。
    pending: Option<DrawList>,
    /// 累积的输入事件（由主循环取出）。
    events: Vec<Event>,
    /// 鼠标是否在窗口内（用于区分 MouseLeave）。
    mouse_inside: bool,
    /// 是否已经请求过 TrackMouseEvent（避免重复请求）。
    tracking: bool,
    width: i32,
    height: i32,
    scale: f32,
    /// 字体缓存：`(size_px, bold)` → HFONT。
    fonts: HashMap<(i32, bool), HFONT>,
    /// 文本宽度缓存。
    widths: HashMap<(String, i32, bool), i32>,
    /// 背景色（清屏用）。
    bg: u32,
    /// 上一帧的时间戳（算帧间隔用）。
    last_tick: u64,
    /// 窗口句柄（窗口级特效要用）。
    hwnd: HWND,
    /// 当前外观配置。
    style: ui_core::StyleConfig,
    /// 当前字体族（UTF-16，NUL 结尾）。
    font_family: Vec<u16>,
    /// 已装载的私有字体文件路径（卸载时用）。
    loaded_font: Option<Vec<u16>>,
    /// 业务侧是否还有动画在跑（决定要不要开 16ms 心跳）。
    anim_active: bool,
    timer_on: bool,
}

static mut SHARED_PTR: *mut Shared = std::ptr::null_mut();

fn shared() -> &'static mut Shared {
    unsafe {
        // SAFETY: 单线程消息循环，SHARED_PTR 在 Win32Backend::run 期间有效。
        &mut *SHARED_PTR
    }
}

// ---------------------------------------------------------------------------
// 窗口过程
// ---------------------------------------------------------------------------

extern "system" fn wnd_proc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let s = shared();
        match msg {
            WM_PAINT => {
                let mut ps: PaintStruct = core::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rc: RectC = core::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                let w = rc.right.max(1);
                let h = rc.bottom.max(1);

                // ★ 双缓冲：整帧先画进内存位图，最后一次 BitBlt 上屏。
                // 直接往屏幕 DC 上"清屏 → 逐条 GDI 绘制"会暴露中间过程；
                // 动画心跳每 16ms 重绘一次时，肉眼就是持续闪烁。
                let mem = CreateCompatibleDC(hdc);
                let bmp = CreateCompatibleBitmap(hdc, w, h);
                let old_bmp = SelectObject(mem, bmp);

                // 背景：brush 与 pen 同色，避免 GDI 的 Rectangle 在边缘留 1px 黑框。
                let bg = to_colorref(s.bg);
                let brush = CreateSolidBrush(bg);
                let pen = CreatePen(PS_SOLID, 1, bg);
                let old_brush = SelectObject(mem, brush);
                let old_pen = SelectObject(mem, pen);
                Rectangle(mem, 0, 0, w, h);
                SelectObject(mem, old_brush);
                SelectObject(mem, old_pen);
                DeleteObject(brush);
                DeleteObject(pen);

                // 用 `clone()` 而不是 `take()`：绘制数据要**保留**。
                // 如果 take 掉，窗口被遮挡后系统要求重画（WM_PAINT）时就没有内容可画，
                // 表现为窗口内容"消失"或闪回空白背景。
                if let Some(list) = s.pending.clone() {
                    draw_list(mem, s, &list);
                }

                BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);

                SelectObject(mem, old_bmp);
                DeleteObject(bmp);
                DeleteDC(mem);
                EndPaint(hwnd, &ps);
                0
            }
            WM_ERASEBKGND => {
                // 背景已在 WM_PAINT 的内存 DC 里一次画完。若让 DefWindowProc 用
                // 系统画刷先擦一遍，每帧都会先闪一下白色（尤其窗口被遮挡恢复时）。
                1
            }
            WM_CLOSE => {
                DestroyWindow(hwnd); // WM_DESTROY → PostQuitMessage
                0
            }
            WM_SIZE => {
                let w = (lp & 0xFFFF) as i32;
                let h = ((lp >> 16) & 0xFFFF) as i32;
                s.width = w;
                s.height = h;
                // 窗口尺寸变了，圆角 region 必须重建（否则缩放后圆角半径不变但边缘错位）。
                if s.hwnd != 0 && s.style.window_radius > 0 {
                    apply_window_effects(s.hwnd, &s.style);
                }
                s.events.push(Event::Resize { w, h });
                0
            }
            WM_TIMER => {
                // 动画心跳：没有用户输入时也要推进时间。
                // 它不是鼠标/键盘事件，UI 层的 hover / 点击判定会忽略它。
                if wp == TIMER_ANIM && s.timer_on {
                    s.events.push(Event::Tick);
                }
                0
            }
            WM_MOUSEMOVE => {
                let x = (lp & 0xFFFF) as i32;
                let y = ((lp >> 16) & 0xFFFF) as i32;
                // 高位是符号扩展的，需要处理负坐标（窗口外）
                let x = (x << 16) >> 16;
                let y = (y << 16) >> 16;
                s.mouse_inside = true;
                s.events.push(Event::MouseMove { x, y });
                if !s.tracking {
                    // 真正请求 WM_MOUSELEAVE。旧实现只把 tracking 置 true 却从不
                    // 调用 TrackMouseEvent，于是"鼠标移出窗口"永远收不到事件 →
                    // 按下态 / hover 会卡住（尤其拖出窗口松手时）。
                    let mut tme = TrackMouseEvent {
                        cb_size: core::mem::size_of::<TrackMouseEvent>() as u32,
                        dw_flags: TME_LEAVE,
                        hwnd_track: hwnd,
                        dw_hover_time: 0,
                    };
                    if TrackMouseEvent(&mut tme) != 0 {
                        s.tracking = true;
                    }
                }
                0
            }
            WM_MOUSELEAVE => {
                s.tracking = false;
                s.mouse_inside = false;
                s.events.push(Event::MouseLeave);
                0
            }
            WM_LBUTTONDOWN => {
                // 无边框 popup 不会自动获得焦点，键盘输入（WM_CHAR）会完全失效。
                SetFocus(hwnd);
                let x = ((lp & 0xFFFF) as i32) << 16 >> 16;
                let y = (((lp >> 16) & 0xFFFF) as i32) << 16 >> 16;
                s.events.push(Event::MouseDown {
                    x,
                    y,
                    button: MouseButton::Left,
                    mods: current_mods(),
                });
                0
            }
            WM_LBUTTONUP => {
                let x = ((lp & 0xFFFF) as i32) << 16 >> 16;
                let y = (((lp >> 16) & 0xFFFF) as i32) << 16 >> 16;
                s.events.push(Event::MouseUp {
                    x,
                    y,
                    button: MouseButton::Left,
                    mods: current_mods(),
                });
                0
            }
            WM_RBUTTONDOWN => {
                let x = ((lp & 0xFFFF) as i32) << 16 >> 16;
                let y = (((lp >> 16) & 0xFFFF) as i32) << 16 >> 16;
                s.events.push(Event::MouseDown {
                    x,
                    y,
                    button: MouseButton::Right,
                    mods: current_mods(),
                });
                0
            }
            WM_RBUTTONUP => {
                let x = ((lp & 0xFFFF) as i32) << 16 >> 16;
                let y = (((lp >> 16) & 0xFFFF) as i32) << 16 >> 16;
                s.events.push(Event::MouseUp {
                    x,
                    y,
                    button: MouseButton::Right,
                    mods: current_mods(),
                });
                0
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp >> 16) as i16) as i32 / 120;
                let mut pt = Point {
                    x: ((lp & 0xFFFF) as i32) << 16 >> 16,
                    y: (((lp >> 16) & 0xFFFF) as i32) << 16 >> 16,
                };
                ScreenToClient(hwnd, &mut pt);
                s.events.push(Event::Wheel {
                    x: pt.x,
                    y: pt.y,
                    delta,
                    mods: current_mods(),
                });
                0
            }
            WM_KEYDOWN => {
                let vk = wp as i32;
                let mods = current_mods();
                let key = match vk {
                    VK_BACK => Some(Key::Backspace),
                    VK_RETURN => Some(Key::Enter),
                    VK_ESCAPE => Some(Key::Escape),
                    VK_DELETE => Some(Key::Delete),
                    VK_TAB => Some(Key::Tab),
                    VK_HOME => Some(Key::Home),
                    VK_END => Some(Key::End),
                    VK_LEFT => Some(Key::Left),
                    VK_RIGHT => Some(Key::Right),
                    VK_UP => Some(Key::Up),
                    VK_DOWN => Some(Key::Down),
                    _ => None,
                };
                if let Some(k) = key {
                    s.events.push(Event::KeyDown { key: k, mods });
                }
                // 全局快捷键：后端把「虚拟键码 + Ctrl」归一化成平台无关的意图。
                if vk == VK_F5 {
                    s.events.push(Event::Shortcut(ui_core::ShortcutKey::F5));
                } else if vk == VK_ESCAPE {
                    s.events.push(Event::Shortcut(ui_core::ShortcutKey::Escape));
                } else if mods.ctrl {
                    // 数字 1..9 与 A..Z 的虚拟键码恰好等于大写 ASCII。
                    if let Some(c) = char::from_u32(vk as u32) {
                        if c.is_ascii_alphanumeric() {
                            s.events.push(Event::Shortcut(ui_core::ShortcutKey::Ctrl(
                                c.to_ascii_uppercase(),
                            )));
                        }
                    }
                }
                0
            }
            WM_CHAR => {
                let ch = wp as u32;
                // Ctrl 组合是快捷键，不能同时把字母插进输入框。
                if ch >= 0x20 && ch != 0x7F && !current_mods().ctrl {
                    if let Some(c) = char::from_u32(ch) {
                        s.events.push(Event::Text {
                            text: c.to_string(),
                        });
                    }
                }
                0
            }
            // 拖入文件：把全部路径交给业务层做拖拽安装。
            WM_DROPFILES => {
                let paths = collect_dropped_files(wp as isize);
                s.events.push(Event::FilesDropped { paths });
                0
            }
            // 托盘图标被点击：恢复窗口并通知业务层清掉 tray_hidden。
            WM_TRAY => {
                let mouse_msg = (lp as u32) & 0xFFFF;
                if mouse_msg == WM_LBUTTONUP || mouse_msg == WM_RBUTTONUP {
                    restore_window(hwnd);
                    s.events.push(Event::TrayRestore);
                }
                0
            }
            WM_DESTROY => {
                tray_delete(hwnd);
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn to_c_rect(r: ui_core::Rect) -> RectC {
    RectC {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

/// 把绘制指令用 GDI 画出来。
unsafe fn draw_list(hdc: HDC, s: &mut Shared, list: &DrawList) {
    let bg = s.bg;
    // 裁剪栈：PushClip 保存 DC 状态并求交，PopClip 恢复。
    // 这让滚动列表真正被裁到可视区（之前是被忽略的，见旧 README 的已知限制）。
    let mut clip_stack: Vec<i32> = Vec::new();
    for op in list.ops() {
        match op {
            DrawOp::FillRoundRect {
                rect,
                color,
                radius,
            } => {
                if *rect == ui_core::Rect::new(0, 0, 0, 0) {
                    continue;
                }
                fill_round_gdi(hdc, to_c_rect(*rect), premix(color.0, bg), *radius);
            }
            DrawOp::StrokeRoundRect {
                rect,
                color,
                radius,
                thickness,
            } => {
                stroke_round_gdi(
                    hdc,
                    to_c_rect(*rect),
                    premix(color.0, bg),
                    *radius,
                    *thickness,
                );
            }
            DrawOp::BlurRegion { rect, radius, tint } => {
                // GDI 没有"模糊已绘制像素"的能力：这里画一层预混 tint 的圆角面板，
                // 视觉上接近毛玻璃；真正的像素模糊在软件后端（用于截图/验证）。
                // 系统级 backdrop（Acrylic/Mica）由 apply_window_effects 申请。
                fill_round_gdi(
                    hdc,
                    to_c_rect(*rect),
                    premix(tint.0, bg),
                    (*radius / 3).max(4),
                );
            }
            DrawOp::FillRect { rect, color } => {
                if *rect == Rect::new(0, 0, 0, 0) {
                    continue;
                }
                let argb = premix(color.0, bg);
                let brush = CreateSolidBrush(to_colorref(argb));
                let old = SelectObject(hdc, brush);
                let pen = CreatePen(0, 1, to_colorref(argb));
                let old_pen = SelectObject(hdc, pen);
                Rectangle(hdc, rect.x, rect.y, rect.right(), rect.bottom());
                SelectObject(hdc, old);
                SelectObject(hdc, old_pen);
                DeleteObject(brush);
                DeleteObject(pen);
            }
            DrawOp::StrokeRect {
                rect,
                color,
                thickness,
            } => {
                // UI 内核通常已展开，这里兜底
                let t = *thickness;
                let brush = CreateSolidBrush(to_colorref(color.0));
                let old = SelectObject(hdc, brush);
                Rectangle(hdc, rect.x, rect.y, rect.right(), rect.y + t);
                Rectangle(hdc, rect.x, rect.bottom() - t, rect.right(), rect.bottom());
                Rectangle(hdc, rect.x, rect.y, rect.x + t, rect.bottom());
                Rectangle(hdc, rect.right() - t, rect.y, rect.right(), rect.bottom());
                SelectObject(hdc, old);
                DeleteObject(brush);
            }
            DrawOp::Text {
                rect,
                text,
                color,
                size_px,
                align,
                bold,
            } => {
                let font = font_for(s, hdc, *size_px, *bold);
                let old_font = SelectObject(hdc, font);
                SetTextColor(hdc, to_colorref(premix(color.0, bg)));
                SetBkMode(hdc, TRANSPARENT);
                SetBkColor(hdc, to_colorref(s.bg));

                let tw = measure_cached(s, hdc, text, *size_px, *bold);
                let x = match align {
                    TextAlign::Left => rect.x,
                    TextAlign::Center => rect.x + (rect.w - tw) / 2,
                    TextAlign::Right => rect.right() - tw,
                };
                let y = rect.y;
                let w = wide(text);
                TextOutW(hdc, x, y, w.as_ptr(), (w.len() - 1) as i32);

                SelectObject(hdc, old_font);
            }
            DrawOp::Line {
                rect,
                color,
                horizontal,
            } => {
                let brush = CreateSolidBrush(to_colorref(color.0));
                let old = SelectObject(hdc, brush);
                let pen = CreatePen(0, 1, to_colorref(color.0));
                let old_pen = SelectObject(hdc, pen);
                if *horizontal {
                    Rectangle(hdc, rect.x, rect.y, rect.right(), rect.y + rect.h.max(1));
                } else {
                    Rectangle(hdc, rect.x, rect.y, rect.x + rect.w.max(1), rect.bottom());
                }
                SelectObject(hdc, old);
                SelectObject(hdc, old_pen);
                DeleteObject(brush);
                DeleteObject(pen);
            }
            DrawOp::PushClip { rect } => {
                // SaveDC 会把裁剪区一并保存，RestoreDC 是唯一可靠的配对方式。
                let saved = SaveDC(hdc);
                IntersectClipRect(hdc, rect.x, rect.y, rect.right(), rect.bottom());
                clip_stack.push(saved);
            }
            DrawOp::PopClip => {
                if let Some(saved) = clip_stack.pop() {
                    RestoreDC(hdc, saved);
                }
            }
        }
    }
}

unsafe fn font_for(s: &mut Shared, _hdc: HDC, size_px: i32, bold: bool) -> HFONT {
    if let Some(f) = s.fonts.get(&(size_px, bold)) {
        return *f;
    }
    // 逻辑像素 → 物理像素：先应用用户字号缩放，再乘 DPI。
    let px = ((s.style.font.scaled_size(size_px) as f32) * s.scale).round() as i32;
    let font = CreateFontW(
        -px, // 负值 = 字符高度而非单元格高度
        0,
        0,
        0,
        if bold { 700 } else { 400 },
        0,
        0,
        0,
        1, // DEFAULT_CHARSET
        0,
        0,
        5, // CLEARTYPE_QUALITY
        0,
        s.font_family.as_ptr(),
    );
    s.fonts.insert((size_px, bold), font);
    font
}

unsafe fn measure_cached(s: &mut Shared, hdc: HDC, text: &str, size_px: i32, bold: bool) -> i32 {
    let key = (text.to_owned(), size_px, bold);
    if let Some(w) = s.widths.get(&key) {
        return *w;
    }
    let font = font_for(s, hdc, size_px, bold);
    let old = SelectObject(hdc, font);
    let w = wide(text);
    let mut sz = SizeC::default();
    GetTextExtentPoint32W(hdc, w.as_ptr(), (w.len() - 1) as i32, &mut sz);
    SelectObject(hdc, old);
    // 逻辑像素
    let logical = ((sz.cx as f32) / s.scale).round() as i32;
    s.widths.insert(key, logical);
    logical
}

// ---------------------------------------------------------------------------
// 后端
// ---------------------------------------------------------------------------

/// Win32 后端句柄。
///
/// 生命周期：`Win32Backend::run()` 进入消息循环，`frame_fn` 每帧回调一次，
/// 直到窗口关闭。
pub struct Win32Backend {
    hwnd: HWND,
    width: i32,
    height: i32,
    scale: f32,
    /// 帧回调之后的绘制数据（由 `run` 内部使用）。
    last_list: Option<DrawList>,
}

impl Win32Backend {
    /// 创建窗口（不进入消息循环）。
    pub fn create(title: &str, width: i32, height: i32) -> Option<Self> {
        unsafe {
            let inst = GetModuleHandleW(core::ptr::null());
            let class_name = wide("LauncherSelfMadeUI");
            let wc = WndClassW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfn_wnd_proc: Some(wnd_proc),
                cb_cls_extra: 0,
                cb_wnd_extra: 0,
                h_instance: inst,
                h_icon: LoadIconW(0, IDI_APPLICATION as *const u16),
                h_cursor: LoadCursorW(0, IDC_ARROW as *const u16),
                // 不注册系统画刷：背景由 WM_PAINT 在内存 DC 里画，且拦截了擦除。
                hbr_background: 0,
                lpsz_menu_name: core::ptr::null(),
                lpsz_class_name: class_name.as_ptr(),
            };
            // 重复注册同一个类会失败，忽略返回值（第二次创建时应复用）
            RegisterClassW(&wc);

            let shared_box = Box::new(Shared {
                pending: None,
                events: Vec::new(),
                mouse_inside: false,
                tracking: false,
                width,
                height,
                scale: dpi_scale_probe(),
                fonts: HashMap::new(),
                widths: HashMap::new(),
                bg: 0xFF18_181B,
                last_tick: 0,
                hwnd: 0,
                style: ui_core::StyleConfig::default(),
                font_family: wide("Segoe UI"),
                loaded_font: None,
                anim_active: false,
                timer_on: false,
            });
            SHARED_PTR = Box::into_raw(shared_box);

            let wtitle = wide(title);
            let hwnd = CreateWindowExW(
                // WS_EX_APPWINDOW：让无边框 popup 出现在任务栏 / Alt+Tab 里。
                WS_EX_APPWINDOW,
                class_name.as_ptr(),
                wtitle.as_ptr(),
                // 无系统标题栏：顶栏由 UI 自绘。这样 DWM 不会因为 SetWindowRgn
                // 回退到经典（XP 观感）窗口装饰，圆角也完整作用在整块窗口上。
                WS_POPUP | WS_MINIMIZEBOX | WS_MAXIMIZEBOX,
                // 居中
                (GetSystemMetrics(0) - width) / 2,
                (GetSystemMetrics(1) - height) / 2,
                width,
                height,
                0,
                0,
                inst,
                core::ptr::null_mut(),
            );
            if hwnd == 0 {
                return None;
            }
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);

            // 窗口建好后才能申请系统级特效（圆角 / 亚克力）。
            shared().hwnd = hwnd;
            load_optional_apis();
            apply_window_effects(hwnd, &shared().style);
            // 允许把整合包 / .jar 拖到窗口上；托盘图标常驻，最小化时可隐藏窗口。
            DragAcceptFiles(hwnd, 1);
            tray_add(hwnd);

            let mut rc: RectC = core::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            let (client_w, client_h, scale) = {
                let s = shared();
                // Shared 的尺寸是 Ui::window_rect() 的来源，必须同步成真实客户区，
                // 否则首帧会按创建参数画、和可见区域差一个边框宽度。
                s.width = rc.right;
                s.height = rc.bottom;
                (s.width, s.height, s.scale)
            };
            Some(Self {
                hwnd,
                width: client_w,
                height: client_h,
                scale,
                last_list: None,
            })
        }
    }

    /// 自检模式：渲染 `frames` 帧后**截图并关闭窗口**。
    ///
    /// 返回 `(最后帧绘制列表, 截图)`。截图是 `(w, h, BGR像素)`。
    ///
    /// 用于**自动化验证**：证明真实窗口能创建、GDI 能用真实系统字体绘制中文、
    /// 消息循环能正常退出。没有它，`run_loop` 会一直阻塞到用户关窗，CI 无法验证。
    pub fn run_selftest<F>(
        self,
        frames: u32,
        mut frame: F,
    ) -> (DrawList, Option<(i32, i32, Vec<u8>)>)
    where
        F: FnMut(&Win32Backend, &[Event]) -> DrawList,
    {
        unsafe {
            let list = frame(&self, &[]);
            shared().pending = Some(list.clone());
            InvalidateRect(self.hwnd, core::ptr::null(), 0);
            UpdateWindow(self.hwnd);

            let mut last = list;
            for _ in 0..frames {
                let mut msg: Msg = core::mem::zeroed();
                // 用 PeekMessage 而非 GetMessage：自检不能阻塞等待用户输入。
                while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let events = take_events();
                last = frame(&self, &events);
                shared().pending = Some(last.clone());
                InvalidateRect(self.hwnd, core::ptr::null(), 0);
                UpdateWindow(self.hwnd);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }

            // ★ 先在窗口还活着时截图，再销毁。
            let shot = self.capture();
            DestroyWindow(self.hwnd);
            (last, Some(shot))
        }
    }

    /// 截图：把客户区抓成 BGR 像素（每像素 3 字节），返回 `(w, h, pixels)`。
    ///
    /// 用于**验证真实字体渲染**（软件后端只有点阵，看不出中文是否正确）。
    pub fn capture(&self) -> (i32, i32, Vec<u8>) {
        unsafe {
            let mut rc: RectC = core::mem::zeroed();
            GetClientRect(self.hwnd, &mut rc);
            let w = rc.right;
            let h = rc.bottom;
            if w <= 0 || h <= 0 {
                return (0, 0, Vec::new());
            }

            let src = GetDC(self.hwnd);
            let mem = CreateCompatibleDC(src);
            let bmp = CreateCompatibleBitmap(src, w, h);
            let old = SelectObject(mem, bmp);
            // 把待绘内容画进 mem（克隆一份，不动 `pending` —— 见 WM_PAINT 处的说明）
            if let Some(list) = shared().pending.clone() {
                draw_list(mem, shared(), &list);
            } else {
                BitBlt(mem, 0, 0, w, h, src, 0, 0, SRCCOPY);
            }

            let mut info = BitmapInfo {
                header: BitmapInfoHeader {
                    size: core::mem::size_of::<BitmapInfoHeader>() as u32,
                    width: w,
                    height: -h, // 负高度 = 自上而下
                    planes: 1,
                    bit_count: 24,
                    compression: BI_RGB,
                    size_image: 0,
                    x_pels: 0,
                    y_pels: 0,
                    clr_used: 0,
                    clr_important: 0,
                },
                colors: [RgbQuad::default()],
            };
            let mut buf = vec![0u8; (w * h * 3) as usize];
            let got = GetDIBits(
                mem,
                bmp,
                0,
                h as u32,
                buf.as_mut_ptr(),
                &mut info,
                DIB_RGB_COLORS,
            );

            SelectObject(mem, old);
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(self.hwnd, src);

            if got == 0 {
                (0, 0, Vec::new())
            } else {
                (w, h, buf)
            }
        }
    }

    /// 进入消息循环。
    ///
    /// `frame` 每帧调用一次，签名 `(backend, events) -> DrawList`：
    /// - `backend` 用于测量文字（布局需要）；
    /// - `events` 是本帧累积的输入事件（已消费清空）。
    ///
    /// # ⚠️ 重绘策略（这里曾经有一个会导致界面闪烁的 bug）
    ///
    /// **不能**在每处理一条消息后都 `InvalidateRect`。原因是：
    ///
    /// ```text
    /// GetMessage → DispatchMessage(WM_PAINT) → InvalidateRect
    ///                                        ↑              │
    ///                                        └──────────────┘
    /// ```
    ///
    /// `InvalidateRect` 会**产生一条新的 `WM_PAINT` 消息**，而 `WM_PAINT` 本身也是
    /// 一条消息 —— 于是形成自激循环：窗口永远处于"持续重绘"状态，表现为
    /// **界面不停闪烁**（且 CPU 100%）。
    ///
    /// 正确做法是**事件驱动**：
    ///
    /// | 触发条件 | 是否重绘 |
    /// |---|---|
    /// | 有输入事件（鼠标/键盘/滚轮） | ✅ |
    /// | 窗口尺寸变化（`WM_SIZE`） | ✅ |
    /// | 纯 `WM_PAINT`（系统要求重画上一次的脏区） | ❌ 不触发新的一帧 |
    /// | 其它消息 | ❌ |
    ///
    /// `WM_PAINT` 本身只**消费**已有的绘制数据（`pending.take()`），不生成新的帧。
    pub fn run_loop<F>(self, mut frame: F)
    where
        F: FnMut(&Win32Backend, &[Event]) -> DrawList,
    {
        unsafe {
            // 首帧（无事件）
            let list = frame(&self, &[]);
            shared().pending = Some(list);
            InvalidateRect(self.hwnd, core::ptr::null(), 0);
            UpdateWindow(self.hwnd);

            let mut msg: Msg = core::mem::zeroed();
            loop {
                let r = GetMessageW(&mut msg, 0, 0, 0);
                if r <= 0 {
                    break; // WM_QUIT 或错误
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);

                // ★ 只在**真的有输入事件**时才重建一帧。
                // `WM_PAINT` 本身不会产生输入事件，所以不会自激。
                let events = take_events();
                if events.is_empty() {
                    continue;
                }
                let list = frame(&self, &events);

                // ★ 进一步：内容没变就**不重绘**。
                // 鼠标在空白处移动时 hover 状态不变 → 指令逐字节相同 → 跳过重绘。
                // 这把"鼠标移动导致的 CPU 占用"从约 19% 降到接近 0。
                let unchanged = shared()
                    .pending
                    .as_ref()
                    .is_some_and(|prev| prev.same_content(&list));
                if unchanged {
                    continue;
                }

                shared().pending = Some(list);
                InvalidateRect(self.hwnd, core::ptr::null(), 0);
                // 立即呈现这一帧：动画心跳每 16ms 一条消息，若只标脏不 UpdateWindow，
                // WM_PAINT 会被不断挤到队尾，画面滞后并出现"跳帧"式的抖动。
                // 双缓冲保证这次同步绘制只是一次 BitBlt，不会闪。
                UpdateWindow(self.hwnd);
            }
        }
    }
}

/// 取出累积的输入事件。
pub fn take_events() -> Vec<Event> {
    let s = shared();
    std::mem::take(&mut s.events)
}

/// 设置清屏背景色（在首帧前调用）。
pub fn set_background(argb: u32) {
    shared().bg = argb;
}

/// 执行 UI 层请求的窗口动作（无边框窗口没有系统标题栏）。
///
/// 调用平台 API 前先把 `Shared` 里需要的值拷出来：`SendMessageW` 会同步进入
/// 模态拖动循环并重入窗口过程，那时若还持有 `&mut Shared` 就会形成别名。
pub fn perform_window_action(action: ui_core::WindowAction) {
    use ui_core::WindowAction as A;
    unsafe {
        let (hwnd, style) = {
            let s = shared();
            (s.hwnd, s.style.clone())
        };
        if hwnd == 0 {
            return;
        }
        match action {
            A::Drag => {
                // 经典做法：释放鼠标捕获后把这次按下伪装成"按在标题栏上"，
                // 系统接管拖动直到松开，比自己算鼠标偏移可靠得多。
                ReleaseCapture();
                SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION, 0);
            }
            A::Minimize => {
                ShowWindow(hwnd, SW_MINIMIZE);
            }
            A::MinimizeToTray => {
                tray_add(hwnd);
                ShowWindow(hwnd, SW_HIDE);
            }
            A::Restore => {
                restore_window(hwnd);
            }
            A::ToggleMaximize => {
                if IsZoomed(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                } else {
                    ShowWindow(hwnd, SW_MAXIMIZE);
                }
                // 尺寸变了，圆角 region 要重建（最大化时内部会自动取消裁角）。
                apply_window_effects(hwnd, &style);
            }
            A::Close => {
                PostMessageW(hwnd, WM_CLOSE, 0, 0);
            }
        }
    }
}

/// 告诉后端"还有动画在跑"。
///
/// 事件驱动的消息循环在没有输入时会睡着，动画就卡在中间态。这里按需开关一个
/// 16ms 的 `WM_TIMER` 心跳：有动画时持续出帧，动画结束后留 20 帧宽限
/// （约 320ms）确保最后一帧落到终点，再关掉定时器回到 0% CPU。
pub fn set_animation_active(active: bool) {
    unsafe {
        static mut IDLE_FRAMES: u32 = 0;
        let s = shared();
        if active {
            IDLE_FRAMES = 0;
            if !s.timer_on {
                SetTimer(s.hwnd, TIMER_ANIM, 16, core::ptr::null());
                s.timer_on = true;
            }
            s.anim_active = true;
        } else {
            s.anim_active = false;
            IDLE_FRAMES = IDLE_FRAMES.saturating_add(1);
            if IDLE_FRAMES > 20 && s.timer_on {
                KillTimer(s.hwnd, TIMER_ANIM);
                s.timer_on = false;
            }
        }
    }
}

/// 距上一帧的毫秒数（动画推进用）。
///
/// 用 `GetTickCount64`（毫秒精度）。第一帧返回 0。
pub fn frame_delta_ms() -> u32 {
    unsafe {
        let s = shared();
        let now = GetTickCount64();
        let last = s.last_tick;
        s.last_tick = now;
        if last == 0 {
            0
        } else {
            now.saturating_sub(last) as u32
        }
    }
}

impl Win32Backend {
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

/// 探测 DPI 缩放（简单实现：按主屏宽度估算）。
fn dpi_scale_probe() -> f32 {
    unsafe {
        let w = GetSystemMetrics(0); // SM_CXSCREEN
        if w >= 2560 {
            1.5
        } else {
            1.0
        }
    }
}

impl Backend for Win32Backend {
    fn size(&self) -> Size {
        let s = shared();
        Size::new(s.width, s.height)
    }

    fn dpi_scale(&self) -> f32 {
        self.scale
    }

    fn present(&mut self, list: &DrawList) {
        // 真正绘制发生在 WM_PAINT 里（由 pending 驱动）。
        shared().pending = Some(list.clone());
        unsafe {
            InvalidateRect(self.hwnd, core::ptr::null(), 0);
            UpdateWindow(self.hwnd);
        }
    }

    fn text_width(&self, text: &str, size_px: i32, bold: bool) -> i32 {
        unsafe {
            // 缓存在 Shared 里（进程内共享状态），不受 `&self` 限制。
            // 旧实现每次都调 GDI 测量，布局一帧上千次，这里修掉那个退化。
            let hdc = GetDC(self.hwnd);
            let w = measure_cached(shared(), hdc, text, size_px, bold);
            ReleaseDC(self.hwnd, hdc);
            w
        }
    }

    fn line_height(&self, size_px: i32, _bold: bool) -> i32 {
        // 与 CreateFontW 使用同一个字号缩放，否则垂直居中会随自定义字号跑偏。
        ((shared().style.font.scaled_size(size_px) as f32) * 1.3).round() as i32
    }

    /// 运行时应用外观配置：换字体、改圆角、开关磨玻璃。
    fn configure(&self, style: &ui_core::StyleConfig) {
        unsafe {
            let s = shared();
            if s.style == *style {
                return; // 每帧调用，配置没变时是纯比较
            }
            let font_changed = s.style.font != style.font;
            let window_changed =
                s.style.window_radius != style.window_radius || s.style.glass != style.glass;
            s.style = style.clone();

            if font_changed {
                // 卸载上一次的私有字体，避免重复注册。
                if let Some(old) = s.loaded_font.take() {
                    RemoveFontResourceExW(old.as_ptr(), FR_PRIVATE, core::ptr::null_mut());
                }
                if let Some(path) = &style.font.file {
                    let w = wide(path);
                    if AddFontResourceExW(w.as_ptr(), FR_PRIVATE, core::ptr::null_mut()) > 0 {
                        s.loaded_font = Some(w);
                    }
                }
                s.font_family = wide(&style.font.family);
                // 字体句柄与测量结果都失效，必须清空（否则会一直用旧字体）。
                for f in s.fonts.values() {
                    DeleteObject(*f);
                }
                s.fonts.clear();
                s.widths.clear();
            }
            if window_changed {
                apply_window_effects(s.hwnd, &s.style);
            }
        }
    }
}
