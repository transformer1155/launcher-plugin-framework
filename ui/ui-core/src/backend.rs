//! 后端抽象：平台相关的部分只允许通过这里暴露。
//!
//! # 后端作者需要实现什么
//!
//! 只有四个方法：
//!
//! | 方法 | 职责 |
//! |---|---|
//! | [`Backend::size`] | 当前客户区尺寸（逻辑像素） |
//! | [`Backend::dpi_scale`] | 缩放因子（1.0 = 100%） |
//! | [`Backend::present`] | 把 [`DrawList`] 画到屏幕上 |
//! | [`Backend::text_width`] | 测量文字宽度（布局需要） |
//!
//! 其余（事件循环、窗口创建、DPI 变化）由各后端自己的平台代码处理，
//! 但它们**必须**把事件翻译成 [`crate::Event`] 并驱动 [`crate::ui::Ui`]。
//!
//! # 为什么 `text_width` 在后端而不是 UI 内核
//!
//! 文字宽度依赖**字体**，而字体是平台资源（GDI 的 `GetTextExtentPoint32`、
//! 软件后端的自绘字形表）。UI 内核不认识字体，所以只能问后端。
//!
//! 代价：布局每帧要调用它若干次。因此后端**必须缓存字体句柄并按 (文本, 字号)
//! 缓存测量结果** —— 否则每帧上千次 GDI 调用的性能是不可接受的。

use crate::draw::DrawList;
use crate::layout::Size;
use crate::style::StyleConfig;

/// 平台后端。
pub trait Backend {
    /// 客户区尺寸（**逻辑像素**，已除去 DPI 缩放）。
    fn size(&self) -> Size;

    /// DPI 缩放因子。`1.0` 表示 96 DPI。
    ///
    /// 后端负责：物理像素 = 逻辑像素 × scale。UI 内核与业务代码永远只用逻辑像素。
    fn dpi_scale(&self) -> f32;

    /// 把一帧画到屏幕。
    ///
    /// 实现方应按 `DrawList` 顺序绘制；`PushClip`/`PopClip` 用平台裁剪区实现。
    fn present(&mut self, list: &DrawList);

    /// 测量文本宽度（逻辑像素）。用于右对齐、居中、截断。
    fn text_width(&self, text: &str, size_px: i32, bold: bool) -> i32;

    /// 单行文字的高度（逻辑像素）。默认按常用比例估算，后端可覆盖为精确值。
    fn line_height(&self, size_px: i32, _bold: bool) -> i32 {
        // 经验值：多数无衬线字体的行高约为字号的 1.3 倍。
        (size_px as f32 * 1.3).round() as i32
    }

    /// 应用外观配置（字体 / 磨玻璃 / 窗口圆角）。
    ///
    /// # 为什么是 `&self`
    ///
    /// `Ui` 每帧只持有 `&Backend`（`text_width` 也是 `&self`），而运行时改外观
    /// 必须能在**渲染帧内**发生 —— 否则用户在设置面板里换字体就得重启程序。
    /// 后端因此用内部可变性（`RefCell` / 进程内共享状态）自己处理。
    ///
    /// 实现必须是**幂等**的：每帧都会调用，配置没变时不得重建资源。
    /// 默认空实现让纯测试用的假后端不必关心外观。
    fn configure(&self, _style: &StyleConfig) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeBackend;
    impl Backend for FakeBackend {
        fn size(&self) -> Size {
            Size::new(800, 600)
        }
        fn dpi_scale(&self) -> f32 {
            1.0
        }
        fn present(&mut self, _list: &DrawList) {}
        fn text_width(&self, text: &str, size_px: i32, _bold: bool) -> i32 {
            // 粗估：每个字符 0.6 倍字号
            (text.chars().count() as f32 * size_px as f32 * 0.6) as i32
        }
    }

    #[test]
    fn default_line_height_is_reasonable() {
        let b = FakeBackend;
        assert_eq!(b.line_height(14, false), 18);
        assert_eq!(b.line_height(20, true), 26);
    }

    #[test]
    fn backend_trait_is_object_safe() {
        // 允许 Box<dyn Backend>：这是所有使用点的基础。
        let b: Box<dyn Backend> = Box::new(FakeBackend);
        assert_eq!(b.size(), Size::new(800, 600));
    }
}
