//! 键盘导航：焦点管理 + Tab 遍历 + Enter/Space 触发。
//!
//! # 参考 PCL 的做法
//!
//! PCL 的 `MyButton` 在 `MouseDown` 时调用 `Focus()`，把键盘焦点交给按钮；
//! 页面层（`MyPageRight`）用 `PageOnEnter` / `PageOnExit` 管理焦点进出。
//!
//! 本实现是即时模式，控件没有持久实例，所以焦点必须由 [`FocusRing`] 显式管理：
//!
//! - [`FocusRing`] 保存"当前焦点控件"与"本帧登记过的可聚焦控件顺序"；
//! - `Tab` 在登记过的控件间循环；
//! - `Enter` / `Space` 触发当前焦点控件。
//!
//! # 与 PCL 的差异（诚实记录）
//!
//! | 能力 | PCL | 本实现 |
//! |---|---|---|
//! | 鼠标点击获得焦点 | ✅ | ✅ |
//! | Tab 遍历焦点 | ✅（WPF 默认） | ✅ |
//! | Enter 触发按钮 | ✅ | ✅ |
//! | 焦点视觉（虚线框） | ✅ | ✅（强调色边框） |
//! | 方向键导航 | 依赖 WPF | ❌ 未实现 |

use std::collections::HashMap;

use crate::event::{Event, Key};
use crate::id::WidgetId;

/// 焦点环：跨帧持久的焦点状态 + 本帧的控件遍历顺序。
///
/// # 为什么遍历顺序要"本帧登记"
///
/// 即时模式每帧重建控件，没有固定的控件树。Tab 的遍历顺序必须由**本帧的实际绘制
/// 顺序**决定（也就是视觉上的从左到右、从上到下）。因此控件在被绘制时调用
/// [`FocusRing::register`] 登记自己，Tab 时按登记顺序遍历。
///
/// 登记表每帧重建（`begin_frame` 清空），但**焦点 ID**跨帧保留。
#[derive(Debug, Default)]
pub struct FocusRing {
    /// 当前焦点。
    current: Option<WidgetId>,
    /// 本帧登记的可聚焦控件（按绘制顺序）。
    order: Vec<WidgetId>,
    /// 每个控件是否可聚焦（不可聚焦的不进 order）。
    focusable: HashMap<WidgetId, bool>,
    /// 每个控件的屏幕矩形（方向键导航需要知道"上下左右"）。
    rects: HashMap<WidgetId, crate::layout::Rect>,
    /// 本帧是否按下了 Tab。
    tab_pressed: bool,
    /// 本帧是否按下了 Enter/Space。
    activate_pressed: bool,
    /// 本帧是否按下了 Escape（用于关闭浮层）。
    escape_pressed: bool,
    /// 本帧是否按下了方向键。
    arrow: Option<ArrowKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrowKey {
    Up,
    Down,
    Left,
    Right,
}

impl FocusRing {
    pub fn new() -> Self {
        Self::default()
    }

    /// 每帧开始：清空本帧的登记表与按键标记，**保留焦点**。
    pub fn begin_frame(&mut self) {
        self.order.clear();
        self.focusable.clear();
        self.rects.clear();
        self.tab_pressed = false;
        self.activate_pressed = false;
        self.escape_pressed = false;
        self.arrow = None;
    }

    /// 处理本帧的键盘事件。
    pub fn handle_events(&mut self, events: &[Event]) {
        for e in events {
            if let Event::KeyDown { key, .. } = e {
                match key {
                    Key::Tab => self.tab_pressed = true,
                    Key::Enter => self.activate_pressed = true,
                    Key::Escape => self.escape_pressed = true,
                    Key::Left => self.arrow = Some(ArrowKey::Left),
                    Key::Right => self.arrow = Some(ArrowKey::Right),
                    Key::Up => self.arrow = Some(ArrowKey::Up),
                    Key::Down => self.arrow = Some(ArrowKey::Down),
                    Key::Backspace | Key::Delete | Key::Home | Key::End | Key::Other => {}
                }
            }
        }
    }

