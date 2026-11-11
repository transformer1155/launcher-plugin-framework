//! 软件光栅后端：把 [`DrawList`] 画进一块**内存像素缓冲**。
//!
//! # 它存在的三个理由
//!
//! 1. **可测试**：CI 没有窗口系统，无法验证 Win32 后端的画面。软件后端让
//!    "布局与绘制是否正确"变成可以断言的事情（检查像素颜色）。
//! 2. **可截图**：CI/文档需要界面截图时不必真开窗口，直接导出 PNG/PPM。
//! 3. **跨平台兜底**：没有窗口系统的环境（服务器、容器）也能渲染。
//!
//! # 字形
//!
//! 不含字体引擎。文字用**内置点阵**渲染：
//!
//! - ASCII 用 5×7 点阵（覆盖缩写/数字/符号，界面里够用）；
//! - 非 ASCII（中文）用**方块占位**（后端会画出可见的方块提示"这里需要真实字体"）。
//!
//! 这看起来是妥协，但它是**刻意**的：引入 `ab_glyph`/`fontdue` 需要第三方依赖，
//! 而"自制 UI 系统"的前提是零依赖。真实文字渲染由 Win32 后端负责；
//! 软件后端只为**布局验证**服务，用点阵完全够用。
//!
//! # 与 Win32 后端的差异（会影响截图可信度，必须写清）
//!
//! | 项目 | Win32 后端 | 软件后端 |
//! |---|---|---|
//! | 文字 | 真实系统字体（ClearType） | 5×7 点阵 / 方块 |
//! | 圆角 | 无（用矩形） | 无 |
//! | 抗锯齿 | 系统 | 无（纯色） |
//! | 尺寸来源 | 真实 DPI | 构造时指定 |
//!
//! 因此**软件后端的截图不能当作"最终界面长什么样"**，它验证的是布局与配色。

#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::collections::HashMap;

use ui_core::backend::Backend;
use ui_core::draw::{DrawList, DrawOp, TextAlign};
use ui_core::layout::{Rect, Size};
use ui_core::style::StyleConfig;

/// 一个 RGBA 像素缓冲。
pub struct SoftBackend {
    width: i32,
    height: i32,
    /// `0xAARRGGBB`，按行存储。`alpha == 0` 表示"窗口圆角之外的透明像素"。
    pixels: Vec<u32>,
    scale: f32,
    /// 文本宽度缓存。
    ///
    /// 用 `RefCell` 是因为 `Backend::text_width` 的签名是 `&self`，而测量结果必须缓存
    /// （否则每帧数千次点阵遍历）。后端是单线程使用的，`RefCell` 足够。
    font_cache: RefCell<HashMap<(String, i32, bool), i32>>,
    /// 当前外观配置（`configure` 每帧调用，幂等）。
    style: RefCell<StyleConfig>,
    /// 自定义点阵字形表（覆盖内置 5×7 字形）。
    custom_glyphs: RefCell<HashMap<char, [u8; 7]>>,
    /// 剪贴板（软后端进程内模拟）。
    clipboard: Rc<RefCell<String>>,
    /// 是否模拟"窗口圆角"。只有被 `Ui::begin_frame` 驱动时才开启，
    /// 直接调用 `present()` 做单元测试时保持方角（否则会遮掉测试像素）。
    round_window: RefCell<bool>,
    /// PPM 不支持 alpha：透明像素导出时用它代替（模拟桌面底色）。
    backdrop: u32,
}

impl SoftBackend {
    pub fn new(width: i32, height: i32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        Self {
            width,
            height,
            pixels: vec![0u32; (width * height) as usize],
            scale: 1.0,
            font_cache: RefCell::new(HashMap::new()),
            style: RefCell::new(StyleConfig::default()),
            custom_glyphs: RefCell::new(HashMap::new()),
            round_window: RefCell::new(false),
            clipboard: std::rc::Rc::new(std::cell::RefCell::new(String::new())),
            backdrop: 0xFF0E_0E12,
        }
    }

    /// 读取进程内剪贴板（供测试断言 Ctrl+C 的结果）。
    pub fn clipboard_text(&self) -> String {
        self.clipboard.borrow().clone()
    }

    /// 预置进程内剪贴板（供测试模拟"外部复制了文本"再 Ctrl+V）。
    pub fn set_clipboard_text(&self, s: &str) {
        *self.clipboard.borrow_mut() = s.to_owned();
    }

    pub fn with_scale(mut self, scale: f32) -> Self {
        self.scale = scale.clamp(0.5, 4.0);
        self
    }

    /// 设置 PPM 导出时透明圆角的替代色。
    pub fn with_backdrop(mut self, argb: u32) -> Self {
        self.backdrop = argb;
        self
    }

