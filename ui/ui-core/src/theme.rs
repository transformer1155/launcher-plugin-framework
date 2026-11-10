//! 主题：所有视觉常量集中在这里。
//!
//! # 为什么主题是结构体而不是一堆散落的常量
//!
//! 启动器需要"暗色/亮色"切换（至少暗色是默认）。把颜色/尺寸收进 [`Theme`]
//! 后，切换主题 = 换一个结构体实例，业务代码里没有任何 `if dark { ... }`。
//!
//! # 尺寸单位
//!
//! 全部是**逻辑像素**。缩放（DPI）由后端处理 —— 见 [`crate::Backend::dpi_scale`]。
//! 这一点很重要：如果业务代码自己乘 DPI，那每个控件都要记得乘，必错；
//! 让后端统一乘一次，业务代码永远写逻辑像素。

use crate::color::Color;
use crate::style::StyleConfig;

/// 暗色主题（默认）。
#[derive(Debug, Clone)]
pub struct Theme {
    // ---- 颜色 ----
    /// 窗口背景。
    pub bg: Color,
    /// 面板背景（比窗口略亮）。
    pub panel: Color,
    /// 面板内更高的区块（卡片）。
    pub card: Color,
    /// 分割线。
    pub divider: Color,
    /// 主文字。
    pub text: Color,
    /// 次要文字（说明、辅助信息）。
    pub text_dim: Color,
    /// 更弱的文字（占位符）。
    pub text_faint: Color,
    /// 强调色（主按钮、选中项）。
    pub accent: Color,
    /// 强调色的 hover。
    pub accent_hover: Color,
    /// 危险色（删除）。
    pub danger: Color,
    /// 危险色 hover。
    pub danger_hover: Color,
    /// 禁用态前景/背景。
    pub disabled: Color,
    /// 输入框背景。
    pub input_bg: Color,
    /// 输入框边框。
    pub input_border: Color,
    /// 输入框聚焦边框。
    pub input_border_focus: Color,
    /// 按钮背景（普通）。
    pub button: Color,
    /// 按钮 hover。
    pub button_hover: Color,
    /// 按钮按下。
    pub button_active: Color,
    /// 列表项选中背景。
    pub row_selected: Color,
    /// 列表项 hover 背景。
    pub row_hover: Color,

    // ---- 字号 ----
    /// 大标题。
    pub font_title: i32,
    /// 区块标题。
    pub font_heading: i32,
    /// 正文。
    pub font_body: i32,
    /// 小字（辅助）。
    pub font_small: i32,

    // ---- 间距/尺寸 ----
    /// 控件高度（按钮/输入框）。
    pub control_h: i32,
    /// 小控件高度（标签/次级按钮）。
    pub control_h_small: i32,
    /// 标准内边距。
    pub pad: i32,
    /// 小内边距。
    pub pad_small: i32,
    /// 区块间距。
    pub gap: i32,
    /// 圆角半径（后端可忽略，或用多矩形近似）。
    pub radius: i32,
    /// 侧栏宽度。
    pub sidebar_w: i32,
    /// 顶栏高度。
    pub topbar_h: i32,
    /// 边框厚度。
    pub border: i32,

    /// 外观配置（字体 / 磨玻璃 / 圆角 / 动画开关）。
    ///
    /// 主题负责"颜色与尺寸"，外观负责"资源与特效"。两者都由业务代码持有，
    /// 但 [`StyleConfig`] 会在每帧交给 [`crate::Backend::configure`]，
    /// 因为字体与窗口圆角属于平台资源。
    pub style: StyleConfig,
}

