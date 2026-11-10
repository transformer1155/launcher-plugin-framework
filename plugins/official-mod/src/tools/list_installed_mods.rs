//! `list_installed_mods`：列出某实例已安装的 Mod。
//!
//! 只读工具：索引文件不存在时返回空列表（与 `list_instances` 同一策略）。

use crate::load_mod_store;
use crate::store::validate_instance_id;
use launcher_plugin_common::result::invalid_args;
use launcher_plugin_common::{tool_args, tool_def};
use serde_json::json;

tool_args! {
    pub struct ListInstalledModsArgs {
        instance_id: r_str() = "实例 ID",
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: ListInstalledModsArgs,
) -> launcher_plugin_common::PluginResult {
    let instance_id = match validate_instance_id(&args.instance_id) {
        Ok(v) => v.to_owned(),
        Err(msg) => return Err(invalid_args(&ctx, msg)),
    };

    let store = load_mod_store!(ctx);
    let mods = store
        .instances
        .get(&instance_id)
        .cloned()
        .unwrap_or_default();

    // 按文件名排序：UI 列表顺序必须稳定，否则每次刷新都在跳。
    let mut sorted = mods;
    sorted.sort_by(|a, b| a.filename.cmp(&b.filename));

    ctx.log_info(format!("实例 {instance_id} 已安装 {} 个 Mod", sorted.len()));

    Ok(launcher_plugin_common::result::success(json!({
        "mods": sorted.iter().map(|m| json!({
            "filename": m.filename,
            "name": m.name,
            "version": m.version,
            "project_id": m.project_id,
            "installed_at": m.installed_at,
        })).collect::<Vec<_>>()
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "list_installed_mods",
        description: "列出指定实例已安装的 Mod",
        destructive: false,
        timeout_ms: 5_000,
        args: ListInstalledModsArgs,
        handler: crate::tools::list_installed_mods::handle
    }
}