    /// 直接装载一份自定义点阵字形表（不依赖文件，便于测试）。
    ///
    /// 格式：每行一个字符 + 7 个字节，`A 0e 11 11 1f 11 11 11`（十六进制）。
    /// 也接受 `U+0041` 形式的码点。空行与 `#` 注释行忽略。
    pub fn set_bitmap_font_text(&self, text: &str) -> usize {
        let glyphs = parse_bitmap_font(text);
        let n = glyphs.len();
        *self.custom_glyphs.borrow_mut() = glyphs;
        self.font_cache.borrow_mut().clear();
        n
    }

    /// 当前字形（自定义表优先，其次内置 5×7 点阵）。
    fn glyph(&self, ch: char) -> Option<[u8; 7]> {
        if let Some(g) = self.custom_glyphs.borrow().get(&ch.to_ascii_uppercase()) {
            return Some(*g);
        }
        // 标题栏按钮图标：自定义表优先，其次内置点阵（否则截图里是空心方块）。
        if let Some(g) = caption_glyph(ch) {
            return Some(g);
        }
        glyph_5x7(ch).copied()
    }

    /// 字号缩放后的实际字号。
    fn scaled(&self, size_px: i32) -> i32 {
        self.style.borrow().font.scaled_size(size_px)
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn pixel_at(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return 0;
        }
        self.pixels[(y * self.width + x) as usize]
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// 清屏为指定颜色。
    pub fn clear(&mut self, argb: u32) {
        self.pixels.fill(argb);
    }

    /// 填充矩形（含 alpha 混合）。
    fn fill_rect(&mut self, r: Rect, argb: u32) {
        let a = (argb >> 24) & 0xFF;
        if a == 0 {
            return;
        }
        let clip = r.intersect(Rect::new(0, 0, self.width, self.height));
        if clip.is_empty() {
            return;
        }
        for y in clip.y..clip.bottom() {
            for x in clip.x..clip.right() {
                let idx = (y * self.width + x) as usize;
                if a == 0xFF {
                    self.pixels[idx] = argb;
                } else {
                    self.pixels[idx] = blend(self.pixels[idx], argb, a);
                }
            }
        }
    }

    /// 把某个像素按 alpha 混合（圆角/描边复用）。
    fn put_blend(&mut self, x: i32, y: i32, argb: u32) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let a = (argb >> 24) & 0xFF;
        if a == 0 {
            return;
        }
        let idx = (y * self.width + x) as usize;
        self.pixels[idx] = if a == 0xFF {
            argb
        } else {
            blend(self.pixels[idx], argb, a)
        };
    }

    /// 填充圆角矩形（逐像素判定，四个角是真正的圆弧）。
    fn fill_round(&mut self, r: Rect, argb: u32, radius: i32) {
        let a = (argb >> 24) & 0xFF;
        if a == 0 {
            return;
        }
        let clip = r.intersect(Rect::new(0, 0, self.width, self.height));
        if clip.is_empty() {
            return;
        }
        for y in clip.y..clip.bottom() {
            for x in clip.x..clip.right() {
                if round_contains(r, radius, x, y) {
                    self.put_blend(x, y, argb);
                }
            }
        }
    }

    /// 圆角描边：外圆角减去内缩后的内圆角。
    fn stroke_round(&mut self, r: Rect, argb: u32, radius: i32, thickness: i32) {
        let t = thickness.max(1).min(r.w.min(r.h) / 2);
        let inner = Rect::new(r.x + t, r.y + t, (r.w - 2 * t).max(0), (r.h - 2 * t).max(0));
        let inner_radius = (radius - t).max(0);
        let clip = r.intersect(Rect::new(0, 0, self.width, self.height));
        for y in clip.y..clip.bottom() {
            for x in clip.x..clip.right() {
                let outside = round_contains(r, radius, x, y);
                let inside = !inner.is_empty() && round_contains(inner, inner_radius, x, y);
                if outside && !inside {
                    self.put_blend(x, y, argb);
                }
            }
        }
    }