impl Theme {
    /// 暗色主题（启动器默认）。
    pub fn dark() -> Self {
        Self {
            bg: Color::rgb(0x18, 0x18, 0x1B),
            panel: Color::rgb(0x22, 0x22, 0x26),
            card: Color::rgb(0x2C, 0x2C, 0x31),
            divider: Color::rgb(0x3A, 0x3A, 0x40),
            text: Color::rgb(0xE8, 0xE8, 0xEC),
            text_dim: Color::rgb(0x9A, 0x9A, 0xA4),
            text_faint: Color::rgb(0x66, 0x66, 0x70),
            accent: Color::rgb(0x4C, 0x8E, 0xE0),
            accent_hover: Color::rgb(0x63, 0xA0, 0xEC),
            danger: Color::rgb(0xD9, 0x53, 0x4F),
            danger_hover: Color::rgb(0xE8, 0x6A, 0x66),
            disabled: Color::rgb(0x4A, 0x4A, 0x52),
            input_bg: Color::rgb(0x1C, 0x1C, 0x20),
            input_border: Color::rgb(0x3A, 0x3A, 0x42),
            input_border_focus: Color::rgb(0x4C, 0x8E, 0xE0),
            button: Color::rgb(0x33, 0x33, 0x3A),
            button_hover: Color::rgb(0x3E, 0x3E, 0x47),
            button_active: Color::rgb(0x2A, 0x2A, 0x30),
            row_selected: Color::rgb(0x2F, 0x45, 0x63),
            row_hover: Color::rgb(0x2A, 0x2A, 0x31),

            font_title: 22,
            font_heading: 17,
            font_body: 14,
            font_small: 12,

            control_h: 32,
            control_h_small: 24,
            pad: 12,
            pad_small: 6,
            gap: 10,
            radius: 4,
            style: StyleConfig::default(),
            sidebar_w: 190,
            topbar_h: 44,
            border: 1,
        }
    }

    /// 亮色主题。
    pub fn light() -> Self {
        Self {
            bg: Color::rgb(0xF4, 0xF4, 0xF6),
            panel: Color::rgb(0xFF, 0xFF, 0xFF),
            card: Color::rgb(0xF0, 0xF0, 0xF3),
            divider: Color::rgb(0xD8, 0xD8, 0xDE),
            text: Color::rgb(0x1C, 0x1C, 0x21),
            text_dim: Color::rgb(0x5A, 0x5A, 0x64),
            text_faint: Color::rgb(0x92, 0x92, 0x9C),
            accent: Color::rgb(0x2A, 0x6F, 0xC8),
            accent_hover: Color::rgb(0x3A, 0x80, 0xD8),
            danger: Color::rgb(0xC0, 0x3B, 0x36),
            danger_hover: Color::rgb(0xD4, 0x4D, 0x48),
            disabled: Color::rgb(0xA8, 0xA8, 0xB0),
            input_bg: Color::rgb(0xFF, 0xFF, 0xFF),
            input_border: Color::rgb(0xC8, 0xC8, 0xD0),
            input_border_focus: Color::rgb(0x2A, 0x6F, 0xC8),
            button: Color::rgb(0xE6, 0xE6, 0xEB),
            button_hover: Color::rgb(0xD8, 0xD8, 0xDF),
            button_active: Color::rgb(0xC8, 0xC8, 0xD2),
            row_selected: Color::rgb(0xCF, 0xE0, 0xF5),
            row_hover: Color::rgb(0xE9, 0xE9, 0xEF),

            font_title: 22,
            font_heading: 17,
            font_body: 14,
            font_small: 12,

            control_h: 32,
            control_h_small: 24,
            pad: 12,
            pad_small: 6,
            gap: 10,
            radius: 4,
            style: StyleConfig::default(),
            sidebar_w: 190,
            topbar_h: 44,
            border: 1,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_and_light_differ_meaningfully() {
        let d = Theme::dark();
        let l = Theme::light();
        assert_ne!(d.bg, l.bg, "背景必须不同");
        // 暗色背景亮度应显著低于亮色
        let lum = |c: Color| c.r() as i32 + c.g() as i32 + c.b() as i32;
        assert!(
            lum(d.bg) < lum(l.bg),
            "暗色背景应更暗：dark={} light={}",
            lum(d.bg),
            lum(l.bg)
        );
    }

    #[test]
    fn text_contrasts_with_background() {
        for t in [Theme::dark(), Theme::light()] {
            let bg = (t.bg.r() as i32 + t.bg.g() as i32 + t.bg.b() as i32) / 3;
            let fg = (t.text.r() as i32 + t.text.g() as i32 + t.text.b() as i32) / 3;
            assert!(
                (bg - fg).abs() > 100,
                "文字与背景亮度差应足够大（可读性）：bg={bg} fg={fg}"
            );
        }
    }

    #[test]
    fn font_sizes_are_ordered() {
        let t = Theme::dark();
        assert!(t.font_title > t.font_heading);
        assert!(t.font_heading > t.font_body);
        assert!(t.font_body > t.font_small);
    }

    #[test]
    fn control_sizes_are_positive() {
        let t = Theme::dark();
        assert!(t.control_h > t.control_h_small);
        assert!(t.sidebar_w > 0 && t.topbar_h > 0);
        assert!(t.pad > 0 && t.gap > 0);
    }
}
