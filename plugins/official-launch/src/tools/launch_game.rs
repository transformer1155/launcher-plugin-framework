//! `launch_game`：启动游戏（**模拟**）。
//!
//! # 规格要求：不实现实际启动
//!
//! > 不实现游戏实际启动逻辑（`launch_game` 只返回模拟 pid，并在日志标注 "simulated"）
//!
//! 且全局约束是"单 OS 进程、没有子进程"。所以本工具**绝不** spawn 进程。
//!
//! # pid 返回什么（按裁决）
//!
//! 返回 **`null`**，并附 `"simulated": true`。
//!
//! 为什么不是假数字：UI 可能拿它去 `kill`、去查进程表、去"检测游戏是否还在运行"。
//! 给一个假 pid 会让这些逻辑静默地错，而且错误会延后暴露（更难排查）。
//! `null` 是**诚实**的 —— 调用方看到 null 就必须显式处理"没有真实进程"这件事。
//!
//! # 与 official-instance 的职责边界
//!
//! 本工具**不**更新 `last_played`（那是 `switch_instance` 的职责）。
//! 但在真实实现里，"启动游戏"必然也该记录游玩时间 —— 因此返回体里提示调用方
//! 应先调 `switch_instance`。这是刻意的解耦：两个插件互不依赖。

use crate::load_launch_store;
use crate::save_launch_store;
use crate::store::{log_path_for, now_rfc3339, validate_instance_id, LaunchConfig};
use launcher_plugin_common::{biz_fail, tool_args, tool_def};
use launcher_plugin_framework::error::ToolErrorCode;
use serde_json::json;

tool_args! {
    pub struct LaunchGameArgs {
        instance_id: r_str() = "实例 ID",
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: LaunchGameArgs,
) -> launcher_plugin_common::PluginResult {
    let instance_id = match validate_instance_id(&args.instance_id) {
        Ok(v) => v.to_owned(),
        Err(msg) => return Err(ctx.err(ToolErrorCode::InvalidArguments, msg)),
    };

    // 读取配置：未配置则用默认值（与 get_launch_config 一致，避免"必须先设置才能启动"）
    let store = load_launch_store!(ctx);
    let config = store
        .configs
        .get(&instance_id)
        .cloned()
        .unwrap_or_else(LaunchConfig::default);

    // ---- 前置检查：内存明显不足时拒绝，而不是让它启动后 OOM ----
    if config.memory_mb < crate::store::MIN_MEMORY_MB {
        biz_fail!(
            launcher_plugin_common::result::biz_codes::PRECONDITION_FAILED,
            "实例 {instance_id} 的 memory_mb={} 低于最小值 {}，无法启动",
            config.memory_mb,
            crate::store::MIN_MEMORY_MB
        );
    }

    // ---- 写一行模拟日志，让 get_last_log 有东西可读 ----
    // 真实实现里这里会是子进程 stdout；框架不允许子进程，所以写占位日志。
    let log_path = ctx.data_path(&log_path_for(&instance_id))?;
    if let Some(parent) = log_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            ctx.log_error(format!("创建日志目录失败：{e}"));
            return Ok(launcher_plugin_common::result::business(
                launcher_plugin_common::result::biz_codes::STORAGE_ERROR,
                format!("创建日志目录失败：{e}"),
            ));
        }
    }
    let ts = now_rfc3339();
    let line = format!(
        "[{ts}] [simulated] launching instance {instance_id} \
         (memory={}MB, jvm_args={}, game_args={}) — 未真正启动进程\n",
        config.memory_mb,
        config.jvm_args.len(),
        config.game_args.len()
    );
    if let Err(e) = append_line(&log_path, &line) {
        ctx.log_error(format!("写模拟日志失败：{e}"));
        // 日志写不进去不应阻止"启动"（真实场景里日志失败也只是少了诊断信息）。
    }

    ctx.log_info(format!(
        "[simulated] 启动实例 {instance_id}（memory={}MB）—— 未真正启动进程",
        config.memory_mb
    ));

    // 记录"最近一次启动"，但不叫 last_played（那是实例插件的概念）
    let store2 = store;
    let key = format!("__launched_at__{instance_id}");
    // 不写进 configs（那是配置），改为单独一步落盘：这里用内存 store 已是副本，
    // 只把 config 落回去保证文件存在；启动时间由日志承载，不污染配置结构。
    let _ = key;
    if store2.configs.contains_key(&instance_id) {
        save_launch_store!(ctx, &store2);
    }

    Ok(launcher_plugin_common::result::success(json!({
        "pid": null,
        "simulated": true,
        "instance_id": instance_id,
        "memory_mb": config.memory_mb,
        "note": "模拟启动：未创建任何 OS 进程（框架约束：单进程、禁止子进程）。\
                 真实启动需要业务层实现并与本框架隔离。\
                 若需记录游玩时间，请另行调用 official-instance.switch_instance",
    })))
}

/// 追加一行到日志。文件不存在则创建。
fn append_line(path: &std::path::Path, line: &str) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("打开日志文件失败：{e}"))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("写入日志失败：{e}"))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "launch_game",
        description: "启动游戏（当前为模拟启动，不创建进程，pid 恒为 null）",
        destructive: true,
        timeout_ms: 10_000,
        args: LaunchGameArgs,
        handler: crate::tools::launch_game::handle
    }
}