    /// 磨玻璃：对**当前已绘制像素**做 box blur，再叠一层 tint。
    fn blur_region(&mut self, r: Rect, radius: i32, tint: u32) {
        let clip = r.intersect(Rect::new(0, 0, self.width, self.height));
        if clip.is_empty() {
            return;
        }
        let w = clip.w as usize;
        let h = clip.h as usize;
        let rad = radius.clamp(0, 48) as usize;
        if rad > 0 {
            // 先把区域拷出来，模糊完再写回（避免读到本 pass 已改过的像素）。
            let mut buf = vec![0u32; w * h];
            for y in 0..h {
                for x in 0..w {
                    buf[y * w + x] = self.pixels
                        [((clip.y + y as i32) * self.width + clip.x + x as i32) as usize];
                }
            }
            // 两趟 box blur 已足够接近高斯，且比逐点窗口求和快得多（积分图）。
            box_blur_pass(&mut buf, w, h, rad);
            box_blur_pass(&mut buf, w, h, rad);
            for y in 0..h {
                for x in 0..w {
                    self.pixels[((clip.y + y as i32) * self.width + clip.x + x as i32) as usize] =
                        buf[y * w + x];
                }
            }
        }
        let a = (tint >> 24) & 0xFF;
        if a > 0 {
            self.fill_rect(clip, tint);
        }
    }

    /// 模拟窗口圆角：把四角以外的像素清成透明（PPM 导出时用 backdrop 代替）。
    fn apply_window_corners(&mut self) {
        if !*self.round_window.borrow() {
            return;
        }
        let radius = self.style.borrow().window_radius;
        if radius <= 0 {
            return;
        }
        let full = Rect::new(0, 0, self.width, self.height);
        // 只需检查四个角的 r×r 方块，中间区域一定在内。
        let r = radius.min(self.width.min(self.height) / 2);
        for y in 0..r {
            for x in 0..r {
                let corners = [
                    (x, y),
                    (self.width - 1 - x, y),
                    (x, self.height - 1 - y),
                    (self.width - 1 - x, self.height - 1 - y),
                ];
                for (cx, cy) in corners {
                    if !round_contains(full, r, cx, cy) {
                        self.pixels[(cy * self.width + cx) as usize] = 0;
                    }
                }
            }
        }
    }

    /// 画 5×7 点阵字符。
    fn draw_glyph(&mut self, ch: char, x: i32, y: i32, px_size: i32, argb: u32, bold: bool) {
        let Some(bitmap) = self.glyph(ch) else {
            // 非 ASCII：画一个空心方块，明确表示"这是占位"
            let s = px_size.max(4);
            let box_r = Rect::new(x, y, s, s);
            for yy in box_r.y..box_r.bottom() {
                for xx in box_r.x..box_r.right() {
                    let edge = xx == box_r.x
                        || xx == box_r.right() - 1
                        || yy == box_r.y
                        || yy == box_r.bottom() - 1;
                    if edge {
                        self.put_px(xx, yy, argb);
                    }
                }
            }
            return;
        };

        // 点阵 5 宽 7 高，按目标像素放大：每个点占 `dot` 个像素
        let dot = (px_size as f32 / 7.0).round().max(1.0) as i32;
        for (row, bits) in bitmap.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) != 0 {
                    let px = x + col * dot;
                    let py = y + (row as i32) * dot;
                    self.fill_rect(Rect::new(px, py, dot, dot), argb);
                    if bold {
                        // 简易加粗：右侧再补一列
                        self.fill_rect(Rect::new(px + dot, py, dot, dot), argb);
                    }
                }
            }
        }
    }

    fn put_px(&mut self, x: i32, y: i32, argb: u32) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        self.pixels[(y * self.width + x) as usize] = argb;
    }

    /// 导出为 PPM（P6）。无需第三方库，任何图片查看器都能打开。
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.width * self.height * 3 + 64) as usize);
        out.extend_from_slice(format!("P6\n{} {}\n255\n", self.width, self.height).as_bytes());
        for px in &self.pixels {
            // alpha == 0 = 窗口圆角之外；PPM 没有 alpha 通道，用底色代替。
            let v = if (px >> 24) & 0xFF == 0 {
                self.backdrop
            } else {
                *px
            };
            out.push(((v >> 16) & 0xFF) as u8); // R
            out.push(((v >> 8) & 0xFF) as u8); // G
            out.push((v & 0xFF) as u8); // B
        }
        out
    }

    /// 统计某种颜色的像素数（测试用）。
    pub fn count_color(&self, argb: u32) -> usize {
        self.pixels.iter().filter(|p| **p == argb).count()
    }
}

