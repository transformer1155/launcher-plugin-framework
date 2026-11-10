//! `create_instance`：创建一个游戏实例。
//!
//! # 校验顺序（与错误码一一对应）
//!
//! ```text
//! 1. 参数 schema（宏生成的解析器）      → InvalidArguments
//! 2. 实例名语义（空/超长/怪字符）       → InvalidArguments
//! 3. 版本格式                          → InvalidArguments
//! 4. loader 枚举                       → InvalidArguments
//! 5. 重名（大小写不敏感）               → 业务错误 already_exists
//! 6. 数量上限                          → 业务错误 limit_exceeded
//! 7. 写盘                              → 业务错误 storage_error
//! ```
//!
//! 前四步是"参数根本不合规"，用框架的 `InvalidArguments`；后三步是"参数合规但业务不允许"，
//! 用插件自己的业务错误码。这个边界决定了 UI 是否显示"请修正输入"还是"操作无法完成"。

use crate::store::{
    name_conflict, validate_game_version, validate_instance_name, Instance, InstanceStore, Loader,
};
use crate::{load_store, save_store};
use launcher_plugin_common::{biz_fail, tool_args, tool_def};
use launcher_plugin_framework::error::ToolErrorCode;
use serde_json::json;

/// 单个实例列表的上限。防止用户误点把目录刷爆。
const MAX_INSTANCES: usize = 200;

tool_args! {
    pub struct CreateInstanceArgs {
        name: r_str() = "实例名（1-64 字符，不能含路径分隔符）",
        game_version: r_str() = "Minecraft 版本，如 1.20.1",
        loader: r_enum["vanilla", "fabric", "forge"] = "Mod 加载器",
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: CreateInstanceArgs,
) -> launcher_plugin_common::PluginResult {
    // ---- 1) 实例名语义校验 ----
    let name = match validate_instance_name(&args.name) {
        Ok(n) => n,
        Err(msg) => {
            return Err(ctx.err(ToolErrorCode::InvalidArguments, msg));
        }
    };

    // ---- 2) 版本格式 ----
    let game_version = match validate_game_version(&args.game_version) {
        Ok(v) => v,
        Err(msg) => {
            return Err(ctx.err(ToolErrorCode::InvalidArguments, msg));
        }
    };

    // ---- 3) loader 枚举。schema 已拦过一次，这里做防御性检查并保持"唯一真相源" ----
    let loader = match Loader::parse(&args.loader) {
        Some(l) => l,
        None => {
            return Err(ctx.err(
                ToolErrorCode::InvalidArguments,
                format!(
                    "loader 必须是 {:?} 之一，实际 `{}`",
                    Loader::ALL,
                    args.loader
                ),
            ));
        }
    };

    // ---- 4) 读存储 ----
    let mut store = load_store!(ctx);

    // ---- 5) 重名（大小写不敏感）----
    if let Some(existing_id) = name_conflict(&store.instances, name, None) {
        ctx.log_warn(format!("创建失败：实例名 `{name}` 与 {existing_id} 冲突"));
        biz_fail!(
            launcher_plugin_common::result::biz_codes::ALREADY_EXISTS,
            "已存在同名实例（{existing_id}）；实例名不区分大小写"
        );
    }

    // ---- 6) 数量上限 ----
    if store.instances.len() >= MAX_INSTANCES {
        biz_fail!(
            launcher_plugin_common::result::biz_codes::LIMIT_EXCEEDED,
            "实例数量已达上限 {MAX_INSTANCES}，请先删除不再使用的实例"
        );
    }

    // ---- 7) 落盘 ----
    let id = InstanceStore::next_id_for(&store.instances);
    let instance = Instance {
        id: id.clone(),
        name: name.to_owned(),
        game_version: game_version.to_owned(),
        loader,
        last_played: None,
    };
    store.instances.push(instance);
    save_store!(ctx, &store);

    ctx.log_info(format!(
        "已创建实例 {id}（{name} / {game_version} / {loader}）"
    ));

    Ok(launcher_plugin_common::result::success(json!({
        "instance_id": id,
        // 同时回传完整对象，UI 不必再调一次 list_instances。
        "instance": {
            "id": id,
            "name": store.instances.last().map(|i| i.name.clone()).unwrap_or_default(),
            "game_version": game_version,
            "loader": loader.as_str(),
            "last_played": null,
        }
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "create_instance",
        description: "创建一个新的游戏实例",
        destructive: false,
        timeout_ms: 5_000,
        args: CreateInstanceArgs,
        handler: crate::tools::create_instance::handle
    }
}
