//! `hello`：打招呼。
//!
//! 最简单的沙箱工具：证明"无签名插件降级到沙箱后仍然能正常工作"。
//! 如果这个工具能调通，说明降级路径没有把插件功能性弄坏。

use launcher_plugin_common::{tool_args, tool_def};
use serde_json::json;

tool_args! {
    pub struct HelloArgs {
        name: r_str() = "你的名字",
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: HelloArgs,
) -> launcher_plugin_common::PluginResult {
    let name = args.name.trim();
    if name.is_empty() {
        return Err(ctx.err(
            launcher_plugin_framework::error::ToolErrorCode::InvalidArguments,
            "name 不能为空",
        ));
    }
    if name.chars().count() > 64 {
        return Err(ctx.err(
            launcher_plugin_framework::error::ToolErrorCode::InvalidArguments,
            "name 最多 64 个字符",
        ));
    }

    ctx.log_info(format!("hello 被调用（name={name}）"));

    Ok(launcher_plugin_common::result::success(json!({
        "greeting": format!("你好，{name}！"),
        // 明确标注当前信任级别，便于演示时观察"这个插件跑在沙箱里"
        "sandboxed": true,
        "plugin": ctx.plugin_id_str(),
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "hello",
        description: "打招呼（沙箱工具）",
        destructive: false,
        timeout_ms: 5_000,
        args: HelloArgs,
        handler: crate::tools::hello::handle
    }
}