/// 简单 alpha 混合（over 运算）。
fn blend(dst: u32, src: u32, a: u32) -> u32 {
    let sa = (a as f32) / 255.0;
    let da = ((dst >> 24) & 0xFF) as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        return 0;
    }
    let ch = |shift: u32| {
        let s = ((src >> shift) & 0xFF) as f32;
        let d = ((dst >> shift) & 0xFF) as f32;
        (((s * sa + d * da * (1.0 - sa)) / out_a).round() as u32) & 0xFF
    };
    let a_out = (out_a * 255.0).round() as u32;
    (a_out << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// 像素中心是否落在圆角矩形内。
///
/// 判据：把矩形四角"内缩 r"得到一个核心矩形，像素中心到该核心矩形的距离
/// 不超过 r 即在内。这等价于标准的圆角矩形 SDF，且只涉及一次 clamp。
fn round_contains(rect: Rect, radius: i32, x: i32, y: i32) -> bool {
    if !rect.contains(x, y) {
        return false;
    }
    let r = radius.max(0).min(rect.w.min(rect.h) / 2);
    if r == 0 {
        return true;
    }
    let px = x as f64 + 0.5;
    let py = y as f64 + 0.5;
    let l = rect.x as f64 + r as f64;
    let right = rect.right() as f64 - r as f64;
    let t = rect.y as f64 + r as f64;
    let b = rect.bottom() as f64 - r as f64;
    let cx = px.clamp(l, right);
    let cy = py.clamp(t, b);
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= (r as f64) * (r as f64)
}

/// 一次可分离 box blur（用二维积分图，复杂度与半径无关）。
///
/// 两趟叠加后接近高斯模糊。只处理 RGB，alpha 保持不透明 —— 模糊的是
/// "已经画好的内容"，不是窗口合成层。
fn box_blur_pass(buf: &mut [u32], w: usize, h: usize, r: usize) {
    if w == 0 || h == 0 || r == 0 {
        return;
    }
    let stride = w + 1;
    let mut integral = vec![[0u64; 3]; stride * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            let p = buf[y * w + x];
            let rgb = [
                ((p >> 16) & 0xFF) as u64,
                ((p >> 8) & 0xFF) as u64,
                (p & 0xFF) as u64,
            ];
            let idx = (y + 1) * stride + x + 1;
            let top = integral[y * stride + x + 1];
            let left = integral[idx - 1];
            let diag = integral[y * stride + x];
            let mut cur = [0u64; 3];
            for c in 0..3 {
                cur[c] = rgb[c] + top[c] + left[c] - diag[c];
            }
            integral[idx] = cur;
        }
    }
    let at = |xx: usize, yy: usize, c: usize| -> u64 { integral[yy * stride + xx][c] };
    for y in 0..h {
        for x in 0..w {
            let x0 = x.saturating_sub(r);
            let x1 = (x + r + 1).min(w);
            let y0 = y.saturating_sub(r);
            let y1 = (y + r + 1).min(h);
            let n = ((x1 - x0) * (y1 - y0)) as u64;
            if n == 0 {
                continue;
            }
            let mut out = 0xFF00_0000u32;
            for c in 0..3 {
                let s = at(x1, y1, c) + at(x0, y0, c) - at(x1, y0, c) - at(x0, y1, c);
                let v = (s / n).min(255) as u32;
                out |= v << (16 - 8 * c);
            }
            buf[y * w + x] = out;
        }
    }
}

/// 解析自定义点阵字形表。
///
/// 每行：`<字符|U+XXXX> <7 个字节>`，字节可用十六进制（`0e` / `0x0e`）或
/// 二进制（`0b01110`），分隔符可以是空格或逗号。`#` 开头是注释。
fn parse_bitmap_font(text: &str) -> HashMap<char, [u8; 7]> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty());
        let Some(head) = parts.next() else { continue };
        let ch = head
            .strip_prefix("U+")
            .or_else(|| head.strip_prefix("u+"))
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .and_then(char::from_u32)
            .or_else(|| head.chars().next());
        let Some(ch) = ch else { continue };
        let mut glyph = [0u8; 7];
        let mut ok = true;
        for slot in &mut glyph {
            let Some(tok) = parts.next() else {
                ok = false;
                break;
            };
            let bits = if let Some(b) = tok.strip_prefix("0b") {
                u32::from_str_radix(b, 2).ok()
            } else {
                let hex = tok.strip_prefix("0x").unwrap_or(tok);
                u32::from_str_radix(hex, 16).ok()
            };
            match bits {
                Some(v) if v <= 0xFF => *slot = v as u8,
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            out.insert(ch.to_ascii_uppercase(), glyph);
        }
    }
    out
}

