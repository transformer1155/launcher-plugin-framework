//! `get_last_log`：读取实例最近 `lines` 行日志。
//!
//! # 大文件处理
//!
//! 日志可能上百 MB。本工具只读文件尾部（见 `store::read_log_tail`），
//! 且读完后仍可能超过沙箱的返回值体积配额 —— 因此这里再做一个**字符级截断**：
//! 若结果超过 `MAX_OUTPUT_CHARS`，保留最新的部分并在开头标注"已截断"。
//!
//! 这样即使调用方要 5000 行，也不会因为触发 `SandboxQuotaExceeded` 而拿不到任何东西。

use crate::store::{log_path_for, read_log_tail, validate_instance_id};
use launcher_plugin_common::result::{biz_codes, business};
use launcher_plugin_common::{biz_fail, tool_args, tool_def};
use launcher_plugin_framework::error::ToolErrorCode;
use serde_json::json;

tool_args! {
    pub struct GetLastLogArgs {
        instance_id: r_str() = "实例 ID",
        lines: o_u64(1, 2000) = "读取最近多少行（1-2000，默认 200）",
    }
}

const DEFAULT_LINES: usize = 200;
/// 返回内容的最大字符数（约 256KB 文本）。
const MAX_OUTPUT_CHARS: usize = 256 * 1024;

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: GetLastLogArgs,
) -> launcher_plugin_common::PluginResult {
    let instance_id = match validate_instance_id(&args.instance_id) {
        Ok(v) => v.to_owned(),
        Err(msg) => return Err(ctx.err(ToolErrorCode::InvalidArguments, msg)),
    };

    let lines = args.lines.unwrap_or(DEFAULT_LINES as u64);
    if !(1..=2000).contains(&lines) {
        return Err(ctx.err(
            ToolErrorCode::InvalidArguments,
            format!("lines 必须在 1..=2000 之间，实际 {lines}"),
        ));
    }

    let path = ctx.data_path(&log_path_for(&instance_id))?;

    if !path.exists() {
        ctx.log_info(format!("实例 {instance_id} 暂无日志（可能从未启动过）"));
        biz_fail!(
            biz_codes::NOT_FOUND,
            "实例 {instance_id} 暂无日志文件（可能从未启动过）"
        );
    }

    let (content, reached_start) = match read_log_tail(&path, lines as usize) {
        Ok(v) => v,
        Err(msg) => {
            ctx.log_error(format!("读取日志失败：{msg}"));
            return Ok(business(biz_codes::STORAGE_ERROR, msg));
        }
    };

    // ---- 输出体积保护：超限则保留尾部 ----
    let (content, truncated) = if content.chars().count() > MAX_OUTPUT_CHARS {
        // 按字符截断（不能用字节切片，否则多字节字符会被切断）。
        let skip = content.chars().count() - MAX_OUTPUT_CHARS;
        let tail: String = content.chars().skip(skip).collect();
        (format!("[...前 {skip} 个字符已截断...]\n{tail}"), true)
    } else {
        (content, false)
    };

    ctx.log_info(format!(
        "读取实例 {instance_id} 的日志 {lines} 行（{} 字符，truncated={truncated}）",
        content.chars().count()
    ));

    Ok(launcher_plugin_common::result::success(json!({
        "content": content,
        "lines": lines,
        // 让 UI 知道这是"文件全部内容"还是"只是尾部"
        "reached_start": reached_start,
        "truncated": truncated,
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "get_last_log",
        description: "读取实例的最近日志（只读文件尾部，大文件自动截断）",
        destructive: false,
        timeout_ms: 10_000,
        args: GetLastLogArgs,
        handler: crate::tools::get_last_log::handle
    }
}
