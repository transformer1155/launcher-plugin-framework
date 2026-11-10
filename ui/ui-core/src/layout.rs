//! 几何类型：`Rect` / `Size` / 边距 / 对齐。
//!
//! 全部用 `i32` 而不是 `f32`：像素是整数，用 f32 会在边界出现"差一像素"的
//! 缝隙/重叠（UI 里最显眼的瑕疵）。布局阶段做除法时**总是向下取整**，
//! 让容器与子元素不会因为舍入而互相重叠。

/// 尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Size {
    pub w: i32,
    pub h: i32,
}

impl Size {
    pub const fn new(w: i32, h: i32) -> Self {
        Self { w, h }
    }
}

/// 矩形（左上角 + 尺寸）。故意不做 `right/bottom` 字段，避免四元组不一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const ZERO: Rect = Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(&self) -> i32 {
        self.x + self.w
    }
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// 向内收缩。
    pub fn shrink(&self, e: EdgeInsets) -> Rect {
        Rect::new(
            self.x + e.left,
            self.y + e.top,
            (self.w - e.left - e.right).max(0),
            (self.h - e.top - e.bottom).max(0),
        )
    }

    /// 求交集。无重叠时返回空矩形（`w`/`h` 为 0），**不是** `None` ——
    /// 让调用方少写一层解构，裁剪逻辑更好写。
    pub fn intersect(&self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        Rect::new(x, y, (r - x).max(0), (b - y).max(0))
    }

    /// 该点是否在矩形内（`right`/`bottom` 为开区间，避免相邻控件共享边时双双命中）。
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    /// 在容器里按对齐方式放置一个 `size` 大小的盒子。
    pub fn align_in(&self, size: Size, ax: Align, ay: Align) -> Rect {
        let x = match ax {
            Align::Start => self.x,
            Align::Center => self.x + (self.w - size.w) / 2,
            Align::End => self.right() - size.w,
        };
        let y = match ay {
            Align::Start => self.y,
            Align::Center => self.y + (self.h - size.h) / 2,
            Align::End => self.bottom() - size.h,
        };
        Rect::new(x, y, size.w, size.h)
    }

    /// 按比例切出一块（用于分割面板）。剩余部分由调用方自己算。
    pub fn take_left(&self, w: i32) -> Rect {
        Rect::new(self.x, self.y, w.min(self.w), self.h)
    }

    pub fn take_right(&self, w: i32) -> Rect {
        let w = w.min(self.w);
        Rect::new(self.right() - w, self.y, w, self.h)
    }

    pub fn take_top(&self, h: i32) -> Rect {
        Rect::new(self.x, self.y, self.w, h.min(self.h))
    }

    pub fn take_bottom(&self, h: i32) -> Rect {
        let h = h.min(self.h);
        Rect::new(self.x, self.bottom() - h, self.w, h)
    }
}

/// 对齐方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

/// 四边距。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EdgeInsets {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl EdgeInsets {
    pub const ZERO: EdgeInsets = EdgeInsets {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    pub const fn all(v: i32) -> Self {
        Self {
            left: v,
            top: v,
            right: v,
            bottom: v,
        }
    }

    pub const fn symmetric(h: i32, v: i32) -> Self {
        Self {
            left: h,
            top: v,
            right: h,
            bottom: v,
        }
    }

    pub const fn horizontal(&self) -> i32 {
        self.left + self.right
    }

    pub const fn vertical(&self) -> i32 {
        self.top + self.bottom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(0, 0, 10, 10);
        assert!(r.contains(0, 0));
        assert!(r.contains(9, 9));
        assert!(!r.contains(10, 0), "右边为开区间");
        assert!(!r.contains(0, 10), "下边为开区间");
        assert!(!r.contains(-1, 0));
    }

    #[test]
    fn intersect_no_overlap_is_empty() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 20, 10, 10);
        let i = a.intersect(b);
        assert!(i.is_empty());
        assert_eq!(i.w, 0);
    }

    #[test]
    fn intersect_partial() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        let i = a.intersect(b);
        assert_eq!(i, Rect::new(5, 5, 5, 5));
    }

    #[test]
    fn shrink_never_negative() {
        let r = Rect::new(0, 0, 10, 10);
        let s = r.shrink(EdgeInsets::all(20));
        assert_eq!(s.w, 0, "过度收缩应为 0 而不是负数");
        assert_eq!(s.h, 0);
    }

    #[test]
    fn take_splits_do_not_overlap() {
        let r = Rect::new(0, 0, 100, 50);
        let left = r.take_left(30);
        let right = r.take_right(20);
        assert_eq!(left, Rect::new(0, 0, 30, 50));
        assert_eq!(right, Rect::new(80, 0, 20, 50));
        assert!(left.intersect(right).is_empty(), "左右切块不应重叠");
    }

    #[test]
    fn align_center_even_and_odd() {
        let container = Rect::new(0, 0, 100, 100);
        let r = container.align_in(Size::new(50, 50), Align::Center, Align::Center);
        assert_eq!(r, Rect::new(25, 25, 50, 50));
        // 奇数差时向下取整，不会偏出右/下边
        let r2 = container.align_in(Size::new(51, 51), Align::Center, Align::Center);
        assert_eq!(r2.x, 24);
        assert!(r2.right() <= container.right());
    }

    #[test]
    fn align_end_sticks_to_edge() {
        let c = Rect::new(0, 0, 100, 100);
        let r = c.align_in(Size::new(10, 10), Align::End, Align::End);
        assert_eq!((r.right(), r.bottom()), (100, 100));
    }
}
