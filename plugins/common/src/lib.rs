//! 插件共享层：清单解析、参数 schema、context、结果构造、工具集合。
//!
//! 这一层**只依赖框架的公开 API**，不改框架任何代码。它存在的目的是让四个插件
//! 共用同一套"schema 与解析同源"的基础设施，而不是每个插件各写一份（那样必然漂移）。

#![deny(unsafe_code)]
// 注：不开 `#![warn(missing_docs)]` —— 这里是内部插件基础设施，关键 API 已有文档，
// 逐字段补齐只增加噪音。真正需要文档的是框架层的公开接口（那边已强制）。

pub mod arg_parse;
pub mod context;
pub mod fsutil;
pub mod json;
pub mod manifest;
pub mod registry;
pub mod result;
pub mod schema;
pub mod test_support;
pub mod tool_macro;

pub use context::{
    AllowListedHttp, CapturingLogger, HttpPort, HttpReply, LogLevel, NullLogger, PluginContext,
    PluginLogger, StderrLogger,
};
pub use manifest::{ManifestError, NetworkDecl, PluginManifestV1, ReviewKind, TrustKind};
pub use registry::{erase_handler, ToolDef, ToolSet};
pub use result::PluginResult;
pub use schema::{object_schema, Field, ToolArgs};

// 宏由 `#[macro_export]` 导出到 crate 根，直接用 `launcher_plugin_common::tool_def!` 即可。
// 不需要（也不能）在这里 re-export：同名 re-export 会与宏自身的根导出冲突（E0255）。
