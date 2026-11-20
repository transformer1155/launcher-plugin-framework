//! `set_launch_config`：设置实例的启动参数。
//!
//! # 为什么 destructive
//!
//! 它会覆盖用户已有的启动参数。虽然可再改回来，但"覆盖配置"在 UI 语义上需要提醒
//! （用户可能手动调过 JVM 参数调优，一次误点就丢了）。
//!
//! # 全量覆盖而非增量合并
//!
//! `config` 是一个完整的对象（三个字段都在）。不做"只传部分字段就更新部分"的合并语义，
//! 因为合并会让"我想清空 game_args"变得无法表达（空数组与"没传"无法区分）。
//! UI 要做增量修改时应当先 `get_launch_config` 再改后整体提交。
//!
//! `java_path` 是**可选**字段：不传表示保留插件侧已有的 Java 路径（兼容老调用方），
//! 传入则覆盖为 UI「Java 管理」选中的运行时；传空串表示清除并回退到 PATH 上的 `java`。
//! 这样 UI 的选择能真正落到 `launch.json`，`launch_game` 才会用它 spawn 进程。

use crate::load_launch_store;
use crate::save_launch_store;
use crate::store::{validate_args, validate_instance_id, validate_memory_mb, LaunchConfig};
use launcher_plugin_common::tool_args;
use launcher_plugin_framework::error::ToolErrorCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// 入参里的 `config` 子对象。
///
/// 注意：这个结构体**不**用 `tool_args!` 宏，因为宏只支持平铺的标量字段，
/// 而这里需要一个嵌套对象。schema 仍在 `def()` 里手工声明 —— 但要与 `handle`
/// 里的解析**同源**：两者都通过本结构体的 `Deserialize` 行为。
/// 测试 `set_config_schema_matches_deserialize` 断言二者一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigInput {
    #[serde(default)]
    pub jvm_args: Vec<String>,
    #[serde(default)]
    pub game_args: Vec<String>,
    pub memory_mb: u32,
    /// 可选：Java 可执行文件绝对路径。`None` = 不改，`Some("")` = 清除（回退 PATH）。
    #[serde(default)]
    pub java_path: Option<String>,
}

tool_args! {
    pub struct SetLaunchConfigArgs {
        instance_id: r_str() = "实例 ID",
    }
}

/// 完整的入参（`instance_id` + `config` 嵌套对象）。
///
/// 由于宏无法表达嵌套对象，这里手写 `ToolArgs`。schema 与解析都在本类型内，
/// 因此仍然只有一处真相。
#[derive(Debug, Clone)]
pub struct SetLaunchConfigFull {
    pub instance_id: String,
    pub config: ConfigInput,
}

