//! 右键菜单（浮层 / overlay）。
//!
//! # 参考 PCL
//!
//! PCL 的 `MyListItem` 支持右键弹出菜单；菜单是 WPF 的 `ContextMenu`（独立弹出窗口）。
//! 本实现没有窗口系统，做**窗口内浮层**（overlay）：菜单画在同一窗口的最上层。
//!
//! # 浮层 vs 窗口
//!
//! | | 独立窗口（PCL） | 窗口内浮层（本实现） |
//! |---|---|---|
//! | 可超出窗口边界 | ✅ | ❌（会被裁剪） |
//! | 需要焦点管理 | 复杂 | 简单 |
//! | 跨平台 | 依赖平台 | 统一 |
//!
//! 启动器的右键菜单都在窗口内，所以浮层够用。
//!
//! # 交互契约
//!
//! | 操作 | 结果 |
//! |---|---|
//! | 在控件上右键 | 打开菜单（记录位置） |
//! | 点击菜单项 | 触发该项，关闭菜单 |
//! | 点击菜单外 | 关闭菜单 |
//! | 按 Escape | 关闭菜单 |
//! | 打开另一个菜单 | 换位置重开（而不是叠加） |

use crate::event::{Event, MouseButton};
use crate::layout::Rect;

/// 菜单项。
#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub label: String,
    /// 该项是否禁用。
    pub disabled: bool,
    /// 是否是分隔线。
    pub separator: bool,
    /// 菜单打开时是否默认勾选。
    pub checked: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            disabled: false,
            separator: false,
            checked: false,
        }
    }

    pub fn separator() -> Self {
        Self {
            label: String::new(),
            disabled: true,
            separator: true,
            checked: false,
        }
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    pub fn checked(mut self) -> Self {
        self.checked = true;
        self
    }
}

/// 菜单状态（跨帧持有）。
#[derive(Debug, Default)]
pub struct ContextMenu {
    /// 菜单项列表。
    items: Vec<MenuItem>,
    /// 菜单是否打开。
    open: bool,
    /// 菜单左上角位置（屏幕坐标）。
    origin: (i32, i32),
    /// 菜单宽度。
    width: i32,
    /// 本帧选中的菜单项下标（Some 表示被点击）。
    clicked: Option<usize>,
    /// 本帧是否请求关闭（点击外部或 Escape）。
    close_requested: bool,
    /// 本帧是否有右键按下（用于打开菜单）。
    right_down: Option<(i32, i32)>,
}

impl ContextMenu {
    pub fn new() -> Self {
        Self::default()
    }

    /// 每帧开始：清空本帧的瞬时状态，**保留**菜单是否打开。
    pub fn begin_frame(&mut self) {
        self.clicked = None;
        self.close_requested = false;
        self.right_down = None;
    }

    /// 处理事件：检测右键按下与 Escape。
    pub fn handle_events(&mut self, events: &[Event]) {
        for e in events {
            match e {
                Event::MouseDown {
                    x,
                    y,
                    button: MouseButton::Right,
                    ..
                } => {
                    self.right_down = Some((*x, *y));
                }
                Event::KeyDown {
                    key: crate::event::Key::Escape,
                    ..
                } if self.open => {
                    self.close_requested = true;
                }
                _ => {}
            }
        }
    }

    /// 打开菜单（记录位置）。已在打开时会**换位置重开**。
    pub fn open_at(&mut self, x: i32, y: i32, items: Vec<MenuItem>, width: i32) {
        self.items = items;
        self.origin = (x, y);
        self.width = width;
        self.open = true;
        self.close_requested = false;
    }

