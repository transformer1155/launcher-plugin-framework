//! `switch_instance`：切换到某个实例（记录"上次游玩时间"）。
//!
//! # 语义
//!
//! "切换"在本框架里是**只记录状态**，不启动游戏（启动是 `official-launch.launch_game`
//! 的职责）。工具返回 `{ ok }`，并更新该实例的 `last_played`。
//!
//! 这个职责划分让 `official-instance` 不依赖 `official-launch`，
//! 两个插件可以独立加载/卸载。
//!
//! # 为什么切换要写盘
//!
//! `last_played` 决定 `list_instances` 的默认排序（最近玩的在前）。如果只存在内存里，
//! 重启启动器后顺序就丢了 —— 用户会觉得"我明明刚玩过那个"。

use crate::store::now_rfc3339;
use crate::{load_store, save_store};
use launcher_plugin_common::{biz_fail, tool_args, tool_def};
use launcher_plugin_framework::error::ToolErrorCode;
use serde_json::json;

tool_args! {
    pub struct SwitchInstanceArgs {
        instance_id: r_str() = "要切换到的实例 ID",
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: SwitchInstanceArgs,
) -> launcher_plugin_common::PluginResult {
    let id = args.instance_id.trim();
    if id.is_empty() {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, "instance_id 不能为空"));
    }

    let mut store = load_store!(ctx);

    // 找到目标实例；找不到是**业务失败**（不是参数格式问题）。
    let Some(idx) = store.instances.iter().position(|i| i.id == id) else {
        ctx.log_warn(format!("切换失败：实例 {id} 不存在"));
        biz_fail!(
            launcher_plugin_common::result::biz_codes::NOT_FOUND,
            "实例 {id} 不存在"
        );
    };

    // 更新时间戳后再落盘。
    store.instances[idx].last_played = Some(now_rfc3339());
    let name = store.instances[idx].name.clone();
    save_store!(ctx, &store);

    ctx.log_info(format!("已切换到实例 {id}（{name}）"));

    Ok(launcher_plugin_common::result::success(json!({
        "ok": true,
        "instance_id": id,
        "last_played": store.instances[idx].last_played,
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "switch_instance",
        description: "切换到指定实例并记录游玩时间",
        destructive: false,
        timeout_ms: 5_000,
        args: SwitchInstanceArgs,
        handler: crate::tools::switch_instance::handle
    }
}
