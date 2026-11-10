//! `crash_me`：故意崩溃，用于演示**沙箱崩溃 → 自动重启 → 恢复可用**。
//!
//! # 崩溃的后果（沙箱插件，不是可信插件）
//!
//! | 层级 | 结果 |
//! |---|---|
//! | handler | `panic!`，被框架的 `catch_unwind` 兜住 |
//! | 宿主进程 | **存活**（panic 没有逃逸） |
//! | 沙箱单元 | 置为 `Crashed`，触发 `RestartManager` |
//! | 调用方 | 得到 `isError: true` + `code: "ProcessCrashed"` |
//! | 其他工具 | 该插件全部工具短暂不可用（`ProcessRestarting`） |
//! | 恢复 | 指数退避后重建单元，工具重新可用 |
//!
//! 对比：如果这是**可信**插件，宿主会直接死。这正是"沙箱隔离"的价值。
//!
//! # 为什么要提供这个工具
//!
//! 它是可测试的故障注入点。没有它，"重启逻辑是否正确"只能靠人造事件验证
//! （那测的是重启器，不是真实链路）。有了它，端到端演示可以真的打崩一个单元。

use launcher_plugin_common::{tool_args, tool_def};
use serde_json::json;

tool_args! {
    pub struct CrashMeArgs {
        note: o_str() = "可选：崩溃前备注（会写进日志，便于定位是哪一次崩溃）",
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: CrashMeArgs,
) -> launcher_plugin_common::PluginResult {
    let note = args.note.unwrap_or_else(|| "no note".to_owned());

    // 崩溃前先留一条日志 —— 这样即使单元随后崩掉，也能在日志里看到"它是有意崩的"。
    ctx.log_warn(format!("crash_me 即将故意 panic（note={note}）"));

    // 故意 panic。框架会在 Future::poll 外层 catch_unwind 捕获它，
    // 宿主进程继续运行，但本沙箱单元会被标记为 Crashed。
    panic!("sample-community: 故意崩溃用于演示沙箱重启（note={note}）");

    // 下面这行永远到不了；写它是为了让返回类型正确（panic! 是 `!`，可以强转）。
    #[allow(unreachable_code)]
    Ok(launcher_plugin_common::result::success(json!({
        "unreachable": true
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "crash_me",
        description: "故意让沙箱单元崩溃（演示 ProcessCrashed → 自动重启）",
        destructive: true,
        timeout_ms: 5_000,
        args: CrashMeArgs,
        handler: crate::tools::crash_me::handle
    }
}
