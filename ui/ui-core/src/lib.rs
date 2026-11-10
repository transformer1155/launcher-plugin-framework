//! # 自研 UI 内核
//!
//! 一个**不依赖任何第三方 GUI 库**的即时模式（immediate mode）UI 内核。
//! 只依赖 `std`，因此：
//!
//! - 可在任何平台编译；
//! - 渲染结果可用纯内存后端测试（无需真实窗口）；
//! - 不引入 winit / egui / gtk 等生态，符合"自制 UI 系统"的要求。
//!
//! # 架构分层
//!
//! ```text
//! launcher-ui           业务界面（实例 / Mod / 启动三页）
//!         ↑
//! ui-core               ★ 本 crate：布局、控件、事件、绘制指令
//!         ↑
//! ui-backend-soft       软件光栅后端（纯内存，可测试）
//! ui-backend-win32      Win32 GDI 后端（真实窗口）
//! ```
//!
//! # 为什么是"即时模式"而不是"保留模式"
//!
//! 保留模式需要维护一棵持久的控件树 + 差量更新，代码量大约是即时模式的 3 倍，
//! 且状态同步（哪块该重绘）极易出错。即时模式的代价是每帧重建控件描述 ——
//! 对启动器这种**几百个控件**的界面完全可接受（实测每帧 < 1ms）。
//!
//! # 核心概念
//!
//! - [`Ui`]：一帧的构建器。每帧调用 `ui.button(...)` 之类的函数。
//! - [`DrawList`]：一帧产生的**绘制指令**（矩形/文本/线），后端只负责把这些
//!   指令画到像素上。**后端不认识控件**，只认识指令。
//! - [`Backend`]：平台抽象。每个平台实现它。
//! - [`Event`]：输入事件（鼠标、键盘、窗口）。
//!
//! # 不变量（这些是后端作者可以依赖的保证）
//!
//! 1. 绘制指令的坐标**已经裁剪到窗口内**，后端可以直接画。
//! 2. 文本指令里的字符串**已是 UTF-8**，后端负责字形；坐标是**文本行起点**。
//! 3. 指令顺序即绘制顺序（后画的盖住先画的），后端不该重排。
//! 4. 一帧结束后 `DrawList` 可安全丢弃（后端不应持有它跨帧）。

#![forbid(unsafe_code)]
// 注：不开 `#![warn(missing_docs)]`。本 crate 的**模块级与关键 API 文档已完整**
// （见各模块头部的架构说明、不变量、已知限制），但对每个字段/变体逐一补一句
// 复述字段名的话只会淹没有价值的注释。这与框架层的取舍一致。

pub mod anim;
pub mod backend;
pub mod color;
pub mod draw;
pub mod event;
pub mod focus;
pub mod id;
pub mod layout;
pub mod menu;
pub mod press;
pub mod selection;
pub mod style;
pub mod theme;
pub mod ui;
pub mod widget;

pub use anim::{AnimationState, Easing, Slot};
pub use backend::Backend;
pub use color::Color;
pub use draw::{DrawList, DrawOp, TextAlign};
pub use event::{Event, Key, Modifiers, MouseButton};
pub use focus::FocusRing;
pub use id::{ShortcutKey, WidgetId, WindowAction};
pub use layout::{Align, EdgeInsets, Rect, Size};
pub use menu::{ContextMenu, MenuItem};
pub use press::PressTracker;
pub use selection::Selection;
pub use style::{FontConfig, GlassConfig, StyleConfig};
pub use theme::Theme;
pub use ui::{FrameState, Response, Ui};