impl Backend for SoftBackend {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }

    fn dpi_scale(&self) -> f32 {
        self.scale
    }

    fn present(&mut self, list: &DrawList) {
        for op in list.ops() {
            match op {
                DrawOp::FillRect { rect, color } => self.fill_rect(*rect, color.0),
                DrawOp::FillRoundRect {
                    rect,
                    color,
                    radius,
                } => self.fill_round(*rect, color.0, *radius),
                DrawOp::StrokeRoundRect {
                    rect,
                    color,
                    radius,
                    thickness,
                } => self.stroke_round(*rect, color.0, *radius, *thickness),
                DrawOp::BlurRegion { rect, radius, tint } => {
                    self.blur_region(*rect, *radius, tint.0);
                }
                DrawOp::StrokeRect {
                    rect,
                    color,
                    thickness,
                } => {
                    // 软件后端不常收到 StrokeRect（UI 内核会展开成 4 个 FillRect），
                    // 但仍支持以便外部直接构造 DrawList。
                    let t = *thickness;
                    self.fill_rect(Rect::new(rect.x, rect.y, rect.w, t), color.0);
                    self.fill_rect(Rect::new(rect.x, rect.bottom() - t, rect.w, t), color.0);
                    self.fill_rect(Rect::new(rect.x, rect.y, t, rect.h), color.0);
                    self.fill_rect(Rect::new(rect.right() - t, rect.y, t, rect.h), color.0);
                }
                DrawOp::Text {
                    rect,
                    text,
                    color,
                    size_px,
                    align,
                    bold,
                } => {
                    let tw = self.text_width(text, *size_px, *bold);
                    let x = match align {
                        TextAlign::Left => rect.x,
                        TextAlign::Center => rect.x + (rect.w - tw) / 2,
                        TextAlign::Right => rect.right() - tw,
                    };
                    let px = self.scaled(*size_px);
                    let dot = (px as f32 / 7.0).round().max(1.0) as i32;
                    let mut cx = x;
                    for ch in text.chars() {
                        let adv = if self.glyph(ch).is_some() {
                            // 5 列 + 1 列间隔
                            (5 * dot) + dot + if *bold { dot } else { 0 }
                        } else {
                            // 方块占位
                            px.max(4) + dot
                        };
                        self.draw_glyph(ch, cx, rect.y, px, color.0, *bold);
                        cx += adv;
                    }
                }
                DrawOp::Line {
                    rect,
                    color,
                    horizontal,
                } => {
                    let r = if *horizontal {
                        Rect::new(rect.x, rect.y, rect.w, rect.h.max(1))
                    } else {
                        Rect::new(rect.x, rect.y, rect.w.max(1), rect.h)
                    };
                    self.fill_rect(r, color.0);
                }
                // DrawList 已把所有几何裁到当前裁剪区，这里无需再维护裁剪栈。
                DrawOp::PushClip { .. } | DrawOp::PopClip => {}
            }
        }
        self.apply_window_corners();
    }

    fn text_width(&self, text: &str, size_px: i32, bold: bool) -> i32 {
        let key = (text.to_owned(), size_px, bold);
        if let Some(w) = self.font_cache.borrow().get(&key) {
            return *w;
        }
        let px = self.scaled(size_px);
        let dot = (px as f32 / 7.0).round().max(1.0) as i32;
        let mut w = 0;
        for ch in text.chars() {
            w += if self.glyph(ch).is_some() {
                (5 * dot) + dot + if bold { dot } else { 0 }
            } else {
                px.max(4) + dot
            };
        }
        self.font_cache.borrow_mut().insert(key, w);
        w
    }

    fn line_height(&self, size_px: i32, _bold: bool) -> i32 {
        (self.scaled(size_px) as f32 * 1.3).round() as i32
    }

    /// 每帧由 `Ui::begin_frame` 调用。配置没变时是纯比较，不重建任何资源。
    fn configure(&self, style: &StyleConfig) {
        // 只要被 UI 帧驱动过，就按"窗口有圆角"渲染（离屏截图也呈现圆角）。
        // 直接 present() 的单元测试不受影响。
        *self.round_window.borrow_mut() = true;
        let mut cur = self.style.borrow_mut();
        if *cur == *style {
            return;
        }
        let font_changed = cur.font != style.font;
        *cur = style.clone();
        drop(cur);
        if font_changed {
            self.font_cache.borrow_mut().clear();
            // 自定义点阵表：路径变了就重新读；清空则回到内置字形。
            let path = self.style.borrow().font.bitmap_font.clone();
            let mut glyphs = HashMap::new();
            if let Some(p) = path {
                if let Ok(text) = std::fs::read_to_string(p) {
                    glyphs = parse_bitmap_font(&text);
                }
            }
            *self.custom_glyphs.borrow_mut() = glyphs;
        }
    }

    fn clipboard_get(&self) -> Option<String> {
        let s = self.clipboard.borrow();
        if s.is_empty() {
            None
        } else {
            Some(s.clone())
        }
    }

    fn clipboard_set(&self, text: &str) {
        *self.clipboard.borrow_mut() = text.to_owned();
    }
}

