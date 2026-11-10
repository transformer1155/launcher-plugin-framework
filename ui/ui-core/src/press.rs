//! 跨帧持久化的**按下态**跟踪。
//!
//! # 为什么需要它
//!
//! 即时模式 UI 每帧重建控件描述，`Ui` 本身是短命的。但"鼠标按下了按钮"这件事
//! **跨越多帧**：按下发生在第 N 帧，抬起可能在第 N+3 帧（用户按住了不动）。
//! 如果按下态存在 `Ui` 里，每帧都会丢失，导致"按住不放"永远触发不了点击。
//!
//! PCL 的做法是每个控件自己存一个 `IsMouseDown` 字段（保留模式控件有持久实例）。
//! 即时模式没有持久实例，所以需要一个**由调用方持有**的旁路存储 —— 就是本模块。
//!
//! # 语义（与 PCL `MyButton` 一致）
//!
//! | 事件 | 行为 |
//! |---|---|
//! | `MouseDown` 落在控件内 | 进入 `Pressed` |
//! | `MouseUp` 落在**同一控件**内 | 触发点击 → 退出 `Pressed` |
//! | `MouseUp` 落在别处 | **不触发点击** → 退出 `Pressed` |
//! | 鼠标移出控件 | 退出 `Pressed`（取消按下视觉） |
//!
//! 第 3、4 行是关键：**按下后拖出按钮再松开，不算点击**。

use std::collections::HashMap;

use crate::id::WidgetId;

/// 按下态跟踪器。由调用方跨帧持有（与 `scroll` 一样）。
#[derive(Debug, Default)]
pub struct PressTracker {
    /// 当前处于"已按下"的控件。同一时刻最多一个（鼠标只有一个左键）。
    pressed: Option<WidgetId>,
    /// 按下时鼠标的位置。用于判断"抬起是否还在同一控件内"之外的场景
    /// （例如拖动滑块时的阈值判定）。
    origin: Option<(i32, i32)>,
    /// 历史上按过的控件（用于调试与"点击后立即再按"的边界）。
    history: HashMap<WidgetId, u32>,
}

impl PressTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次按下。
    pub fn begin(&mut self, id: WidgetId) {
        self.pressed = Some(id);
        self.origin = None;
    }

    /// 记录一次按下（带起点坐标）。
    pub fn begin_at(&mut self, id: WidgetId, x: i32, y: i32) {
        self.pressed = Some(id);
        self.origin = Some((x, y));
    }

    /// 结束按下。返回 `true` 表示**是这个控件**结束按下（调用方据此判断是否点击）。
    ///
    /// 注意：返回 `true` 只说明"是这个控件按下的"，**不代表**点击成功 ——
    /// 还要看抬起位置是否仍在控件内（由调用方判断）。
    pub fn end(&mut self, id: WidgetId) -> bool {
        let was = self.pressed == Some(id);
        if was {
            self.pressed = None;
            self.origin = None;
            *self.history.entry(id).or_insert(0) += 1;
        }
        was
    }

    /// 取消某控件的按下态（鼠标移出时调用）。
    pub fn cancel(&mut self, id: WidgetId) {
        if self.pressed == Some(id) {
            self.pressed = None;
            self.origin = None;
        }
    }

    /// 清空所有按下态（窗口失去焦点 / `MouseLeave` 时调用）。
    ///
    /// 这一条很重要：按住鼠标拖到窗口外松开，松开事件不会到达窗口，
    /// 按钮会**永远停在按下态**。清除它才能恢复正常。
    pub fn clear_all(&mut self) {
        self.pressed = None;
        self.origin = None;
    }

    /// 该控件当前是否处于按下态。
    pub fn is_pressed(&self, id: WidgetId) -> bool {
        self.pressed == Some(id)
    }

    /// 当前被按下的控件。
    pub fn pressed_widget(&self) -> Option<WidgetId> {
        self.pressed
    }

    /// 按下时的鼠标位置。
    pub fn press_origin(&self) -> Option<(i32, i32)> {
        self.origin
    }

    /// 该控件累计被点击的次数（可用于诊断"按钮点了没反应"）。
    pub fn click_count(&self, id: WidgetId) -> u32 {
        self.history.get(&id).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> WidgetId {
        WidgetId(n)
    }

    #[test]
    fn down_then_up_same_widget_is_click() {
        let mut t = PressTracker::new();
        t.begin(id(1));
        assert!(t.is_pressed(id(1)));
        assert!(t.end(id(1)), "应由同一个控件结束按下");
        assert!(!t.is_pressed(id(1)));
    }

    #[test]
    fn up_on_other_widget_is_not_click() {
        // 关键用例：在 A 上按下，在 B 上抬起 → A 不算被点击。
        let mut t = PressTracker::new();
        t.begin(id(1));
        assert!(!t.end(id(2)), "B 并未按下过");
    }

    #[test]
    fn cancel_on_mouse_leave() {
        // 按下后拖出 → 取消按下态。
        let mut t = PressTracker::new();
        t.begin(id(1));
        t.cancel(id(1));
        assert!(!t.is_pressed(id(1)), "移出后不应仍是按下态");
        // 之后抬起也不该算点击
        assert!(!t.end(id(1)));
    }

    #[test]
    fn clear_all_resets() {
        let mut t = PressTracker::new();
        t.begin(id(1));
        t.clear_all();
        assert!(!t.is_pressed(id(1)));
        assert_eq!(t.pressed_widget(), None);
    }

    #[test]
    fn only_one_widget_pressed_at_a_time() {
        let mut t = PressTracker::new();
        t.begin(id(1));
        t.begin(id(2));
        assert!(!t.is_pressed(id(1)), "后按下的取代先按下的");
        assert!(t.is_pressed(id(2)));
    }

    #[test]
    fn click_count_accumulates() {
        let mut t = PressTracker::new();
        for _ in 0..3 {
            t.begin(id(1));
            t.end(id(1));
        }
        assert_eq!(t.click_count(id(1)), 3);
        assert_eq!(t.click_count(id(2)), 0);
    }

    #[test]
    fn press_origin_recorded() {
        let mut t = PressTracker::new();
        t.begin_at(id(1), 100, 200);
        assert_eq!(t.press_origin(), Some((100, 200)));
        t.end(id(1));
        assert_eq!(t.press_origin(), None);
    }
}