impl launcher_plugin_common::schema::ToolArgs for SetLaunchConfigFull {
    fn args_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "instance_id": {
                    "type": "string",
                    "description": "实例 ID"
                },
                "config": {
                    "type": "object",
                    "description": "完整启动配置（全量覆盖）",
                    "properties": {
                        "jvm_args": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "JVM 参数列表"
                        },
                        "game_args": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "游戏参数列表"
                        },
                        "memory_mb": {
                            "type": "integer",
                            "minimum": 512,
                            "maximum": 32768,
                            "description": "最大堆内存（MB）"
                        },
                        "java_path": {
                            "type": "string",
                            "description": "可选：Java 可执行文件绝对路径；不传保留原值，传空串清除并回退 PATH 上的 java"
                        }
                    },
                    "required": ["memory_mb"],
                    "additionalProperties": false
                }
            },
            "required": ["instance_id", "config"],
            "additionalProperties": false
        })
    }

    fn from_value(v: &serde_json::Value) -> Result<Self, String> {
        // 先用同一份 schema 校验（保证与 schema 不漂移）
        let schema = <Self as launcher_plugin_common::schema::ToolArgs>::args_schema();
        launcher_plugin_common::json::validate(v, &schema).map_err(|e| e.to_string())?;

        let instance_id = v
            .get("instance_id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "缺少 instance_id".to_owned())?
            .to_owned();

        let config_val = v.get("config").ok_or_else(|| "缺少 config".to_owned())?;
        let config: ConfigInput = serde_json::from_value(config_val.clone())
            .map_err(|e| format!("config 解析失败：{e}"))?;

        Ok(Self {
            instance_id,
            config,
        })
    }
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: SetLaunchConfigFull,
) -> launcher_plugin_common::PluginResult {
    let instance_id = match validate_instance_id(&args.instance_id) {
        Ok(v) => v.to_owned(),
        Err(msg) => return Err(ctx.err(ToolErrorCode::InvalidArguments, msg)),
    };

    // ---- 逐项校验（错误码一律 InvalidArguments：都是"参数不合规"）----
    if let Err(msg) = validate_args(&args.config.jvm_args, "jvm_args") {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, msg));
    }
    if let Err(msg) = validate_args(&args.config.game_args, "game_args") {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, msg));
    }
    if let Err(msg) = validate_memory_mb(args.config.memory_mb) {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, msg));
    }

    // ---- 与既有配置对比：没变就不写盘（避免无意义的文件抖动）----
    // 只覆盖用户可见的三项（jvm/game/memory）；`entry`/`mc_args`/`natives_dir`
    // 由 `install_minecraft` 写入，必须保留——否则每次启动前调本工具都会把
    // 真实的 Minecraft 启动入口重置回探测式启动。
    let mut store = load_launch_store!(ctx);
    let existing = store.configs.get(&instance_id).cloned();

    // `java_path` 由 UI「Java 管理」传入：不传保留原值，传空串清除（回退 PATH），
    // 传路径则校验文件存在后覆盖。校验放在这里而不是 launch_game，
    // 是为了让用户点「选择」时就能得到明确错误，而不是启动时静默回退到旧 Java。
    let java_path = match args.config.java_path.as_deref() {
        None => existing.as_ref().and_then(|e| e.java_path.clone()),
        Some("") => None,
        Some(p) if p == "java" => Some(p.to_owned()),
        Some(p) => {
            if !std::path::Path::new(p).is_file() {
                return Err(ctx.err(
                    ToolErrorCode::InvalidArguments,
                    format!("java_path 指向的文件不存在：{p}"),
                ));
            }
            Some(p.to_owned())
        }
    };

    let config = LaunchConfig {
        jvm_args: args.config.jvm_args.clone(),
        game_args: args.config.game_args.clone(),
        memory_mb: args.config.memory_mb,
        java_path,
        mc_args: existing
            .as_ref()
            .map(|e| e.mc_args.clone())
            .unwrap_or_default(),
        entry: existing.as_ref().map(|e| e.entry.clone()).unwrap_or_default(),
        natives_dir: existing.as_ref().and_then(|e| e.natives_dir.clone()),
    };

    if let Some(existing) = store.configs.get(&instance_id) {
        if existing == &config {
            ctx.log_info(format!("实例 {instance_id} 的启动配置未变化，跳过写入"));
            return Ok(launcher_plugin_common::result::success(json!({
                "ok": true,
                "changed": false,
                "config": {
                    "jvm_args": config.jvm_args,
                    "game_args": config.game_args,
                    "memory_mb": config.memory_mb,
                },
            })));
        }
    }

    // ---- 合理性提醒（不阻断）：在沙箱里超上限的 Xmx 只会崩溃得更晚 ----
    let xmx_upper = args.config.memory_mb;
    if xmx_upper < 2048 {
        ctx.log_warn(format!(
            "实例 {instance_id} 的 memory_mb={xmx_upper} 偏低，现代 Minecraft 可能无法启动"
        ));
    }

    store.configs.insert(instance_id.clone(), config.clone());
    save_launch_store!(ctx, &store);

    ctx.log_info(format!(
        "已更新实例 {instance_id} 的启动配置（memory={}MB, jvm={} 项, game={} 项）",
        config.memory_mb,
        config.jvm_args.len(),
        config.game_args.len()
    ));

    Ok(launcher_plugin_common::result::success(json!({
        "ok": true,
        "changed": true,
        "config": {
            "jvm_args": config.jvm_args,
            "game_args": config.game_args,
            "memory_mb": config.memory_mb,
        },
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    launcher_plugin_common::ToolDef {
        short_name: "set_launch_config",
        description: "设置实例的启动参数（全量覆盖）",
        input_schema:
            <SetLaunchConfigFull as launcher_plugin_common::schema::ToolArgs>::args_schema(),
        destructive: true,
        timeout_ms: 5_000,
        handler: launcher_plugin_common::erase_handler::<SetLaunchConfigFull, _, _>(
            handle_with_full,
        ),
    }
}

/// 供 `erase_handler` 使用的包装（签名固定为 `(ctx, args)`）。
async fn handle_with_full(
    ctx: launcher_plugin_common::PluginContext,
    args: SetLaunchConfigFull,
) -> launcher_plugin_common::PluginResult {
    handle(ctx, args).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_plugin_common::schema::ToolArgs;

    /// UI 传入 `java_path` 时必须能被 schema 校验 + 反序列化同时接受，
    /// 否则「Java 管理」的选择会在启动前被 set_launch_config 直接拒绝。
    #[test]
    fn accepts_optional_java_path() {
        let v = json!({
            "instance_id": "inst-1",
            "config": {
                "jvm_args": ["-Xmx4G"],
                "game_args": [],
                "memory_mb": 4096,
                "java_path": r"C:\Program Files\Java\jdk-21\bin\java.exe"
            }
        });
        let parsed = SetLaunchConfigFull::from_value(&v).expect("带 java_path 的配置应能解析");
        assert_eq!(
            parsed.config.java_path.as_deref(),
            Some(r"C:\Program Files\Java\jdk-21\bin\java.exe")
        );
    }

    /// 不传 `java_path` 的旧调用方（既有测试 / 外部 MCP 调用）必须保持兼容。
    #[test]
    fn java_path_is_optional() {
        let v = json!({
            "instance_id": "inst-1",
            "config": { "memory_mb": 4096 }
        });
        let parsed = SetLaunchConfigFull::from_value(&v).expect("不传 java_path 应兼容旧调用");
        assert!(parsed.config.java_path.is_none());
    }
}