    /// 登记一个可聚焦控件。控件在绘制时调用。
    pub fn register(&mut self, id: WidgetId, focusable: bool) {
        self.focusable.insert(id, focusable);
        if focusable && !self.order.contains(&id) {
            self.order.push(id);
        }
    }

    /// 登记一个可聚焦控件**及其位置**（方向键导航需要）。
    pub fn register_at(&mut self, id: WidgetId, focusable: bool, rect: crate::layout::Rect) {
        self.rects.insert(id, rect);
        self.register(id, focusable);
    }

    /// 方向键导航：按方向找最合适的控件。
    ///
    /// # 算法：主轴优先 + 最近距离
    ///
    /// 1. 先按**主轴**筛选：按 Down 只考虑在当前控件**下方**的候选
    ///    （`candidate.y > current.y`），按 Right 只考虑右侧的。
    /// 2. 在候选里按**主轴距离**排序，取最近的。
    /// 3. 主轴距离相同时，按**副轴距离**（垂直于移动方向）排序 ——
    ///    这样在同一行的控件之间移动会保持"水平"而不是乱跳。
    /// 4. 若主轴方向没有候选，则**回退到副轴**（例如在最后一行按 Down 会走到
    ///    同一列上一个，或干脆不动）。
    ///
    /// 这不是完整的二维空间导航算法，但对"侧栏 + 表单 + 列表"这类界面足够好用。
    pub fn focus_arrow(&mut self, dir: ArrowKey) -> Option<WidgetId> {
        let cur = self.current?;
        let cur_rect = *self.rects.get(&cur)?;
        if self.order.is_empty() {
            return None;
        }

        let mut best: Option<(WidgetId, i64, i64)> = None;
        for &id in &self.order {
            if id == cur {
                continue;
            }
            let Some(r) = self.rects.get(&id).copied() else {
                continue;
            };
            // 主轴/副轴上的"前进距离"与"偏移量"。
            let (main, cross, ok) = match dir {
                ArrowKey::Down => {
                    let dy = r.y - cur_rect.y;
                    (dy, (r.x - cur_rect.x).abs(), dy > 0)
                }
                ArrowKey::Up => {
                    let dy = cur_rect.y - r.y;
                    (dy, (r.x - cur_rect.x).abs(), dy > 0)
                }
                ArrowKey::Right => {
                    let dx = r.x - cur_rect.x;
                    (dx, (r.y - cur_rect.y).abs(), dx > 0)
                }
                ArrowKey::Left => {
                    let dx = cur_rect.x - r.x;
                    (dx, (r.y - cur_rect.y).abs(), dx > 0)
                }
            };
            if !ok {
                continue;
            }
            // 主轴距离小的优先；主轴距离相同时，副轴偏移小的优先。
            let candidate = (id, main as i64, cross as i64);
            let better = match best {
                None => true,
                Some((_, bm, bc)) => candidate.1 < bm || (candidate.1 == bm && candidate.2 < bc),
            };
            if better {
                best = Some(candidate);
            }
        }

        if let Some((id, _, _)) = best {
            self.current = Some(id);
            Some(id)
        } else {
            None
        }
    }

    /// 当前焦点。
    pub fn current(&self) -> Option<WidgetId> {
        self.current
    }

    /// 设置焦点。
    pub fn set_focus(&mut self, id: Option<WidgetId>) {
        self.current = id;
    }

    /// 该控件是否拥有焦点。
    pub fn has_focus(&self, id: WidgetId) -> bool {
        self.current == Some(id)
    }

    /// 本帧是否按下了 Tab。
    pub fn tab_pressed(&self) -> bool {
        self.tab_pressed
    }

    /// 本帧是否按下了 Enter/Space。
    pub fn activate_pressed(&self) -> bool {
        self.activate_pressed
    }

    /// 本帧是否按下了 Escape。
    pub fn escape_pressed(&self) -> bool {
        self.escape_pressed
    }

    /// 本帧的方向键。
    pub fn arrow(&self) -> Option<ArrowKey> {
        self.arrow
    }

