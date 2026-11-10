//! `sample-community`：示例社区插件（**沙箱**，无签名）。
//!
#![doc = ""]
//! # 这个插件的用途
//!
//! 它**不是**业务插件，是一个**可执行的演示/回归夹具**，用来验证框架的三条关键路径：
//!
//! | 工具 | 演示的框架能力 |
//! |---|---|
//! | `hello` | 无签名插件降级到沙箱后**仍然能正常工作** |
//! | `slow_task` | 可中断超时 → `Timeout`，单元**不崩溃** |
//! | `crash_me` | 沙箱 panic → `ProcessCrashed` → 自动重启 → 恢复可用 |
//!
//! # 为什么无签名
//!
//! `plugin.json` 声明 `trust: "sandboxed"`、`review: "unreviewed"`，且**没有 `signature` 字段**。
//! 因此无论宿主的验签器怎么配，它都会被 loader 降级到沙箱 —— 这正是要演示的路径。
//!
//! 同时它触发 Q8 要求的 WARN 日志："沙箱插件未人工审核"。
//!
//! # 它为什么能碰文件系统
//!
//! 它能 —— 因为沙箱是**故障隔离**，不是安全隔离（Q8 已裁决保持当前 Rust 单元架构）。
//! 本插件刻意只写自己的私有数据目录，不碰其他位置；但这靠的是"插件守规矩"，
//! 不是"框架拦得住"。这一条写在 `docs/` 与框架的 `security-boundary.md` 里。

pub mod tools;

use std::sync::Arc;

use launcher_plugin_common::{PluginContext, PluginLogger, StderrLogger, ToolSet};

/// 插件 ID。**必须与 `plugin.json` 的 `id` 一致**。
pub const PLUGIN_ID: &str = "com.example.sample-community";

pub fn tools() -> ToolSet {
    ToolSet::new(
        PLUGIN_ID,
        vec![
            tools::hello::def(),
            tools::slow_task::def(),
            tools::crash_me::def(),
        ],
    )
}

pub fn build(data_dir: impl Into<std::path::PathBuf>) -> (PluginContext, ToolSet) {
    build_with_logger(data_dir, Arc::new(StderrLogger))
}

pub fn build_with_logger(
    data_dir: impl Into<std::path::PathBuf>,
    logger: Arc<dyn PluginLogger>,
) -> (PluginContext, ToolSet) {
    let ctx = PluginContext::new(PLUGIN_ID, data_dir, logger);
    (ctx, tool_set())
}

/// 内部别名（避免与 `tools()` 重名混淆）。
fn tool_set() -> ToolSet {
    tools()
}

pub fn declared_tools() -> Vec<&'static str> {
    tools().manifest_tools()
}
