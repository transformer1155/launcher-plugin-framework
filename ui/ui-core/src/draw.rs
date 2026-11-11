//! 绘制指令与绘制列表。
//!
//! # 后端只需要认识这几种指令
//!
//! [`DrawOp`] 是一组**很小**的指令：填充矩形（含圆角）、描边（含圆角）、
//! 文字、线段、裁剪栈，以及一条磨玻璃后处理指令。没有渐变、阴影、变换矩阵
//! —— 那些会让每个后端都要实现一套复杂光栅化器。
//!
//! 圆角值得单列一条指令：把圆角拆成多个矩形会在斜边出现台阶，而
//! Win32 的 `RoundRect` / 软件后端的逐像素距离判定都能一次画对。
//! 磨玻璃同理，它是"对已画内容做后处理"，无法用普通矩形表达。

use crate::color::Color;
use crate::layout::Rect;

/// 文字水平对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// 一条绘制指令。
#[derive(Debug, Clone, PartialEq)]
pub enum DrawOp {
    /// 填充矩形。
    FillRect { rect: Rect, color: Color },
    /// 填充**圆角**矩形。`radius` 是逻辑像素，后端夹到 `min(w,h)/2`。
    FillRoundRect {
        rect: Rect,
        color: Color,
        radius: i32,
    },
    /// 圆角描边（描边不能拆成 4 条矩形，否则四个角是方的）。
    StrokeRoundRect {
        rect: Rect,
        color: Color,
        radius: i32,
        thickness: i32,
    },
    /// 磨玻璃区域：后端对**已绘制的内容**做模糊，再叠一层 tint。
    ///
    /// 这是一条"后处理"指令，指令顺序仍然有意义：它只作用于此刻之前的像素。
    /// 软件后端做真实 box blur；Win32 后端用系统 backdrop + 半透明 tint 近似。
    BlurRegion {
        rect: Rect,
        radius: i32,
        tint: Color,
    },
    /// 描边矩形（`thickness` 向内）。用于分割线、输入框边框。
    StrokeRect {
        rect: Rect,
        color: Color,
        thickness: i32,
    },
    /// 文字。`rect` 定义**可用区域**，后端按 `align` 在区域内摆放。
    Text {
        rect: Rect,
        text: String,
        color: Color,
        size_px: i32,
        align: TextAlign,
        /// 是否加粗。后端可忽略（软件后端可用简单加粗模拟）。
        bold: bool,
    },
    /// 水平或垂直线段（`horizontal` 决定方向）。用于分隔线。
    Line {
        rect: Rect,
        color: Color,
        horizontal: bool,
    },
    /// 开始裁剪到 `rect`（之后所有指令都被裁到此区域）。
    PushClip { rect: Rect },
    /// 结束最近一次裁剪。
    PopClip,
}

/// 一帧的绘制指令集合。
///
/// # 不变量
///
/// - `PushClip` / `PopClip` 必须配对（[`DrawList::push_clip`] / [`DrawList::pop_clip`]
///   保证这一点）。
/// - 指令顺序即绘制顺序。
#[derive(Debug, Default, Clone)]
pub struct DrawList {
    ops: Vec<DrawOp>,
    /// 当前裁剪栈（用于在 push 时求交集）。
    clip_stack: Vec<Rect>,
}