/// 自绘标题栏三个窗口按钮图标的点阵字形。
///
/// 软件后端没有 CJK/符号字体，`— □ ✕` 若不在表里就会退化成空心方块占位。
/// 真实 Win32 窗口由系统字体渲染，不受影响；这里只保证离屏截图可辨认。
fn caption_glyph(ch: char) -> Option<[u8; 7]> {
    const MIN: [u8; 7] = [0, 0, 0, 0b11111, 0, 0, 0];
    const MAX: [u8; 7] = [
        0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111,
    ];
    const CLOSE: [u8; 7] = [
        0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b01010, 0b10001,
    ];
    match ch {
        '—' | '−' | '–' => Some(MIN),
        '□' | '▢' => Some(MAX),
        '✕' | '✖' | '×' => Some(CLOSE),
        _ => None,
    }
}

/// 5×7 点阵字形。每行 5 位，`1` 表示点亮。
///
/// 只覆盖**界面里真正用到的字符**：大写字母、数字、常见符号。
/// 小写映射到对应大写（启动器界面文字主要靠 Win32 后端渲染真实字体，
/// 软件后端只需可辨认）。
fn glyph_5x7(ch: char) -> Option<&'static [u8; 7]> {
    // A 最后一行必须是 `10001`（两条腿）—— 之前误写成 `01110`（收口），
    // 导致 A 看起来像 O/0（界面上 `JAR` 渲染成 `J0R`）。
    static A: [u8; 7] = [
        0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
    ];
    static B: [u8; 7] = [
        0b11111, 0b10001, 0b11111, 0b10001, 0b11111, 0b10001, 0b11111,
    ];
    static C: [u8; 7] = [
        0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
    ];
    static D: [u8; 7] = [
        0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
    ];
    static E: [u8; 7] = [
        0b11111, 0b10000, 0b11110, 0b10000, 0b11111, 0b10000, 0b11111,
    ];
    static F: [u8; 7] = [
        0b11111, 0b10000, 0b11110, 0b10000, 0b11111, 0b10000, 0b10000,
    ];
    static G: [u8; 7] = [
        0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
    ];
    static H: [u8; 7] = [
        0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
    ];
    static I: [u8; 7] = [
        0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ];
    static J: [u8; 7] = [
        0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
    ];
    static K: [u8; 7] = [
        0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
    ];
    static L: [u8; 7] = [
        0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
    ];
    static M: [u8; 7] = [
        0b10001, 0b11011, 0b10101, 0b10101, 0b10101, 0b11011, 0b10001,
    ];
    static N: [u8; 7] = [
        0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b10001, 0b10001,
    ];
    static O: [u8; 7] = [
        0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
    ];
    static P: [u8; 7] = [
        0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
    ];
    static Q: [u8; 7] = [
        0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10011, 0b01101,
    ];
    static R: [u8; 7] = [
        0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b10001,
    ];
    static S: [u8; 7] = [
        0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
    ];
    static T: [u8; 7] = [
        0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
    ];
    static U: [u8; 7] = [
        0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
    ];
    static V: [u8; 7] = [
        0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
    ];
    static W: [u8; 7] = [
        0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b10101, 0b01010,
    ];
    static X: [u8; 7] = [
        0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b01010, 0b10001,
    ];
    static Y: [u8; 7] = [
        0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
    ];
    static Z: [u8; 7] = [
        0b11111, 0b10000, 0b01000, 0b00100, 0b00010, 0b00001, 0b11111,
    ];
    // 数字点阵（标准 5×7 字形）。
    static N0: [u8; 7] = [
        0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
    ];
    static N1: [u8; 7] = [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ];
    static N2: [u8; 7] = [
        0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
    ];
    static N3: [u8; 7] = [
        0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
    ];
    static N4: [u8; 7] = [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ];
    static N5: [u8; 7] = [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ];
    static N6: [u8; 7] = [
        0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ];
    static N7: [u8; 7] = [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ];
    static N8: [u8; 7] = [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ];
    static N9: [u8; 7] = [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
    ];
    static SPACE: [u8; 7] = [0, 0, 0, 0, 0, 0, 0];
    static DOT: [u8; 7] = [0, 0, 0, 0, 0, 0b01100, 0b01100];
    static DASH: [u8; 7] = [0, 0, 0, 0b11111, 0, 0, 0];
    static COLON: [u8; 7] = [0, 0b00110, 0b00110, 0, 0b00110, 0b00110, 0];
    static SLASH: [u8; 7] = [0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0, 0];
    static QUESTION: [u8; 7] = [0b01110, 0b00001, 0b00001, 0b00110, 0b00010, 0, 0b00100];

    match ch.to_ascii_uppercase() {
        'A' => Some(&A),
        'B' => Some(&B),
        'C' => Some(&C),
        'D' => Some(&D),
        'E' => Some(&E),
        'F' => Some(&F),
        'G' => Some(&G),
        'H' => Some(&H),
        'I' => Some(&I),
        'J' => Some(&J),
        'K' => Some(&K),
        'L' => Some(&L),
        'M' => Some(&M),
        'N' => Some(&N),
        'O' => Some(&O),
        'P' => Some(&P),
        'Q' => Some(&Q),
        'R' => Some(&R),
        'S' => Some(&S),
        'T' => Some(&T),
        'U' => Some(&U),
        'V' => Some(&V),
        'W' => Some(&W),
        'X' => Some(&X),
        'Y' => Some(&Y),
        'Z' => Some(&Z),
        '0' => Some(&N0),
        '1' => Some(&N1),
        '2' => Some(&N2),
        '3' => Some(&N3),
        '4' => Some(&N4),
        '5' => Some(&N5),
        '6' => Some(&N6),
        '7' => Some(&N7),
        '8' => Some(&N8),
        '9' => Some(&N9),
        ' ' => Some(&SPACE),
        '.' => Some(&DOT),
        '-' => Some(&DASH),
        ':' => Some(&COLON),
        '/' => Some(&SLASH),
        '?' => Some(&QUESTION),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_core::color::Color;

    #[test]
    fn fill_rect_sets_pixels() {
        let mut b = SoftBackend::new(10, 10);
        b.fill_rect(Rect::new(0, 0, 5, 5), 0xFFFF_0000);
        assert_eq!(b.pixel_at(0, 0), 0xFFFF_0000);
        assert_eq!(b.pixel_at(4, 4), 0xFFFF_0000);
        assert_eq!(b.pixel_at(5, 5), 0, "界外不应被填");
    }

    #[test]
    fn fill_rect_clips_out_of_bounds() {
        let mut b = SoftBackend::new(4, 4);
        // 部分越界：不应 panic
        b.fill_rect(Rect::new(-5, -5, 10, 10), 0xFF00_FF00);
        assert_eq!(b.pixel_at(0, 0), 0xFF00_FF00);
        b.fill_rect(Rect::new(100, 100, 10, 10), 0xFF00_00FF);
        // 完全越界：不 panic
    }

    #[test]
    fn alpha_blend_mixes() {
        let mut b = SoftBackend::new(2, 2);
        b.fill_rect(Rect::new(0, 0, 2, 2), 0xFF00_0000);
        // 50% 白
        b.fill_rect(Rect::new(0, 0, 2, 2), 0x80FF_FFFF);
        let px = b.pixel_at(0, 0);
        let r = (px >> 16) & 0xFF;
        assert!(r > 100 && r < 160, "半透明白应产生中间灰，实际 r={r}");
    }

    #[test]
    fn transparent_fill_is_noop() {
        let mut b = SoftBackend::new(2, 2);
        b.fill_rect(Rect::new(0, 0, 2, 2), 0x00FF_FFFF);
        assert_eq!(b.pixel_at(0, 0), 0);
    }

    #[test]
    fn text_width_is_positive_and_grows() {
        let b = SoftBackend::new(100, 100);
        assert!(b.text_width("A", 14, false) > 0);
        assert!(b.text_width("AB", 14, false) > b.text_width("A", 14, false));
        assert_eq!(b.text_width("", 14, false), 0);
    }

    #[test]
    fn text_width_is_cached() {
        let b = SoftBackend::new(100, 100);
        let w1 = b.text_width("HELLO", 14, false);
        let w2 = b.text_width("HELLO", 14, false);
        assert_eq!(w1, w2);
        assert_eq!(b.font_cache.borrow().len(), 1, "应命中缓存");
    }

    #[test]
    fn present_draws_text_pixels() {
        let mut b = SoftBackend::new(40, 20);
        let mut list = DrawList::new();
        list.text(
            Rect::new(0, 0, 40, 20),
            "AB",
            Color::rgb(255, 255, 255),
            14,
            TextAlign::Left,
            false,
        );
        b.present(&list);
        // 应当有白色像素被画出来
        assert!(b.count_color(0xFFFF_FFFF) > 0, "文字应当产生像素");
    }

    #[test]
    fn non_ascii_draws_placeholder_box() {
        let mut b = SoftBackend::new(30, 20);
        let mut list = DrawList::new();
        list.text(
            Rect::new(0, 0, 30, 20),
            "中",
            Color::rgb(255, 0, 0),
            14,
            TextAlign::Left,
            false,
        );
        b.present(&list);
        assert!(
            b.count_color(0xFFFF_0000) > 0,
            "非 ASCII 应画出具可见的占位方块"
        );
    }

    #[test]
    fn ppm_header_is_valid() {
        let b = SoftBackend::new(4, 3);
        let ppm = b.to_ppm();
        let head = String::from_utf8_lossy(&ppm[..ppm.len().min(20)]);
        assert!(head.starts_with("P6\n4 3\n255\n"), "PPM 头应合法：{head}");
        assert_eq!(ppm.len(), b"P6\n4 3\n255\n".len() + (4 * 3 * 3));
    }

    #[test]
    fn scale_is_reported() {
        let b = SoftBackend::new(100, 100).with_scale(1.5);
        assert_eq!(b.dpi_scale(), 1.5);
    }

    #[test]
    fn size_matches_construction() {
        let b = SoftBackend::new(320, 200);
        assert_eq!(b.size(), Size::new(320, 200));
        assert_eq!(b.width(), 320);
        assert_eq!(b.height(), 200);
    }

    #[test]
    fn fill_round_keeps_corners_clear() {
        let mut b = SoftBackend::new(20, 20);
        b.clear(0xFF00_0000);
        let mut list = DrawList::new();
        list.fill_round_rect(Rect::new(0, 0, 20, 20), Color::rgb(255, 255, 255), 8);
        b.present(&list);
        assert_eq!(b.pixel_at(0, 0), 0xFF00_0000, "圆角外的角必须保留底色");
        assert_eq!(b.pixel_at(10, 10), 0xFFFF_FFFF, "中心必须被填充");
    }

    #[test]
    fn configure_clips_window_corners_to_transparent() {
        let mut list = DrawList::new();
        list.fill_rect(Rect::new(0, 0, 40, 40), Color::rgb(255, 255, 255));

        let mut b = SoftBackend::new(40, 40);
        b.configure(&StyleConfig::default().with_window_radius(12));
        b.clear(0xFFFF_FFFF);
        b.present(&list);
        assert_eq!(b.pixel_at(0, 0), 0, "左上角应被裁成透明");
        assert_eq!(b.pixel_at(20, 20), 0xFFFF_FFFF, "中心不应被裁掉");

        // 半径 0 = 方角：即使被 UI 帧驱动过也不能裁剪。
        let mut square = SoftBackend::new(40, 40);
        square.configure(&StyleConfig::default().with_window_radius(0));
        square.clear(0xFFFF_FFFF);
        square.present(&list);
        assert_eq!(square.pixel_at(0, 0), 0xFFFF_FFFF);
    }

    #[test]
    fn blur_region_softens_a_hard_edge() {
        let mut b = SoftBackend::new(20, 10);
        b.clear(0xFF00_0000);
        b.fill_rect(Rect::new(0, 0, 10, 10), 0xFFFF_FFFF);
        assert_eq!(b.pixel_at(10, 5), 0xFF00_0000);

        let mut list = DrawList::new();
        // tint alpha = 0：只验证模糊，不叠加颜色。
        list.blur_region(Rect::new(0, 0, 20, 10), 3, Color::argb(0, 0, 0, 0));
        b.present(&list);
        let after = b.pixel_at(10, 5);
        assert_ne!(after, 0xFF00_0000, "模糊必须把白色渗过硬边界");
        assert!((after & 0xFF) > 0);
    }

    #[test]
    fn custom_bitmap_font_overrides_builtin_glyph() {
        let b = SoftBackend::new(20, 20);
        let builtin = b.glyph('A').expect("内置点阵应有 A");
        let n = b.set_bitmap_font_text(
            "# 注释行\nA 00 00 00 00 00 00 00\nU+0042 1f 1f 1f 1f 1f 1f 1f\n",
        );
        assert_eq!(n, 2, "注释行被忽略，两种字符写法各解析出一个字形");
        assert_ne!(b.glyph('A'), Some(builtin), "自定义 A 必须覆盖内置 A");
        assert_eq!(b.glyph('a').unwrap(), [0u8; 7], "字形按大写归一");
        assert!(b.glyph('B').is_some(), "U+0042 形式也要能解析");
    }

    #[test]
    fn caption_button_glyphs_are_built_in() {
        let mut b = SoftBackend::new(80, 30);
        // 无边框标题栏的三个按钮图标：软件后端截图不能退化成方块占位。
        for ch in ['—', '□', '✕'] {
            assert!(b.glyph(ch).is_some(), "{ch} 应有内置点阵字形");
        }
        let mut list = DrawList::new();
        list.text(
            Rect::new(0, 0, 60, 14),
            "—□✕",
            Color::rgb(255, 255, 255),
            14,
            TextAlign::Left,
            false,
        );
        b.present(&list);
        let painted = (0..60).any(|x| b.pixel_at(x, 4) != 0);
        assert!(painted, "三个图标应真的画出像素，而不是留空");
    }
}
