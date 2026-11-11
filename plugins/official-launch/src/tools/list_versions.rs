//! `list_versions`：拉取 Mojang 官方版本清单，返回**可安装**的 Minecraft 版本列表。
//!
//! 供 UI 的安装表单做"点选版本"而不是手输版本号——手输极易拼错
//! （`1.21.1 ` 带空格、`1,21`、不存在的快照号……都会让 `install_minecraft`
//! 在清单里查不到而失败）。版本 id 直接来自清单，点选即合法。

use launcher_plugin_common::{result, tool_args, tool_def, PluginContext, PluginResult};
use serde_json::{json, Value};

tool_args! {
    pub struct ListVersionsArgs {
        // 版本类型过滤：release（正式版，缺省）/ snapshot / old_beta / old_alpha / all。
        kind: o_str() = "版本类型：release（缺省）/ snapshot / old_beta / old_alpha / all",
    }
}

async fn get_json(
    http: &std::sync::Arc<dyn launcher_plugin_common::context::HttpPort>,
    url: &str,
) -> Result<Value, String> {
    let reply = http.get(url.to_owned()).await?;
    if reply.status != 200 {
        return Err(format!("HTTP {}（{url}）", reply.status));
    }
    serde_json::from_slice(&reply.body).map_err(|e| format!("JSON 解析失败（{url}）：{e}"))
}

pub async fn handle(ctx: PluginContext, args: ListVersionsArgs) -> PluginResult {
    let kind = args
        .kind
        .clone()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "release".to_owned());
    if !["release", "snapshot", "old_beta", "old_alpha", "all"].contains(&kind.as_str()) {
        return Ok(result::business(
            result::biz_codes::UNSUPPORTED,
            format!("未知的版本类型 `{kind}`"),
        ));
    }

    let Ok(http) = ctx.http() else {
        return Ok(result::business(
            result::biz_codes::UPSTREAM_ERROR,
            "网络能力不可用（宿主未注入 HTTP 端口）",
        ));
    };

    let manifest = match get_json(&http, "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json").await {
        Ok(m) => m,
        Err(e) => {
            ctx.log_error(format!("拉取版本清单失败：{e}"));
            return Ok(result::business(
                result::biz_codes::UPSTREAM_ERROR,
                format!("拉取版本清单失败：{e}"),
            ));
        }
    };
    let Some(versions) = manifest.get("versions").and_then(|v| v.as_array()) else {
        return Ok(result::business(
            result::biz_codes::UPSTREAM_ERROR,
            "版本清单格式异常（缺少 versions 数组）",
        ));
    };

    let list: Vec<Value> = versions
        .iter()
        .filter(|v| {
            kind == "all"
                || v.get("type").and_then(|x| x.as_str()) == Some(kind.as_str())
        })
        .map(|v| {
            json!({
                "id": v.get("id").and_then(|x| x.as_str()).unwrap_or_default(),
                "type": v.get("type").and_then(|x| x.as_str()).unwrap_or_default(),
                "release_time": v.get("releaseTime").and_then(|x| x.as_str()).unwrap_or_default(),
            })
        })
        .collect();
    let total = list.len();
    ctx.log_info(format!("已拉取版本清单（{kind}，{total} 个）"));

    Ok(result::success(json!({
        "kind": kind,
        "count": total,
        "versions": list,
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "list_versions",
        description: "拉取 Mojang 官方可安装版本清单（可按 release/snapshot 等类型过滤）",
        destructive: false,
        timeout_ms: 30_000,
        args: ListVersionsArgs,
        handler: crate::tools::list_versions::handle
    }
}
