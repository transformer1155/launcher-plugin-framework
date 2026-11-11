//! 输入事件与控件标识。
//!
//! # 为什么事件是"值"而不是"回调"
//!
//! 即时模式 UI 的标准做法：事件先在 [`crate::ui::Ui`] 里分配（决定哪个控件
//! "拥有"这次点击），再由业务代码通过 [`crate::ui::Response`] 查询。
//! 这样业务代码写成线性的 `if resp.clicked() { ... }`，不需要注册回调、
//! 不需要管理生命周期。

/// 鼠标按键。启动器只需要左键（右键可以扩展）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// 键盘按键。只列出启动器输入框/快捷键需要的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Backspace,
    Delete,
    Enter,
    Tab,
    Escape,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    /// 可打印字符由 [`Event::Text`] 单独传递（避免键盘布局差异）。
    Other,
}

/// 修饰键状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Ctrl（Windows）/ Cmd（macOS）。
    pub ctrl: bool,
    /// Shift。
    pub shift: bool,
    /// Alt / Option。
    pub alt: bool,
}

/// 输入事件。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// 鼠标移动（坐标是窗口像素）。
    MouseMove { x: i32, y: i32 },
    MouseDown {
        x: i32,
        y: i32,
        button: MouseButton,
        /// 按下时的修饰键。
        mods: Modifiers,
    },
    MouseUp {
        x: i32,
        y: i32,
        button: MouseButton,
        /// 抬起时的修饰键。
        mods: Modifiers,
    },
    /// 滚轮。`delta` 正为向上/向前。
    Wheel {
        x: i32,
        y: i32,
        delta: i32,
        /// 滚轮时的修饰键。
        mods: Modifiers,
    },
    /// 鼠标离开窗口（用于清除 hover 状态）。
    MouseLeave,
    KeyDown {
        key: Key,
        /// 按键时的修饰键。
        mods: Modifiers,
    },
    /// 文本输入（已处理过输入法/布局，直接是可插入的字符）。
    Text { text: String },
    /// 窗口尺寸变化。
    Resize { w: i32, h: i32 },
    /// 获得/失去焦点（用于关闭下拉框等）。
    Focus { focused: bool },
    /// 动画心跳（由后端定时器产生，不是用户输入）。
    ///
    /// 即时模式 UI 的动画需要"没有输入事件时也推进时间"。事件驱动的消息循环
    /// 靠这条事件把动画帧拉起来；它不是鼠标/键盘事件，不参与 hover / 点击判定。
    Tick,
    /// 全局快捷键（由后端把虚拟键码 + 修饰键归一化后发出）。
    Shortcut(ShortcutKey),
    /// 文件被拖放到窗口上（Win32 `WM_DROPFILES`；用于拖拽安装）。
    FilesDropped { paths: Vec<String> },
    /// 托盘图标被点击，请求恢复窗口。
    TrayRestore,

    /// 后端拦截了"最小化到托盘"（例如托盘图标不可用时），转发给 UI 决定兜底行为。
    TrayMinimized,
}

/// 业务层可直接消费的全局快捷键。
///
/// 后端负责把「虚拟键码 + Ctrl 状态」归一化成这里的值，UI 层不需要认识
/// `WM_KEYDOWN` / `VK_F5` 之类的平台细节。带 Ctrl 的组合用字符表示，
/// 例如 `Ctrl('1')` = Ctrl+1、`Ctrl('L')` = Ctrl+L。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutKey {
    F5,
    Escape,
    Ctrl(char),
}

/// 业务层向平台后端请求的**窗口级动作**。
///
/// 无边框窗口没有系统标题栏，最小化 / 最大化 / 关闭 / 拖动都只能由 UI 自己发起。
/// `ui-core` 只记录意图（放进 [`crate::ui::FrameState`] 的共享槽），具体执行
/// 由主循环交给后端 —— 内核不依赖任何平台 API，软件后端可以直接忽略它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    /// 按住标题栏空白处，开始系统级窗口拖动。
    Drag,
    Minimize,
    /// 隐藏窗口并驻留系统托盘。
    MinimizeToTray,
    /// 从托盘恢复窗口。
    Restore,
    /// 在最大化与还原之间切换。
    ToggleMaximize,
    Close,
}

impl Event {
    /// 事件发生时的鼠标坐标（非鼠标事件返回 `None`）。
    pub fn pos(&self) -> Option<(i32, i32)> {
        match self {
            Event::MouseMove { x, y }
            | Event::MouseDown { x, y, .. }
            | Event::MouseUp { x, y, .. }
            | Event::Wheel { x, y, .. } => Some((*x, *y)),
            _ => None,
        }
    }
}

/// 控件标识。
///
/// # 为什么不是全局自增 ID
///
/// 即时模式 UI 每帧重建控件，全局自增 ID 每帧都会变，导致"上一帧 hover 的按钮
/// 在下一帧变成别的按钮"。因此 ID 由**调用栈位置 + 显式标签**决定：同一段代码
/// 在同一位置生成同一个 ID。
///
/// 实现：一个 `u64` 哈希，由父容器 ID + 序号 + 可选标签混合而成。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct WidgetId(pub u64);

impl WidgetId {
    pub const NONE: WidgetId = WidgetId(0);

    /// 由父 ID + 序号 + 标签派生。
    ///
    /// 用手写 FNV-1a 而不是 `DefaultHasher`：后者在不同运行间**不保证稳定**
    /// （标准库明确说不保证），会导致同一份代码两次运行产生不同 ID。
    pub fn derive(parent: WidgetId, index: u32, label: &str) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        fn mix(h: &mut u64, v: u64) {
            *h ^= v;
            // FNV-1a 的 64 位素数：0x100000001b3（2^40 + 2^8 + 0xb3）。
            // 之前写成 0x0000_0100_0000_001b_3 是错的（分组错位，值实际等于 0x10000000_001b3
            // 少了一位），会让哈希分布明显变差、增加 ID 碰撞概率。
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        mix(&mut h, parent.0);
        mix(&mut h, index as u64);
        for b in label.as_bytes() {
            mix(&mut h, *b as u64);
        }
        WidgetId(h)
    }

    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_is_deterministic() {
        let p = WidgetId(12345);
        let a = WidgetId::derive(p, 7, "btn");
        let b = WidgetId::derive(p, 7, "btn");
        assert_eq!(a, b, "同一输入必须产生同一 ID");
    }

    #[test]
    fn derive_differs_by_index_and_label() {
        let p = WidgetId(999);
        let a = WidgetId::derive(p, 1, "x");
        let b = WidgetId::derive(p, 2, "x");
        let c = WidgetId::derive(p, 1, "y");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn derive_differs_by_parent() {
        let a = WidgetId::derive(WidgetId(1), 1, "x");
        let b = WidgetId::derive(WidgetId(2), 1, "x");
        assert_ne!(a, b, "不同父容器下同位置控件不能撞 ID");
    }

    #[test]
    fn none_is_zero() {
        assert!(WidgetId::NONE.is_none());
        assert!(!WidgetId::derive(WidgetId::NONE, 1, "x").is_none());
    }

    #[test]
    fn event_pos_extraction() {
        assert_eq!(Event::MouseMove { x: 3, y: 4 }.pos(), Some((3, 4)));
        assert_eq!(
            Event::KeyDown {
                key: Key::Enter,
                mods: crate::id::Modifiers::default()
            }
            .pos(),
            None
        );
        assert_eq!(Event::MouseLeave.pos(), None);
    }
}