impl DrawList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.ops.clear();
        self.clip_stack.clear();
    }

    pub fn ops(&self) -> &[DrawOp] {
        &self.ops
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// 当前有效裁剪区（无裁剪时为 `None`，表示整窗）。
    pub fn current_clip(&self) -> Option<Rect> {
        self.clip_stack.last().copied()
    }

    /// 把一个矩形与当前裁剪求交集。**所有 `add_*` 都经过它**，
    /// 因此后端收到的一切都在窗口内。
    fn clip_rect(&self, r: Rect) -> Option<Rect> {
        let r = match self.current_clip() {
            Some(c) => r.intersect(c),
            None => r,
        };
        if r.is_empty() {
            None
        } else {
            Some(r)
        }
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        if color.a() == 0 {
            return; // 完全透明，跳过（省后端工作）
        }
        if let Some(rect) = self.clip_rect(rect) {
            self.ops.push(DrawOp::FillRect { rect, color });
        }
    }

    /// 填充圆角矩形。`radius <= 0` 时退化成普通矩形，后端不必处理退化情形。
    pub fn fill_round_rect(&mut self, rect: Rect, color: Color, radius: i32) {
        if radius <= 0 {
            self.fill_rect(rect, color);
            return;
        }
        if color.a() == 0 {
            return;
        }
        if let Some(rect) = self.clip_rect(rect) {
            self.ops.push(DrawOp::FillRoundRect {
                rect,
                color,
                radius: clamp_radius(radius, rect),
            });
        }
    }

    /// 圆角描边。
    pub fn stroke_round_rect(&mut self, rect: Rect, color: Color, radius: i32, thickness: i32) {
        if radius <= 0 {
            self.stroke_rect(rect, color, thickness);
            return;
        }
        if thickness <= 0 || color.a() == 0 {
            return;
        }
        if let Some(rect) = self.clip_rect(rect) {
            let t = thickness.min(rect.w).min(rect.h);
            self.ops.push(DrawOp::StrokeRoundRect {
                rect,
                color,
                radius: clamp_radius(radius, rect),
                thickness: t,
            });
        }
    }

    /// 在指定区域上做磨玻璃后处理（模糊当前像素 + 叠加 tint）。
    pub fn blur_region(&mut self, rect: Rect, radius: i32, tint: Color) {
        if radius <= 0 && tint.a() == 0 {
            return;
        }
        if let Some(rect) = self.clip_rect(rect) {
            self.ops.push(DrawOp::BlurRegion {
                rect,
                radius: radius.clamp(0, 48),
                tint,
            });
        }
    }

    /// 描边：实现为四条细矩形拼成的边框（后端不必实现 `StrokeRect`）。
    ///
    /// 这样拆的好处：后端**只需要实现 `FillRect`** 就能支持边框，
    /// 减少了后端之间视觉不一致的可能。
    pub fn stroke_rect(&mut self, rect: Rect, color: Color, thickness: i32) {
        if thickness <= 0 || rect.is_empty() {
            return;
        }
        let t = thickness.min(rect.w).min(rect.h);
        // 上
        self.fill_rect(Rect::new(rect.x, rect.y, rect.w, t), color);
        // 下
        self.fill_rect(Rect::new(rect.x, rect.bottom() - t, rect.w, t), color);
        // 左（跳过已被上下覆盖的角，避免重复绘制时的半透明叠加）
        self.fill_rect(
            Rect::new(rect.x, rect.y + t, t, (rect.h - 2 * t).max(0)),
            color,
        );
        // 右
        self.fill_rect(
            Rect::new(rect.right() - t, rect.y + t, t, (rect.h - 2 * t).max(0)),
            color,
        );
    }

    pub fn text(
        &mut self,
        rect: Rect,
        text: impl Into<String>,
        color: Color,
        size_px: i32,
        align: TextAlign,
        bold: bool,
    ) {
        let text = text.into();
        if text.is_empty() || color.a() == 0 {
            return;
        }
        if let Some(rect) = self.clip_rect(rect) {
            self.ops.push(DrawOp::Text {
                rect,
                text,
                color,
                size_px: size_px.max(1),
                align,
                bold,
            });
        }
    }

    pub fn line(&mut self, rect: Rect, color: Color, horizontal: bool) {
        if let Some(rect) = self.clip_rect(rect) {
            self.ops.push(DrawOp::Line {
                rect,
                color,
                horizontal,
            });
        }
    }

    /// 压入裁剪区。与 [`Self::pop_clip`] 必须配对。
    pub fn push_clip(&mut self, rect: Rect) {
        let effective = match self.current_clip() {
            Some(c) => rect.intersect(c),
            None => rect,
        };
        self.clip_stack.push(effective);
        self.ops.push(DrawOp::PushClip { rect: effective });
    }

    pub fn pop_clip(&mut self) {
        if self.clip_stack.pop().is_some() {
            self.ops.push(DrawOp::PopClip);
        }
    }

    /// 判断两帧的绘制内容是否**完全相同**。
    ///
    /// # 用途：避免无意义的重绘
    ///
    /// 即时模式 UI 每帧重建控件描述。鼠标在窗口里移动时会产生大量 `MouseMove`
    /// 事件，若每帧都全窗口重绘，CPU 占用会明显偏高（实测约 19%）。
    ///
    /// 但**大多数 mousemove 并不改变画面** —— 鼠标没碰到任何可交互控件时，
    /// hover 状态不变，绘制指令逐字节相同。此时跳过重绘即可。
    ///
    /// 代价是比较一次绘制列表（几十 KB 的逐字段比较），远小于一次全窗口 GDI 重绘。
    ///
    /// # 注意
    ///
    /// 这只适用于**纯函数式**的 UI 渲染：如果某帧的绘制依赖外部可变状态
    /// （比如时间、动画），即使指令相同画面也可能不同。这种情况下应当
    /// 由业务侧自行决定（例如加入一个"脏标记"）。
    pub fn same_content(&self, other: &DrawList) -> bool {
        self.ops.len() == other.ops.len() && self.ops == other.ops
    }

    /// 拍照式调试：把指令列表转成简短文本描述（用于测试断言）。
    pub fn debug_summary(&self) -> String {
        let mut s = String::new();
        for op in &self.ops {
            s.push_str(&format!("{op}\n"));
        }
        s
    }
}

