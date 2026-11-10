//! 外观配置：字体、磨玻璃、圆角、动画。
//!
//! # 为什么放在 `ui-core`（内核零平台依赖）
//!
//! 这些是**意图**，不是平台资源：
//!
//! - "用哪个字体"是意图；"调用 `CreateFontW` 生成 HFONT"是 Win32 的事；
//! - "这块面板要磨玻璃"是意图；"调用 DWM / 对像素做 box blur"是后端的事。
//!
//! [`crate::Theme`] 持有 [`StyleConfig`]，业务代码每帧把配置交给
//! [`crate::Backend::configure`]。后端自己判断"配置变没变"，变了才重建资源。
//! 这样运行时改字体/磨玻璃不需要重建窗口，也不需要 `Ui` 认识任何平台 API。

use crate::color::Color;

/// 字体配置。
#[derive(Debug, Clone, PartialEq)]
pub struct FontConfig {
    /// 字体族名（Win32 后端传给 `CreateFontW`）。
    pub family: String,
    /// 私有字体文件路径（`.ttf` / `.otf`）。设置后先用
    /// `AddFontResourceExW(FR_PRIVATE)` 装载，再按 `family` 创建字体。
    pub file: Option<String>,
    /// 软件后端的自定义点阵字形表路径（见 `ui-backend-soft` 文档）。
    /// 软件后端没有字体引擎，只能换字形表；这一项不影响 Win32 后端。
    pub bitmap_font: Option<String>,
    /// 全局字号缩放。`1.0` = 原始字号，范围 0.75..=1.5。
    pub size_scale: f32,
}

impl FontConfig {
    /// 指定字体族。
    pub fn family(name: impl Into<String>) -> Self {
        Self {
            family: name.into(),
            ..Self::default()
        }
    }

    /// 附带一个私有字体文件。
    pub fn from_file(path: impl Into<String>, family: impl Into<String>) -> Self {
        Self {
            family: family.into(),
            file: Some(path.into()),
            ..Self::default()
        }
    }

    /// 字号缩放后的实际字号（至少 1px）。
    pub fn scaled_size(&self, size_px: i32) -> i32 {
        ((size_px as f32) * self.size_scale.clamp(0.75, 1.5))
            .round()
            .max(1.0) as i32
    }

    /// 缩放系数（已夹到合法区间）。
    pub fn scale(&self) -> f32 {
        self.size_scale.clamp(0.75, 1.5)
    }
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "Segoe UI".to_owned(),
            file: None,
            bitmap_font: None,
            size_scale: 1.0,
        }
    }
}

/// 磨玻璃配置。
///
/// `tint` 的 alpha 就是叠加强度：`0` = 完全透出背景，`255` = 完全盖住。
/// `blur_px` 是模糊半径（软件后端做真实 box blur；Win32 后端把它作为
/// 系统级 backdrop 的强度提示，面板本身用半透明 tint 近似）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlassConfig {
    pub enabled: bool,
    pub blur_px: i32,
    pub tint: Color,
}

impl GlassConfig {
    /// 开启磨玻璃并指定模糊半径。
    pub fn enabled(blur_px: i32) -> Self {
        Self {
            enabled: true,
            blur_px: blur_px.clamp(0, 48),
            ..Self::default()
        }
    }

    /// 叠加色（alpha 决定强度）。
    pub fn with_tint(mut self, tint: Color) -> Self {
        self.tint = tint;
        self
    }

    /// 模糊半径（夹到 0..=48）。
    pub fn radius(&self) -> i32 {
        self.blur_px.clamp(0, 48)
    }
}

impl Default for GlassConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            blur_px: 12,
            // 暗色玻璃：透出后面的桌面/内容，再压一层深色保证文字可读。
            tint: Color::argb(0x70, 0x18, 0x18, 0x1B),
        }
    }
}

/// 一整套外观配置。
#[derive(Debug, Clone, PartialEq)]
pub struct StyleConfig {
    pub font: FontConfig,
    pub glass: GlassConfig,
    /// 窗口整体圆角半径（逻辑像素）。`0` = 方角。
    pub window_radius: i32,
    /// 控件圆角半径（逻辑像素）。`0` = 方角。
    pub control_radius: i32,
    /// 是否启用过渡动画。关掉后所有状态切换瞬时完成。
    pub animations: bool,
}

impl StyleConfig {
    pub fn with_font(mut self, font: FontConfig) -> Self {
        self.font = font;
        self
    }

    pub fn with_glass(mut self, glass: GlassConfig) -> Self {
        self.glass = glass;
        self
    }

    pub fn with_window_radius(mut self, r: i32) -> Self {
        self.window_radius = r.clamp(0, 64);
        self
    }

    pub fn with_control_radius(mut self, r: i32) -> Self {
        self.control_radius = r.clamp(0, 32);
        self
    }

    pub fn with_animations(mut self, on: bool) -> Self {
        self.animations = on;
        self
    }

    /// 控件圆角（已夹到合法区间）。
    pub fn radius(&self) -> i32 {
        self.control_radius.clamp(0, 32)
    }
}

impl Default for StyleConfig {
    fn default() -> Self {
        Self {
            font: FontConfig::default(),
            glass: GlassConfig::default(),
            window_radius: 16,
            control_radius: 8,
            animations: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_style_is_rounded_and_animated() {
        let s = StyleConfig::default();
        assert!(s.window_radius > 0);
        assert!(s.control_radius > 0);
        assert!(s.animations);
        assert!(
            !s.glass.enabled,
            "磨玻璃默认关闭：不是所有系统都支持 backdrop"
        );
        assert_eq!(s.font.family, "Segoe UI");
    }

    #[test]
    fn font_scale_is_clamped() {
        let mut f = FontConfig {
            size_scale: 10.0,
            ..Default::default()
        };
        assert_eq!(f.scaled_size(14), 21); // 1.5 倍
        f.size_scale = 0.1;
        assert_eq!(f.scaled_size(14), 11); // 0.75 倍（14*0.75=10.5→11）
    }

    #[test]
    fn glass_radius_is_clamped() {
        let g = GlassConfig::enabled(999);
        assert_eq!(g.radius(), 48);
        let g = GlassConfig::enabled(-3);
        assert_eq!(g.radius(), 0);
        assert!(g.enabled);
    }

    #[test]
    fn builders_clamp_radii() {
        let s = StyleConfig::default()
            .with_window_radius(999)
            .with_control_radius(-1);
        assert_eq!(s.window_radius, 64);
        assert_eq!(s.control_radius, 0);
        assert_eq!(s.radius(), 0);
    }

    #[test]
    fn tint_alpha_is_the_intensity() {
        let g = GlassConfig::enabled(8).with_tint(Color::argb(0x40, 0, 0, 0));
        assert_eq!(g.tint.a(), 0x40);
    }
}
