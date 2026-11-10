//! `search_mods`：通过 Modrinth 在线搜索 Mod。
//!
//! # 网络能力缺失时的返回（按你裁决的方案 a）
//!
//! 宿主没有注入 `HttpPort` 时返回**业务错误** `upstream_error`，而不是框架的
//! `PermissionDenied`。理由：这属于"环境能力缺失"（UI 应显示"网络不可用，请检查启动器
//! 的网络组件"），不是"调用方越权"。
//!
//! 同样地，上游 5xx / 超时 / 响应格式异常都归为 `upstream_error` ——
//! 它们的共同点是"重试或稍后可能成功"，与参数错误截然不同。

use std::sync::Arc;

use crate::modrinth::{self, SearchRequest};
use launcher_plugin_common::context::HttpPort;
use launcher_plugin_common::result::{biz_codes, business};
use launcher_plugin_common::{tool_args, tool_def};
use launcher_plugin_framework::error::ToolErrorCode;
use serde_json::json;

tool_args! {
    pub struct SearchModsArgs {
        query: r_str() = "搜索关键词",
        loader: r_enum["vanilla", "fabric", "forge"] = "目标 Mod 加载器",
        game_version: r_str() = "Minecraft 版本，如 1.20.1",
        limit: o_u64(1, 50) = "返回条数（1-50，默认 20）",
    }
}

/// 默认返回条数。
const DEFAULT_LIMIT: u64 = 20;

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: SearchModsArgs,
) -> launcher_plugin_common::PluginResult {
    // ---- 参数校验（schema 已拦，这里是防御性检查，给出更可读的错误）----
    let query = args.query.trim();
    if query.is_empty() {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, "query 不能为空"));
    }
    if query.chars().count() > 100 {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, "query 最多 100 个字符"));
    }

    let ver = args.game_version.trim();
    if ver.is_empty() {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, "game_version 不能为空"));
    }

    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=50).contains(&limit) {
        return Err(ctx.err(
            ToolErrorCode::InvalidArguments,
            format!("limit 必须在 1..=50 之间，实际 {limit}"),
        ));
    }

    // ---- 取网络能力：缺失 → 业务错误 upstream_error（方案 a）----
    let http: Arc<dyn HttpPort> = match ctx.http() {
        Ok(h) => h,
        Err(e) => {
            ctx.log_warn(format!("搜索不可用：{}", e.message));
            return Ok(business(
                biz_codes::UPSTREAM_ERROR,
                "当前宿主未提供网络能力，无法在线搜索 Mod；请检查启动器的网络组件是否已装配",
            ));
        }
    };

    // ---- 请求上游 ----
    let req = SearchRequest {
        query: query.to_owned(),
        loader: args.loader.clone(),
        game_version: ver.to_owned(),
        limit: limit as u32,
    };

    match modrinth::search(&ctx, http, &req).await {
        Ok(hits) => {
            ctx.log_info(format!("搜索命中 {} 条", hits.len()));
            Ok(launcher_plugin_common::result::success(json!({
                "results": hits.iter().map(|h| json!({
                    "project_id": h.project_id,
                    "name": h.name,
                    "description": h.description,
                })).collect::<Vec<_>>(),
                // 让 UI 能显示"这些结果是模拟/实时"
                "source": "modrinth",
                "queried_at": modrinth::search_timestamp(),
            })))
        }
        Err(e) => {
            // 所有上游失败统一归为 upstream_error：重试可能成功。
            ctx.log_error(format!("Modrinth 搜索失败：{e}"));
            Ok(business(biz_codes::UPSTREAM_ERROR, e.to_string()))
        }
    }
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "search_mods",
        description: "在线搜索 Mod（数据源 Modrinth，需要宿主提供网络能力）",
        destructive: false,
        // 网络搜索比本地读写慢，给更宽的超时；但仍必须有上限（沙箱工具级超时优先）。
        timeout_ms: 15_000,
        args: SearchModsArgs,
        handler: crate::tools::search_mods::handle
    }
}