/// 圆角半径不能超过短边的一半，否则两个角的圆弧会互相吃掉中间区域。
fn clamp_radius(radius: i32, rect: Rect) -> i32 {
    radius.max(0).min(rect.w.min(rect.h) / 2)
}

impl std::fmt::Display for DrawOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DrawOp::FillRect { rect, color } => {
                write!(
                    f,
                    "FillRect({},{},{},{} #{:08X})",
                    rect.x, rect.y, rect.w, rect.h, color.0
                )
            }
            DrawOp::FillRoundRect { rect, color, radius } => {
                write!(
                    f,
                    "FillRoundRect({},{},{},{} r={} #{:08X})",
                    rect.x, rect.y, rect.w, rect.h, radius, color.0
                )
            }
            DrawOp::StrokeRoundRect {
                rect,
                color,
                radius,
                thickness,
            } => write!(
                f,
                "StrokeRoundRect({},{},{},{} r={} t={} #{:08X})",
                rect.x, rect.y, rect.w, rect.h, radius, thickness, color.0
            ),
            DrawOp::BlurRegion { rect, radius, tint } => write!(
                f,
                "BlurRegion({},{},{},{} r={} #{:08X})",
                rect.x, rect.y, rect.w, rect.h, radius, tint.0
            ),
            DrawOp::StrokeRect {
                rect,
                color,
                thickness,
            } => write!(
                f,
                "StrokeRect({},{},{},{} t={} #{:08X})",
                rect.x, rect.y, rect.w, rect.h, thickness, color.0
            ),
            DrawOp::Text {
                rect,
                text,
                color,
                size_px,
                align,
                bold,
            } => write!(
                f,
                "Text({},{},{},{} \"{}\" {:?} {}px{} #{:08X})",
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                text,
                align,
                size_px,
                if *bold { " bold" } else { "" },
                color.0
            ),
            DrawOp::Line {
                rect,
                color,
                horizontal,
            } => write!(
                f,
                "Line({},{},{},{} {:?} #{:08X})",
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                if *horizontal { "H" } else { "V" },
                color.0
            ),
            DrawOp::PushClip { rect } => {
                write!(f, "PushClip({},{},{},{})", rect.x, rect.y, rect.w, rect.h)
            }
            DrawOp::PopClip => f.write_str("PopClip"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_fill_is_skipped() {
        let mut d = DrawList::new();
        d.fill_rect(Rect::new(0, 0, 10, 10), Color::TRANSPARENT);
        assert!(d.is_empty(), "完全透明的填充应当被跳过");
    }

    #[test]
    fn empty_text_is_skipped() {
        let mut d = DrawList::new();
        d.text(
            Rect::new(0, 0, 100, 20),
            "",
            Color::rgb(255, 255, 255),
            14,
            TextAlign::Left,
            false,
        );
        assert!(d.is_empty());
    }

    #[test]
    fn clip_trims_and_filters() {
        let mut d = DrawList::new();
        d.push_clip(Rect::new(0, 0, 100, 100));
        d.fill_rect(Rect::new(50, 50, 100, 100), Color::rgb(1, 2, 3));
        // 完全在外面的应当被过滤掉
        d.fill_rect(Rect::new(200, 200, 10, 10), Color::rgb(1, 2, 3));
        d.pop_clip();

        let fills: Vec<&DrawOp> = d
            .ops()
            .iter()
            .filter(|o| matches!(o, DrawOp::FillRect { .. }))
            .collect();
        assert_eq!(fills.len(), 1, "完全在裁剪区外的应被丢弃");
        if let DrawOp::FillRect { rect, .. } = fills[0] {
            assert_eq!(*rect, Rect::new(50, 50, 50, 50), "应被裁剪到 50x50");
        }
    }

    #[test]
    fn nested_clips_intersect() {
        let mut d = DrawList::new();
        d.push_clip(Rect::new(0, 0, 100, 100));
        d.push_clip(Rect::new(50, 50, 100, 100));
        assert_eq!(d.current_clip(), Some(Rect::new(50, 50, 50, 50)));
        d.pop_clip();
        assert_eq!(d.current_clip(), Some(Rect::new(0, 0, 100, 100)));
        d.pop_clip();
        assert_eq!(d.current_clip(), None);
    }

    #[test]
    fn pop_without_push_is_noop() {
        let mut d = DrawList::new();
        d.pop_clip();
        assert!(d.is_empty(), "无配对的 pop 不应产生指令");
    }

    #[test]
    fn stroke_expands_to_four_fills() {
        let mut d = DrawList::new();
        d.stroke_rect(Rect::new(0, 0, 100, 50), Color::rgb(255, 255, 255), 1);
        assert_eq!(d.len(), 4, "边框应展开为 4 条填充");
    }

    #[test]
    fn stroke_zero_thickness_is_noop() {
        let mut d = DrawList::new();
        d.stroke_rect(Rect::new(0, 0, 100, 50), Color::rgb(255, 255, 255), 0);
        assert!(d.is_empty());
    }

    #[test]
    fn stroke_thickness_clamped_to_size() {
        let mut d = DrawList::new();
        d.stroke_rect(Rect::new(0, 0, 4, 4), Color::rgb(255, 255, 255), 10);
        // 不 panic，且左右条高度会是 0（被 clamp 后跳过）
        assert!(d.len() <= 4);
    }

    #[test]
    fn clip_stack_always_balanced() {
        let mut d = DrawList::new();
        d.push_clip(Rect::new(0, 0, 10, 10));
        d.push_clip(Rect::new(0, 0, 10, 10));
        d.pop_clip();
        d.pop_clip();
        // 统计 Push/Pop 数量应相等
        let pushes = d
            .ops()
            .iter()
            .filter(|o| matches!(o, DrawOp::PushClip { .. }))
            .count();
        let pops = d
            .ops()
            .iter()
            .filter(|o| matches!(o, DrawOp::PopClip))
            .count();
        assert_eq!(pushes, pops);
    }

    #[test]
    fn text_preserves_utf8() {
        let mut d = DrawList::new();
        d.text(
            Rect::new(0, 0, 200, 20),
            "中文 Hello",
            Color::rgb(255, 255, 255),
            14,
            TextAlign::Center,
            true,
        );
        match &d.ops()[0] {
            DrawOp::Text {
                text, align, bold, ..
            } => {
                assert_eq!(text, "中文 Hello");
                assert_eq!(*align, TextAlign::Center);
                assert!(*bold);
            }
            _ => panic!("应为 Text"),
        }
    }

    #[test]
    fn clear_resets_stack() {
        let mut d = DrawList::new();
        d.push_clip(Rect::new(0, 0, 10, 10));
        d.clear();
        assert!(d.is_empty());
        assert_eq!(d.current_clip(), None);
    }

    #[test]
    fn round_rect_degrades_to_plain_when_radius_zero() {
        let mut d = DrawList::new();
        d.fill_round_rect(Rect::new(0, 0, 40, 20), Color::rgb(1, 2, 3), 0);
        assert!(matches!(d.ops()[0], DrawOp::FillRect { .. }));
    }

    #[test]
    fn round_radius_clamped_to_half_short_edge() {
        let mut d = DrawList::new();
        d.fill_round_rect(Rect::new(0, 0, 40, 20), Color::rgb(1, 2, 3), 999);
        match d.ops()[0] {
            DrawOp::FillRoundRect { radius, .. } => assert_eq!(radius, 10),
            _ => panic!("应为圆角填充"),
        }
    }

    #[test]
    fn blur_region_is_clipped_and_emitted() {
        let mut d = DrawList::new();
        d.push_clip(Rect::new(0, 0, 50, 50));
        d.blur_region(Rect::new(10, 10, 100, 100), 12, Color::argb(0x30, 0, 0, 0));
        d.pop_clip();
        match &d.ops()[1] {
            DrawOp::BlurRegion { rect, radius, .. } => {
                assert_eq!(*rect, Rect::new(10, 10, 40, 40));
                assert_eq!(*radius, 12);
            }
            _ => panic!("应为磨玻璃指令"),
        }
    }

    #[test]
    fn round_ops_affect_same_content() {
        let mut a = DrawList::new();
        let mut b = DrawList::new();
        a.fill_round_rect(Rect::new(0, 0, 40, 20), Color::rgb(1, 2, 3), 8);
        b.fill_round_rect(Rect::new(0, 0, 40, 20), Color::rgb(1, 2, 3), 4);
        assert!(!a.same_content(&b), "半径变化必须触发重绘");
    }
}
