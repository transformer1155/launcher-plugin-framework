//! `official-mod`：Mod 管理（可信插件）。
//!
//! # 信任级别
//!
//! `plugin.json` 声明 `trust: "trusted"`、`review: "official"`。
//! 实际信任级别由框架签名校验决定。
//!
//! **本插件需要网络访问**（`network.allow_hosts: ["api.modrinth.com"]`），
//! 因此宿主必须注入实现了白名单强制的 [`launcher_plugin_common::HttpPort`]
//! （推荐直接用 `AllowListedHttp` 包一层）。未注入时 `search_mods` 返回业务错误
//! `upstream_error`，其余三个工具正常工作。
//!
//! # 工具
//!
//! | 短名 | 网络 | destructive | 说明 |
//! |---|---|---|---|
//! | `list_installed_mods` | 否 | 否 | 列出已安装 Mod |
//! | `search_mods` | **是** | 否 | Modrinth 在线搜索 |
//! | `install_mod` | 否 | **是** | 模拟安装（写占位文件） |
//! | `remove_mod` | 否 | **是** | 移除 Mod + 删文件 |

#![deny(unsafe_code)]

pub mod modrinth;
pub mod store;
pub mod tools;

use std::sync::Arc;

use launcher_plugin_common::{PluginContext, PluginLogger, StderrLogger, ToolSet};

/// 插件 ID。**必须与 `plugin.json` 的 `id` 一致**。
pub const PLUGIN_ID: &str = "com.example.official-mod";

/// 本插件声明需要访问的域名。与 `plugin.json` 的 `network.allow_hosts` 必须一致
/// （测试 `manifest_network_matches_code` 会断言这一点）。
pub const REQUIRED_HOSTS: [&str; 1] = ["api.modrinth.com"];

/// 构造全部工具。
pub fn tools() -> ToolSet {
    ToolSet::new(
        PLUGIN_ID,
        vec![
            tools::list_installed_mods::def(),
            tools::search_mods::def(),
            tools::install_mod::def(),
            tools::remove_mod::def(),
        ],
    )
}

/// 构造上下文（含日志）。网络能力由宿主后续 `.with_http(..)` 注入。
pub fn build(data_dir: impl Into<std::path::PathBuf>) -> (PluginContext, ToolSet) {
    build_with_logger(data_dir, Arc::new(StderrLogger))
}

/// 可指定日志实现（测试用）。
pub fn build_with_logger(
    data_dir: impl Into<std::path::PathBuf>,
    logger: Arc<dyn PluginLogger>,
) -> (PluginContext, ToolSet) {
    let ctx = PluginContext::new(PLUGIN_ID, data_dir, logger);
    (ctx, tools())
}

/// 清单声明的工具短名（供一致性校验）。
pub fn declared_tools() -> Vec<&'static str> {
    tools().manifest_tools()
}
