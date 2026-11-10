//! 框架 JSON Schema 校验的 re-export。
//!
//! 宏（`tool_args!`）展开在**调用方 crate** 里，因此宏内不能直接写
//! `launcher_plugin_framework::...`（调用方不一定依赖它）。统一走 `$crate::json::*`
//! 指向这里，宏就只依赖 `launcher_plugin_common` 一个 crate。

pub use launcher_plugin_framework::json::{validate, validate_schema, SchemaError};
