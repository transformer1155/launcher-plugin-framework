//! `slow_task`：耗时任务，用于演示**可中断超时**。
//!
//! # 为什么用 `tokio::time::sleep` 而不是 `std::thread::sleep`
//!
//! 这是演示的核心。两种实现对应框架的两种超时结局：
//!
//! | 实现 | handler 状态 | 框架判定 | 返回码 |
//! |---|---|---|---|
//! | `tokio::time::sleep` | parked（`in_poll == false`） | **可中断** | `Timeout` |
//! | `std::thread::sleep` | 卡在 poll 内（`in_poll == true`） | **不可中断** | `Uninterruptible` + 单元崩溃 |
//!
//! 本工具用前者，因此超时后返回 `Timeout`，单元**保持 Running**，不触发重启。
//! 另一个工具 `burn_cpu`（本示例不暴露）才用后者。
//!
//! 这样演示时能清楚看到：可中断超时 ≠ 崩溃。

use std::time::Duration;

use launcher_plugin_common::{tool_args, tool_def};
use serde_json::json;

tool_args! {
    pub struct SlowTaskArgs {
        seconds: o_u64(1, 30) = "要睡多久（1-30 秒，默认 3）",
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: SlowTaskArgs,
) -> launcher_plugin_common::PluginResult {
    let seconds = args.seconds.unwrap_or(3);
    if !(1..=30).contains(&seconds) {
        return Err(ctx.err(
            launcher_plugin_framework::error::ToolErrorCode::InvalidArguments,
            format!("seconds 必须在 1..=30 之间，实际 {seconds}"),
        ));
    }

    ctx.log_info(format!("slow_task 开始，将 sleep {seconds} 秒（可被中断）"));

    // 关键：必须用 tokio 的 sleep —— 它在等待期间不占用 poll，因此可被 abort。
    tokio::time::sleep(Duration::from_secs(seconds)).await;

    ctx.log_info(format!("slow_task 完成（{seconds} 秒）"));

    Ok(launcher_plugin_common::result::success(json!({
        "done": true,
        "slept_seconds": seconds,
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "slow_task",
        description: "耗时任务（可中断；超时返回 Timeout 而不是崩溃）",
        destructive: false,
        // 故意设一个很短的超时，方便演示"超时"这个分支。
        // 真实插件应设为业务预期的上限。
        timeout_ms: 2_000,
        args: SlowTaskArgs,
        handler: crate::tools::slow_task::handle
    }
}