    /// 关闭菜单。
    pub fn close(&mut self) {
        self.open = false;
        self.items.clear();
        self.clicked = None;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn items(&self) -> &[MenuItem] {
        &self.items
    }

    /// 菜单左上角位置。
    pub fn origin(&self) -> (i32, i32) {
        self.origin
    }

    /// 本帧被点击的菜单项下标。
    pub fn clicked(&self) -> Option<usize> {
        self.clicked
    }

    /// 本帧是否请求关闭（点击外部 / Escape）。
    pub fn close_requested(&self) -> bool {
        self.close_requested
    }

    /// 本帧的右键按下位置（用于"在控件上右键打开菜单"）。
    pub fn right_down(&self) -> Option<(i32, i32)> {
        self.right_down
    }

    /// 菜单项的高度。
    pub fn item_height(&self) -> i32 {
        28
    }

    /// 菜单总高度。
    pub fn total_height(&self) -> i32 {
        self.items.len() as i32 * self.item_height()
    }

    /// 菜单的矩形（用于绘制与命中测试）。
    pub fn rect(&self) -> Rect {
        let h = self.total_height();
        Rect::new(self.origin.0, self.origin.1, self.width, h)
    }

    /// 第 `index` 项的矩形。
    pub fn item_rect(&self, index: usize) -> Rect {
        Rect::new(
            self.origin.0,
            self.origin.1 + index as i32 * self.item_height(),
            self.width,
            self.item_height(),
        )
    }

    /// 命中测试：返回被点击的菜单项下标。
    ///
    /// 若点击在菜单**外**，返回 `None` 并标记需要关闭。
    pub fn hit_test(&mut self, x: i32, y: i32) -> Option<usize> {
        if !self.open {
            return None;
        }
        if !self.rect().contains(x, y) {
            // 点击外部 → 关闭菜单
            self.close_requested = true;
            return None;
        }
        let idx = ((y - self.origin.1) / self.item_height()) as usize;
        if idx >= self.items.len() {
            return None;
        }
        if self.items[idx].separator || self.items[idx].disabled {
            // 分隔线/禁用项不可点击，但也不关闭菜单
            return None;
        }
        self.clicked = Some(idx);
        Some(idx)
    }

    /// 标记某项被点击（由 UI 层在检测到点击时调用）。
    pub fn set_clicked(&mut self, index: usize) {
        self.clicked = Some(index);
    }

    /// 请求关闭。
    pub fn request_close(&mut self) {
        self.close_requested = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> ContextMenu {
        let mut m = ContextMenu::new();
        m.open_at(
            100,
            50,
            vec![
                MenuItem::new("打开"),
                MenuItem::separator(),
                MenuItem::new("删除").disabled(),
                MenuItem::new("属性"),
            ],
            120,
        );
        m
    }

    #[test]
    fn open_at_sets_state() {
        let m = menu();
        assert!(m.is_open());
        assert_eq!(m.origin(), (100, 50));
        assert_eq!(m.items().len(), 4);
    }

    #[test]
    fn hit_test_finds_item() {
        let mut m = menu();
        // 第 0 项在 y=50..78
        assert_eq!(m.hit_test(120, 60), Some(0));
        // 第 3 项在 y=134..162
        assert_eq!(m.hit_test(120, 140), Some(3));
    }

    #[test]
    fn hit_test_outside_closes() {
        let mut m = menu();
        assert_eq!(m.hit_test(10, 10), None);
        assert!(m.close_requested(), "点击菜单外应请求关闭");
    }

    #[test]
    fn separator_is_not_clickable() {
        let mut m = menu();
        // 第 1 项是分隔线
        assert_eq!(m.hit_test(120, 90), None);
        assert!(!m.close_requested(), "点到分隔线不应关闭菜单");
    }

    #[test]
    fn disabled_item_is_not_clickable() {
        let mut m = menu();
        // 第 2 项是禁用的"删除"
        assert_eq!(m.hit_test(120, 110), None);
        assert!(!m.close_requested());
    }

    #[test]
    fn clicked_is_recorded() {
        let mut m = menu();
        m.hit_test(120, 60);
        assert_eq!(m.clicked(), Some(0));
    }

    #[test]
    fn escape_requests_close() {
        let mut m = menu();
        m.handle_events(&[Event::KeyDown {
            key: crate::event::Key::Escape,
            mods: crate::id::Modifiers::default(),
        }]);
        assert!(m.close_requested());
    }

    #[test]
    fn right_click_is_captured() {
        let mut m = ContextMenu::new();
        m.handle_events(&[Event::MouseDown {
            x: 10,
            y: 20,
            button: MouseButton::Right,
            mods: crate::id::Modifiers::default(),
        }]);
        assert_eq!(m.right_down(), Some((10, 20)));
    }

    #[test]
    fn reopening_moves_menu() {
        let mut m = menu();
        m.open_at(200, 300, vec![MenuItem::new("新")], 100);
        assert_eq!(m.origin(), (200, 300));
        assert_eq!(m.items().len(), 1, "重开应替换内容，不叠加");
    }

    #[test]
    fn begin_frame_clears_transient_state() {
        let mut m = menu();
        m.hit_test(120, 60);
        m.begin_frame();
        assert_eq!(m.clicked(), None, "本帧状态应清空");
        assert!(m.is_open(), "但菜单仍应保持打开");
    }
}
