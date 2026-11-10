//! 颜色：`0xAARRGGBB` 的简单封装。
//!
//! # 为什么是 ARGB 而不是 RGBA
//!
//! Windows GDI 的 `COLORREF` 是 `0x00BBGGRR`（注意字节序：蓝绿红），而常见的
//! 网页/设计稿是 `RRGGBB`。选 `0xAARRGGBB` 是因为它**人眼可读**（`0xFF3366FF`
//! 一眼能看出是蓝），后端再做一次转换。

/// 8 位每通道，`0xAARRGGBB`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color(pub u32);

impl Color {
    pub const TRANSPARENT: Color = Color(0x0000_0000);

    /// 从不透明 RGB 构造。
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color(0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    /// 从 ARGB 构造。
    pub const fn argb(a: u8, r: u8, g: u8, b: u8) -> Self {
        Color((a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    pub const fn a(self) -> u8 {
        ((self.0 >> 24) & 0xFF) as u8
    }
    pub const fn r(self) -> u8 {
        ((self.0 >> 16) & 0xFF) as u8
    }
    pub const fn g(self) -> u8 {
        ((self.0 >> 8) & 0xFF) as u8
    }
    pub const fn b(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// 完全不透明？后端可据此跳过 alpha 混合（性能）。
    pub const fn is_opaque(self) -> bool {
        self.a() == 0xFF
    }

    /// 与另一个颜色按 `t`（0..=1）混合。用于 hover/active 状态过渡。
    pub fn lerp(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let f = |a: u8, b: u8| ((a as f32) * (1.0 - t) + (b as f32) * t).round() as u8;
        Color::argb(
            f(self.a(), other.a()),
            f(self.r(), other.r()),
            f(self.g(), other.g()),
            f(self.b(), other.b()),
        )
    }

    /// 调整亮度（`amount` 为负则变暗）。用于按下态。
    pub fn lighten(self, amount: f32) -> Color {
        let f = |c: u8| {
            let v = c as f32 + 255.0 * amount;
            v.clamp(0.0, 255.0).round() as u8
        };
        Color::argb(self.a(), f(self.r()), f(self.g()), f(self.b()))
    }
}

impl Default for Color {
    fn default() -> Self {
        Color::TRANSPARENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_sets_opaque_alpha() {
        let c = Color::rgb(0x33, 0x66, 0xFF);
        assert_eq!(c.a(), 0xFF);
        assert_eq!(c.r(), 0x33);
        assert_eq!(c.g(), 0x66);
        assert_eq!(c.b(), 0xFF);
        assert!(c.is_opaque());
    }

    #[test]
    fn argb_roundtrip() {
        let c = Color::argb(0x80, 0x11, 0x22, 0x33);
        assert_eq!((c.a(), c.r(), c.g(), c.b()), (0x80, 0x11, 0x22, 0x33));
        assert!(!c.is_opaque());
    }

    #[test]
    fn lerp_endpoints() {
        let a = Color::rgb(0, 0, 0);
        let b = Color::rgb(255, 255, 255);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        let mid = a.lerp(b, 0.5);
        assert_eq!(mid.r(), 128);
    }

    #[test]
    fn lerp_clamps() {
        let a = Color::rgb(0, 0, 0);
        let b = Color::rgb(255, 255, 255);
        assert_eq!(a.lerp(b, -5.0), a);
        assert_eq!(a.lerp(b, 5.0), b);
    }

    #[test]
    fn lighten_saturates() {
        let c = Color::rgb(200, 200, 200);
        let l = c.lighten(1.0);
        assert_eq!((l.r(), l.g(), l.b()), (255, 255, 255), "不能溢出");
        let d = c.lighten(-1.0);
        assert_eq!((d.r(), d.g(), d.b()), (0, 0, 0), "不能下溢");
    }
}
