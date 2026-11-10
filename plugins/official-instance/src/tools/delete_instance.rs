//! `delete_instance`：删除实例。
//!
//! # 二次确认（规格第 4 条）
//!
//! 规格要求：**"destructive 工具在 handler 内二次确认参数（如 delete_instance 的
//! confirm），不依赖调用方"**。
//!
//! 因此 `confirm=false` 时在 handler 内直接拒绝，返回 `InvalidArguments`。
//! 注意这不是"靠 UI 弹窗就够了"—— UI 可以被绕过（MCP 客户端直接调），
//! handler 内的检查才是唯一有效的。
//!
//! `delete_instance` 的 `destructive` 标记为 **true**，框架会把它透出给 UI，
//! 让 UI 知道"这个操作要弹确认框"。两道关卡职责不同：
//!
//! | 关卡 | 位置 | 作用 |
//! |---|---|---|
//! | `destructive: true` | 元数据 | **告知** UI 需要确认 |
//! | `confirm: bool` | handler 内 | **强制**确认（无法绕过） |
//!
//! # 删除是"从索引移除"，不是"删目录"
//!
//! 本框架只管实例索引。实例目录里的游戏文件属于业务层，由启动器在收到成功响应后
//! 自行处理（或由专门工具处理）。这样 `official-instance` 不会意外删掉用户数据。

use crate::{load_store, save_store};
use launcher_plugin_common::{biz_fail, tool_args, tool_def};
use launcher_plugin_framework::error::ToolErrorCode;
use serde_json::json;

tool_args! {
    pub struct DeleteInstanceArgs {
        instance_id: r_str() = "要删除的实例 ID",
        confirm: r_bool() = "二次确认，必须为 true；false 会被拒绝",
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: DeleteInstanceArgs,
) -> launcher_plugin_common::PluginResult {
    // ---- 二次确认：handler 内强制，不依赖调用方 ----
    if !args.confirm {
        ctx.log_warn(format!(
            "拒绝删除实例 {}：confirm=false（未二次确认）",
            args.instance_id
        ));
        return Err(ctx.err(
            ToolErrorCode::InvalidArguments,
            "删除实例必须显式二次确认：请把 confirm 设为 true",
        ));
    }

    let id = args.instance_id.trim();
    if id.is_empty() {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, "instance_id 不能为空"));
    }

    let mut store = load_store!(ctx);

    let Some(pos) = store.instances.iter().position(|i| i.id == id) else {
        ctx.log_warn(format!("删除失败：实例 {id} 不存在"));
        biz_fail!(
            launcher_plugin_common::result::biz_codes::NOT_FOUND,
            "实例 {id} 不存在"
        );
    };

    let removed = store.instances.remove(pos);
    save_store!(ctx, &store);

    ctx.log_info(format!(
        "已删除实例 {}（{} / {} / {}）",
        removed.id, removed.name, removed.game_version, removed.loader
    ));

    Ok(launcher_plugin_common::result::success(json!({
        "ok": true,
        "instance_id": id,
        "deleted": {
            "id": removed.id,
            "name": removed.name,
            "game_version": removed.game_version,
            "loader": removed.loader.as_str(),
        },
        // 明确告知调用方"只删了索引"，避免它以为游戏文件也没了。
        "note": "仅从实例索引移除；游戏文件未删除",
        "remaining": store.instances.len(),
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "delete_instance",
        description: "删除指定实例（必须二次确认 confirm=true）",
        destructive: true,
        timeout_ms: 5_000,
        args: DeleteInstanceArgs,
        handler: crate::tools::delete_instance::handle
    }
}
