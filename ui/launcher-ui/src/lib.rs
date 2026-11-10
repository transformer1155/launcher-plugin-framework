//! 启动器界面：业务层（实例 / Mod / 启动三页）。
//!
//! # 职责边界
//!
//! - **不认识 Win32/GDI**：只依赖 [`ui_core`] 的即时模式 API。
//! - **不直接碰文件**：所有数据通过插件工具（`official-instance` 等）获取。
//!   这样可以保证"UI 只是插件能力的呈现"，与插件架构一致。
//!
//! # 为什么 UI 走同步而非 async
//!
//! 即时模式 UI 的 `frame` 回调必须是同步的（后端消息循环驱动）。
//! 而插件工具是 async。桥接方式是：**UI 侧持有一个"数据快照"**，
//! 由后台线程周期性拉取刷新，`frame` 只读快照。
//!
//! 这样做的代价是数据有延迟（约 100ms），但换来的是 UI 永不阻塞 ——
//! 对启动器完全够用。

#![forbid(unsafe_code)]

pub mod app;
pub mod data;
pub mod domain;
pub mod onboarding;
pub mod pages;
pub mod shell;

pub use app::App;