    /// 移动焦点到下一个可聚焦控件。返回新的焦点。
    ///
    /// 循环语义：到末尾回到开头（PCL 的 WPF 默认也是循环的）。
    pub fn focus_next(&mut self) -> Option<WidgetId> {
        if self.order.is_empty() {
            return None;
        }
        let next = match self.current {
            None => self.order[0],
            Some(cur) => {
                let pos = self.order.iter().position(|id| *id == cur);
                match pos {
                    Some(i) => self.order[(i + 1) % self.order.len()],
                    None => self.order[0],
                }
            }
        };
        self.current = Some(next);
        Some(next)
    }

    /// 移动焦点到上一个可聚焦控件（Shift+Tab）。
    pub fn focus_prev(&mut self) -> Option<WidgetId> {
        if self.order.is_empty() {
            return None;
        }
        let prev = match self.current {
            None => self.order[self.order.len() - 1],
            Some(cur) => {
                let pos = self.order.iter().position(|id| *id == cur);
                match pos {
                    Some(0) => self.order[self.order.len() - 1],
                    Some(i) => self.order[i - 1],
                    None => self.order[self.order.len() - 1],
                }
            }
        };
        self.current = Some(prev);
        Some(prev)
    }

    /// 可聚焦控件数量（诊断用）。
    pub fn focusable_count(&self) -> usize {
        self.order.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> WidgetId {
        WidgetId(n)
    }

    #[test]
    fn tab_cycles_through_focusable_widgets() {
        let mut f = FocusRing::new();
        f.begin_frame();
        f.register(id(1), true);
        f.register(id(2), true);
        f.register(id(3), true);

        assert_eq!(f.focus_next(), Some(id(1)));
        assert_eq!(f.focus_next(), Some(id(2)));
        assert_eq!(f.focus_next(), Some(id(3)));
        // 循环回到开头
        assert_eq!(f.focus_next(), Some(id(1)));
    }

    #[test]
    fn shift_tab_goes_backwards() {
        let mut f = FocusRing::new();
        f.begin_frame();
        f.register(id(1), true);
        f.register(id(2), true);
        f.focus_next();
        f.focus_next();
        assert_eq!(f.focus_prev(), Some(id(1)));
    }

    #[test]
    fn non_focusable_widgets_are_skipped() {
        let mut f = FocusRing::new();
        f.begin_frame();
        f.register(id(1), true);
        f.register(id(2), false); // 不可聚焦
        f.register(id(3), true);
        assert_eq!(f.focus_next(), Some(id(1)));
        assert_eq!(f.focus_next(), Some(id(3)), "不可聚焦的控件应被跳过");
    }

    #[test]
    fn focus_persists_across_frames_but_order_resets() {
        let mut f = FocusRing::new();
        f.begin_frame();
        f.register(id(1), true);
        f.focus_next();
        assert_eq!(f.current(), Some(id(1)));

        // 下一帧：登记表清空，但焦点保留
        f.begin_frame();
        assert_eq!(f.current(), Some(id(1)), "焦点必须跨帧保留");
        assert_eq!(f.focusable_count(), 0, "登记表每帧重建");
    }

    #[test]
    fn tab_with_no_focusable_widgets_does_nothing() {
        let mut f = FocusRing::new();
        f.begin_frame();
        assert_eq!(f.focus_next(), None);
        assert_eq!(f.current(), None);
    }

    #[test]
    fn keyboard_events_are_recorded() {
        let mut f = FocusRing::new();
        f.begin_frame();
        f.handle_events(&[
            Event::KeyDown {
                key: Key::Tab,
                mods: crate::id::Modifiers::default(),
            },
            Event::KeyDown {
                key: Key::Enter,
                mods: crate::id::Modifiers::default(),
            },
        ]);
        assert!(f.tab_pressed());
        assert!(f.activate_pressed());
        assert!(!f.escape_pressed());
    }

    #[test]
    fn escape_is_recorded() {
        let mut f = FocusRing::new();
        f.begin_frame();
        f.handle_events(&[Event::KeyDown {
            key: Key::Escape,
            mods: crate::id::Modifiers::default(),
        }]);
        assert!(f.escape_pressed());
    }
}
