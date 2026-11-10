//! Win32 GDI 后端（真正的实现在 `imp` 模块，仅 Windows 编译）。
//!
//! 其他平台上本 crate 是空的，保证 workspace 整体可编译。
//!
//! # 关于 `unsafe`
//!
//! 本 crate **必须**用 `unsafe`：直接调用 Win32 API 是 `unsafe` 操作。
//! 这是唯一被允许放开 `unsafe_code` 的 crate —— UI 内核与业务层仍然
//! `#![deny(unsafe_code)]`，因此 unsafe 面积被**限制在一个文件**里
//! （`imp.rs`），便于审计。

#![cfg_attr(target_os = "windows", allow(unsafe_code))]
#![cfg_attr(not(target_os = "windows"), deny(unsafe_code))]

#[cfg(target_os = "windows")]
pub use imp::*;

#[cfg(target_os = "windows")]
mod imp;

/// 非 Windows 平台上没有实现。调用方应当用 `#[cfg]` 分支选择后端。
#[cfg(not(target_os = "windows"))]
pub mod unavailable {
    /// 说明该平台没有 Win32 后端。
    pub const REASON: &str = "Win32 后端仅在 Windows 上可用；其他平台请用软件后端或实现自己的后端";
}
