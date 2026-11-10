//! `official-launch`：启动参数与游戏启动（可信插件）。
//!
//! # 关键约束：不创建 OS 进程
//!
//! 框架的进程模型是"单 OS 进程、无子进程"。因此 `launch_game` 是**模拟启动**：
//! 返回 `pid: null` + `simulated: true`，不 spawn 任何进程。
//!
//! 真实启动需要业务层实现，且必须与本框架的进程模型重新对齐 ——
//! 这是一个**架构级问题**，不是插件能自行决定的（见 README）。
//!
//! # 工具
//!
//! | 短名 | destructive | 说明 |
//! |---|---|---|
//! | `get_launch_config` | 否 | 读取配置（无配置返回默认值） |
//! | `set_launch_config` | **是** | 全量覆盖配置 |
//! | `launch_game` | **是** | 模拟启动，pid=null |
//! | `get_last_log` | 否 | 读日志尾部 |

#![deny(unsafe_code)]

pub mod store;
pub mod tools;

use std::sync::Arc;

use launcher_plugin_common::{PluginContext, PluginLogger, StderrLogger, ToolSet};

/// 插件 ID。**必须与 `plugin.json` 的 `id` 一致**。
pub const PLUGIN_ID: &str = "com.example.official-launch";

pub fn tools() -> ToolSet {
    ToolSet::new(
        PLUGIN_ID,
        vec![
            tools::get_launch_config::def(),
            tools::set_launch_config::def(),
            tools::launch_game::def(),
            tools::get_last_log::def(),
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
    (ctx, tools())
}

pub fn declared_tools() -> Vec<&'static str> {
    tools().manifest_tools()
}
