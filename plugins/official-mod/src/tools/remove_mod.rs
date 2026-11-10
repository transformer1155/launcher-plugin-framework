//! `remove_mod`：从实例移除 Mod。
//!
//! # destructive 语义
//!
//! 标记 `destructive: true`，但**不需要** `confirm` 参数 ——
//! 移除 Mod 是可逆操作（重新安装即可），且用户明确指定了文件名。
//! 这与 `delete_instance` 的区别是刻意的：删除整个实例会丢存档，不可逆。
//!
//! # 副作用
//!
//! 1. 从 `mods.json` 移除记录
//! 2. 删除磁盘上的文件（如果存在）
//!
//! 文件不存在时不报错（幂等）：索引里有记录但文件被手动删了是常见状态，
//! 此时"移除"应当成功，而不是因为文件缺失就失败。

use crate::load_mod_store;
use crate::save_mod_store;
use crate::store::{mods_dir_for, validate_instance_id, validate_mod_filename};
use launcher_plugin_common::result::biz_codes;
use launcher_plugin_common::{biz_fail, tool_args, tool_def};
use launcher_plugin_framework::error::ToolErrorCode;
use serde_json::json;

tool_args! {
    pub struct RemoveModArgs {
        instance_id: r_str() = "目标实例 ID",
        filename: r_str() = "要移除的 Mod 文件名（含 .jar）",
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: RemoveModArgs,
) -> launcher_plugin_common::PluginResult {
    let instance_id = match validate_instance_id(&args.instance_id) {
        Ok(v) => v.to_owned(),
        Err(msg) => return Err(ctx.err(ToolErrorCode::InvalidArguments, msg)),
    };

    let filename = match validate_mod_filename(&args.filename) {
        Ok(v) => v.to_owned(),
        Err(msg) => return Err(ctx.err(ToolErrorCode::InvalidArguments, msg)),
    };

    let mut store = load_mod_store!(ctx);

    let Some(mods) = store.instances.get_mut(&instance_id) else {
        ctx.log_warn(format!("移除失败：实例 {instance_id} 没有任何 Mod 记录"));
        biz_fail!(
            biz_codes::NOT_FOUND,
            "实例 {instance_id} 没有已安装的 Mod 记录"
        );
    };

    let Some(pos) = mods.iter().position(|m| m.filename == filename) else {
        ctx.log_warn(format!(
            "移除失败：{filename} 不在实例 {instance_id} 的记录里"
        ));
        biz_fail!(
            biz_codes::NOT_FOUND,
            "实例 {instance_id} 未安装名为 {filename} 的 Mod"
        );
    };

    let removed = mods.remove(pos);

    // 删磁盘文件：不存在不算错（幂等）。
    let file_path = ctx
        .data_path(&mods_dir_for(&instance_id))
        .map(|d| d.join(&filename));
    let file_deleted = match file_path {
        Ok(p) if p.exists() => match std::fs::remove_file(&p) {
            Ok(_) => true,
            Err(e) => {
                ctx.log_error(format!("删除 Mod 文件失败：{e}"));
                // 索引已移除，但文件还在 —— 必须如实告知，不能假装干净。
                return Ok(launcher_plugin_common::result::business(
                    biz_codes::STORAGE_ERROR,
                    format!("已从索引移除，但删除文件失败：{e}（{filename}）"),
                ));
            }
        },
        _ => false,
    };

    // 若该实例已无 Mod，清掉空 key，避免 mods.json 里堆积空数组。
    if store
        .instances
        .get(&instance_id)
        .is_some_and(|v| v.is_empty())
    {
        store.instances.remove(&instance_id);
    }

    save_mod_store!(ctx, &store);

    ctx.log_info(format!(
        "已从实例 {instance_id} 移除 Mod {filename}（文件{}）",
        if file_deleted {
            "已删除"
        } else {
            "不存在，跳过"
        }
    ));

    Ok(launcher_plugin_common::result::success(json!({
        "ok": true,
        "instance_id": instance_id,
        "filename": filename,
        "removed": {
            "name": removed.name,
            "version": removed.version,
            "project_id": removed.project_id,
        },
        "file_deleted": file_deleted,
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "remove_mod",
        description: "从实例移除指定 Mod（同时删除磁盘文件）",
        destructive: true,
        timeout_ms: 5_000,
        args: RemoveModArgs,
        handler: crate::tools::remove_mod::handle
    }
}
