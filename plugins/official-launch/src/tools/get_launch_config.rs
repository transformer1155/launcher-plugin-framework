//! `get_launch_config`：读取实例的启动参数。
//!
//! 实例从未配置过时返回**默认配置**并标 `"is_default": true`，
//! 而不是报错 —— 新实例第一次打开设置页应当看到合理的默认值，而不是空白。

use crate::load_launch_store;
use crate::store::{validate_instance_id, LaunchConfig};
use launcher_plugin_common::result::invalid_args;
use launcher_plugin_common::{tool_args, tool_def};
use serde_json::json;

tool_args! {
    pub struct GetLaunchConfigArgs {
        instance_id: r_str() = "实例 ID",
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: GetLaunchConfigArgs,
) -> launcher_plugin_common::PluginResult {
    let instance_id = match validate_instance_id(&args.instance_id) {
        Ok(v) => v.to_owned(),
        Err(msg) => return Err(invalid_args(&ctx, msg)),
    };

    let store = load_launch_store!(ctx);
    let (config, is_default) = match store.configs.get(&instance_id) {
        Some(c) => (c.clone(), false),
        None => (LaunchConfig::default(), true),
    };

    ctx.log_info(format!(
        "读取实例 {instance_id} 的启动配置（is_default={is_default}）"
    ));

    Ok(launcher_plugin_common::result::success(json!({
        "jvm_args": config.jvm_args,
        "game_args": config.game_args,
        "memory_mb": config.memory_mb,
        // 让 UI 能显示"当前是默认值，尚未自定义"
        "is_default": is_default,
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "get_launch_config",
        description: "读取实例的启动参数（未配置时返回默认值）",
        destructive: false,
        timeout_ms: 5_000,
        args: GetLaunchConfigArgs,
        handler: crate::tools::get_launch_config::handle
    }
}
