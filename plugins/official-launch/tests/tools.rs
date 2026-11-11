//! `official-launch` 工具测试（直接调 handler，不启动框架）。

use std::sync::Arc;

use launcher_plugin_common::{CapturingLogger, LogLevel, PluginContext};
use launcher_plugin_framework::types::ToolResult;
use official_launch::store::{LaunchConfig, LaunchIndex, LaunchStore};
use official_launch::tools::set_launch_config::{ConfigInput, SetLaunchConfigFull};
use official_launch::tools::{get_last_log, get_launch_config, launch_game, set_launch_config};
use serde_json::json;

fn ctx(tag: &str) -> (PluginContext, Arc<CapturingLogger>) {
    // 用共享的临时目录（进程退出时统一清理，避免遗留垃圾）。
    let dir = launcher_plugin_common::test_support::temp_path(tag);
    let logger = Arc::new(CapturingLogger::default());
    let c = PluginContext::new("com.example.official-launch", dir, logger.clone());
    (c, logger)
}

fn as_result(r: launcher_plugin_common::PluginResult) -> ToolResult {
    launcher_plugin_common::result::into_framework_result(r)
}

fn code_of(r: &ToolResult) -> Option<String> {
    r.structured_content
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

fn get_args(id: &str) -> get_launch_config::GetLaunchConfigArgs {
    get_launch_config::GetLaunchConfigArgs {
        instance_id: id.to_owned(),
    }
}

fn set_args(id: &str, jvm: &[&str], game: &[&str], mem: u32) -> SetLaunchConfigFull {
    SetLaunchConfigFull {
        instance_id: id.to_owned(),
        config: ConfigInput {
            jvm_args: jvm.iter().map(|s| s.to_string()).collect(),
            game_args: game.iter().map(|s| s.to_string()).collect(),
            memory_mb: mem,
        },
    }
}

fn log_args(id: &str, lines: Option<u64>) -> get_last_log::GetLastLogArgs {
    get_last_log::GetLastLogArgs {
        instance_id: id.to_owned(),
        lines,
    }
}

// ===========================================================================
// get_launch_config
// ===========================================================================

#[tokio::test]
async fn get_returns_default_when_unset() {
    let (c, _) = ctx("get-default");
    let r = as_result(get_launch_config::handle(c.clone(), get_args("inst-1")).await);
    assert!(!r.is_error, "未配置时应返回默认值而不是报错");
    let sc = r.structured_content.unwrap();
    assert_eq!(sc["is_default"], true);
    // 默认值应当合理
    assert_eq!(sc["memory_mb"], 4096);
    assert!(!sc["jvm_args"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn get_returns_saved_config() {
    let (c, _) = ctx("get-saved");
    let _ = as_result(
        set_launch_config::handle(
            c.clone(),
            set_args("inst-1", &["-Xmx4G"], &["--fullscreen"], 4096),
        )
        .await,
    );
    let r = as_result(get_launch_config::handle(c.clone(), get_args("inst-1")).await);
    let sc = r.structured_content.unwrap();
    assert_eq!(sc["is_default"], false, "已配置时不应标默认");
    assert_eq!(sc["memory_mb"], 4096);
    assert_eq!(sc["game_args"][0], "--fullscreen");
}

#[tokio::test]
async fn get_rejects_bad_instance_id() {
    let (c, _) = ctx("get-bad-id");
    let r = as_result(get_launch_config::handle(c.clone(), get_args("../x")).await);
    assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"));
}

// ===========================================================================
// set_launch_config
// ===========================================================================

#[tokio::test]
async fn set_persists_config() {
    let (c, logger) = ctx("set-ok");
    let r = as_result(
        set_launch_config::handle(
            c.clone(),
            set_args(
                "inst-1",
                &["-XX:+UseG1GC", "-Xms1G"],
                &["--fullscreen"],
                6144,
            ),
        )
        .await,
    );
    assert!(!r.is_error);
    let sc = r.structured_content.unwrap();
    assert_eq!(sc["ok"], true);
    assert_eq!(sc["changed"], true);

    // 落盘验证
    let store = LaunchIndex::new(c.data_dir()).load().unwrap();
    let cfg = store.configs.get("inst-1").unwrap();
    assert_eq!(cfg.memory_mb, 6144);
    assert_eq!(cfg.jvm_args, vec!["-XX:+UseG1GC", "-Xms1G"]);
    assert!(logger.saw(LogLevel::Info, "已更新"));
}

#[tokio::test]
async fn set_unchanged_reports_changed_false() {
    // 幂等：相同配置重复提交不应反复写盘。
    let (c, logger) = ctx("set-idempotent");
    let args = || set_args("inst-1", &["-Xmx4G"], &[], 4096);
    let first = as_result(set_launch_config::handle(c.clone(), args()).await);
    assert_eq!(first.structured_content.unwrap()["changed"], true);

    let second = as_result(set_launch_config::handle(c.clone(), args()).await);
    assert!(!second.is_error);
    assert_eq!(
        second.structured_content.unwrap()["changed"],
        false,
        "相同配置应报告 changed=false"
    );
    assert!(logger.saw(LogLevel::Info, "未变化"));
}

#[tokio::test]
async fn set_rejects_memory_out_of_range() {
    let (c, _) = ctx("set-mem");
    for bad in [0u32, 511, 32769, 65536] {
        let r = as_result(
            set_launch_config::handle(c.clone(), set_args("inst-1", &[], &[], bad)).await,
        );
        assert!(r.is_error, "memory_mb={bad} 应被拒");
        assert_eq!(
            code_of(&r).as_deref(),
            Some("InvalidArguments"),
            "memory_mb={bad}"
        );
    }
    // 边界合法
    for ok in [512u32, 32768] {
        let r =
            as_result(set_launch_config::handle(c.clone(), set_args("inst-1", &[], &[], ok)).await);
        assert!(!r.is_error, "memory_mb={ok} 应当合法");
    }
}

#[tokio::test]
async fn set_rejects_bad_args() {
    let (c, _) = ctx("set-badargs");
    // 空参数项
    let r =
        as_result(set_launch_config::handle(c.clone(), set_args("inst-1", &[""], &[], 4096)).await);
    assert_eq!(
        code_of(&r).as_deref(),
        Some("InvalidArguments"),
        "空 JVM 参数应被拒"
    );

    // 换行注入
    let r = as_result(
        set_launch_config::handle(c.clone(), set_args("inst-1", &["a\nb"], &[], 4096)).await,
    );
    assert_eq!(
        code_of(&r).as_deref(),
        Some("InvalidArguments"),
        "换行应被拒"
    );

    // 超长
    let huge = "x".repeat(1025);
    let r = as_result(
        set_launch_config::handle(c.clone(), set_args("inst-1", &[&huge], &[], 4096)).await,
    );
    assert_eq!(
        code_of(&r).as_deref(),
        Some("InvalidArguments"),
        "超长参数应被拒"
    );
}

#[tokio::test]
async fn set_warns_on_low_memory_but_succeeds() {
    // 2048 以下给警告，但仍然允许保存（用户可能就要玩极轻量整合）。
    let (c, logger) = ctx("set-lowmem");
    let r =
        as_result(set_launch_config::handle(c.clone(), set_args("inst-1", &[], &[], 1024)).await);
    assert!(!r.is_error, "低内存应警告而非拒绝");
    assert!(logger.saw(LogLevel::Warn, "偏低"));
}

// ===========================================================================
// launch_game
// ===========================================================================

#[tokio::test]
async fn launch_spawns_real_java_process() {
    // 真实 spawn：若环境里有 `java`，应返回真实 PID（数字）且 simulated=false；
    // 若没有 `java`，应返回业务错误 launch_failed。两种情况都说明"不再是模拟启动"。
    let java_available = std::process::Command::new("java")
        .arg("-version")
        .output()
        .is_ok();

    let (c, logger) = ctx("launch-ok");
    let r = as_result(
        launch_game::handle(
            c.clone(),
            launch_game::LaunchGameArgs {
                instance_id: "inst-1".into(),
            },
        )
        .await,
    );

    if java_available {
        assert!(!r.is_error, "有 java 时应启动成功：{r:?}");
        let sc = r.structured_content.unwrap();
        assert!(
            sc["pid"].is_u64(),
            "pid 必须是真实数字（非 null）：{:?}",
            sc["pid"]
        );
        assert_eq!(sc["simulated"], false, "必须标注非模拟");
        assert!(logger.saw(LogLevel::Info, "[launch]"), "日志应标真实启动");
        // 进程已退出（这里是 -version，立即 exit 0），状态可读到退出码。
        let status = official_launch::store::launch_status();
        assert!(status.is_some(), "应能读到真实退出码");
    } else {
        assert!(r.is_error, "无 java 时应返回错误");
        assert_eq!(
            code_of(&r).as_deref(),
            Some("launch_failed"),
            "spawn 失败应是企业务错误 launch_failed"
        );
    }
}

#[tokio::test]
async fn launch_writes_real_log() {
    let (c, _) = ctx("launch-log");
    let _ = as_result(
        launch_game::handle(
            c.clone(),
            launch_game::LaunchGameArgs {
                instance_id: "inst-1".into(),
            },
        )
        .await,
    );

    let log = c.data_dir().join("instances/inst-1/logs/latest.log");
    if !log.exists() {
        // 环境无 java，spawn 失败，不会写日志——这是预期的分支。
        eprintln!("跳过日志断言：当前环境没有 java");
        return;
    }
    let content = std::fs::read_to_string(&log).unwrap();
    assert!(content.contains("inst-1"), "日志必须包含实例 ID");
    assert!(content.contains("[launch]"), "日志必须标真实启动");
}

#[tokio::test]
async fn launch_uses_saved_memory() {
    // 依赖真实 java：环境无 java 时跳过（launch 会因 spawn 失败而返回错误）。
    if std::process::Command::new("java")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("跳过 launch_uses_saved_memory：当前环境没有 java");
        return;
    }
    let (c, _) = ctx("launch-mem");
    let _ =
        as_result(set_launch_config::handle(c.clone(), set_args("inst-1", &[], &[], 8192)).await);
    let r = as_result(
        launch_game::handle(
            c.clone(),
            launch_game::LaunchGameArgs {
                instance_id: "inst-1".into(),
            },
        )
        .await,
    );
    assert!(!r.is_error, "启动应成功：{r:?}");
    assert_eq!(r.structured_content.unwrap()["memory_mb"], 8192);
}

#[tokio::test]
async fn launch_rejects_insufficient_memory() {
    // 构造一个内存低于最小值的配置（绕过 set 的校验，直接写盘）
    let (c, _) = ctx("launch-lowmem");
    let _ = c.ensure_data_dir();
    let mut store = LaunchStore::default();
    store.configs.insert(
        "inst-1".into(),
        LaunchConfig {
            jvm_args: vec![],
            game_args: vec![],
            memory_mb: 256, // 低于 MIN_MEMORY_MB
            ..Default::default()
        },
    );
    LaunchIndex::new(c.data_dir()).save(&store).unwrap();

    let r = as_result(
        launch_game::handle(
            c.clone(),
            launch_game::LaunchGameArgs {
                instance_id: "inst-1".into(),
            },
        )
        .await,
    );
    assert!(r.is_error);
    assert_eq!(
        code_of(&r).as_deref(),
        Some("precondition_failed"),
        "内存不足是前置条件失败（业务错误）"
    );
}

#[tokio::test]
async fn launch_rejects_bad_instance_id() {
    let (c, _) = ctx("launch-badid");
    let r = as_result(
        launch_game::handle(
            c.clone(),
            launch_game::LaunchGameArgs {
                instance_id: "a/b".into(),
            },
        )
        .await,
    );
    assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"));
}

// ===========================================================================
// get_last_log
// ===========================================================================

#[tokio::test]
async fn get_log_not_found_when_never_launched() {
    let (c, _) = ctx("log-missing");
    let r = as_result(
        get_last_log::handle(
            c.clone(),
            get_last_log::GetLastLogArgs {
                instance_id: "inst-1".into(),
                lines: None,
            },
        )
        .await,
    );
    assert!(r.is_error);
    assert_eq!(
        code_of(&r).as_deref(),
        Some("not_found"),
        "没有日志应是业务错误，不是参数错误"
    );
}

#[tokio::test]
async fn get_log_returns_tail_after_launch() {
    // 依赖真实 java：无 java 时 launch 不会写日志，跳过。
    if std::process::Command::new("java")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("跳过 get_log_returns_tail_after_launch：当前环境没有 java");
        return;
    }
    let (c, _) = ctx("log-tail");
    // 启动两次，产生两行日志
    for _ in 0..2 {
        let _ = as_result(
            launch_game::handle(
                c.clone(),
                launch_game::LaunchGameArgs {
                    instance_id: "inst-1".into(),
                },
            )
            .await,
        );
    }
    let r = as_result(get_last_log::handle(c.clone(), log_args("inst-1", Some(2))).await);
    assert!(!r.is_error, "日志应可读：{r:?}");
    let sc = r.structured_content.unwrap();
    let content = sc["content"].as_str().unwrap();
    assert_eq!(content.split('\n').count(), 2, "应当正好 2 行");
    assert!(content.contains("[launch]"), "日志必须标真实启动");
    assert_eq!(sc["truncated"], false);
}

#[tokio::test]
async fn get_log_respects_lines_limit() {
    let (c, _) = ctx("log-limit");
    // 用足够多的行，保证读到的是尾部 5 行（且文件足够大）
    let dir = c.data_dir().join("instances/inst-1/logs");
    std::fs::create_dir_all(&dir).unwrap();
    let content: String = (1..=2000)
        .map(|i| format!("a-reasonably-long-log-line-{i:04}\n"))
        .collect();
    std::fs::write(dir.join("latest.log"), content).unwrap();

    let r = as_result(get_last_log::handle(c.clone(), log_args("inst-1", Some(5))).await);
    let sc = r.structured_content.unwrap();
    let got: Vec<&str> = sc["content"].as_str().unwrap().split('\n').collect();
    assert_eq!(got.len(), 5, "必须正好 5 行");
    assert_eq!(
        got[4], "a-reasonably-long-log-line-2000",
        "最后一行必须是文件最后一行"
    );
    assert_eq!(sc["reached_start"], false, "大文件只读尾部，未到开头");
}

#[tokio::test]
async fn get_log_rejects_lines_out_of_range() {
    let (c, _) = ctx("log-range");
    let dir = c.data_dir().join("instances/inst-1/logs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("latest.log"), "x\n").unwrap();

    for bad in [Some(0u64), Some(2001)] {
        let r = as_result(get_last_log::handle(c.clone(), log_args("inst-1", bad)).await);
        assert!(r.is_error, "lines={bad:?} 应被拒");
        assert_eq!(
            code_of(&r).as_deref(),
            Some("InvalidArguments"),
            "lines={bad:?}"
        );
    }
}

// ===========================================================================
// 插件级
// ===========================================================================

#[test]
fn tool_set_matches_manifest() {
    let set = official_launch::tools();
    set.validate().expect("工具集合应自洽");
    let manifest =
        launcher_plugin_common::PluginManifestV1::parse(include_bytes!("../plugin.json")).unwrap();
    manifest
        .check_tools(&set.manifest_tools())
        .expect("plugin.json 的 tools 必须与代码一致");
    assert_eq!(manifest.tools.len(), 5);
}

#[test]
fn destructive_flags() {
    let set = official_launch::tools();
    let get = |n: &str| set.tools.iter().find(|t| t.short_name == n).unwrap();
    assert!(
        get("set_launch_config").destructive,
        "覆盖配置应标 destructive"
    );
    assert!(get("launch_game").destructive, "启动游戏应标 destructive");
    assert!(!get("get_launch_config").destructive, "读配置是只读的");
    assert!(!get("get_last_log").destructive, "读日志是只读的");
}

#[test]
fn all_schemas_valid_subset() {
    let set = official_launch::tools();
    for t in &set.tools {
        launcher_plugin_framework::json::validate_schema(&t.input_schema)
            .unwrap_or_else(|e| panic!("{} schema 非法：{e}", t.short_name));
    }
}

#[test]
fn set_config_schema_declares_nested_object() {
    let set = official_launch::tools();
    let d = set
        .tools
        .iter()
        .find(|t| t.short_name == "set_launch_config")
        .unwrap();
    let cfg = &d.input_schema["properties"]["config"];
    assert_eq!(cfg["type"], "object");
    assert_eq!(cfg["required"], json!(["memory_mb"]));
    assert_eq!(cfg["properties"]["memory_mb"]["minimum"], 512.0);
    assert_eq!(cfg["properties"]["memory_mb"]["maximum"], 32768.0);
    assert_eq!(cfg["properties"]["jvm_args"]["type"], "array");
    // 顶层必填
    assert_eq!(d.input_schema["required"], json!(["instance_id", "config"]));
}

#[test]
fn set_config_schema_matches_deserialize() {
    // schema 与 from_value 必须同源：合法输入应当解析成功，非法应当解析失败。
    use launcher_plugin_common::schema::ToolArgs;

    let ok = json!({
        "instance_id": "inst-1",
        "config": {"jvm_args": ["-Xmx4G"], "game_args": [], "memory_mb": 4096}
    });
    let parsed = SetLaunchConfigFull::from_value(&ok).expect("合法输入应解析成功");
    assert_eq!(parsed.instance_id, "inst-1");
    assert_eq!(parsed.config.memory_mb, 4096);

    // 缺 memory_mb
    let bad = json!({"instance_id": "inst-1", "config": {"jvm_args": []}});
    assert!(
        SetLaunchConfigFull::from_value(&bad).is_err(),
        "缺必填字段应当解析失败（与 schema 一致）"
    );

    // 额外字段（additionalProperties: false）
    let extra = json!({
        "instance_id": "inst-1",
        "config": {"memory_mb": 4096, "unknown_field": 1}
    });
    assert!(
        SetLaunchConfigFull::from_value(&extra).is_err(),
        "额外字段应被 schema 拒绝"
    );

    // memory_mb 越界（schema 有 minimum/maximum）
    let out_of_range = json!({
        "instance_id": "inst-1",
        "config": {"memory_mb": 99999}
    });
    assert!(
        SetLaunchConfigFull::from_value(&out_of_range).is_err(),
        "越界内存应被 schema 拒绝"
    );
}

#[test]
fn launch_game_spawns_real_process() {
    // 结构性断言：源码里**必须**出现进程创建 API。
    // 这是"真实 spawn Java 进程"这一里程碑的静态检查（取代早期的"禁止 spawn"约束）。
    let src = include_str!("../src/tools/launch_game.rs");
    for required in ["std::process::Command", "Command::new", ".spawn()"] {
        assert!(
            src.contains(required),
            "launch_game 必须包含进程创建 API：`{required}`"
        );
    }
}

#[test]
fn process_spawn_allowed_only_in_launch_path() {
    // 只有真正负责启动的 launch_game.rs / store.rs（持有子进程槽位）允许 spawn 进程；
    // 其余工具不得创建 OS 进程。
    for (name, src) in [
        ("lib.rs", include_str!("../src/lib.rs")),
        (
            "get_launch_config.rs",
            include_str!("../src/tools/get_launch_config.rs"),
        ),
        (
            "set_launch_config.rs",
            include_str!("../src/tools/set_launch_config.rs"),
        ),
        (
            "get_last_log.rs",
            include_str!("../src/tools/get_last_log.rs"),
        ),
    ] {
        assert!(
            !src.contains("std::process::Command") && !src.contains("tokio::process"),
            "{name} 不得创建 OS 进程"
        );
    }
    // 启动相关文件应当出现进程 API。
    assert!(
        include_str!("../src/tools/launch_game.rs").contains("std::process::Command"),
        "launch_game.rs 应当 spawn 进程"
    );
    assert!(
        include_str!("../src/store.rs").contains("std::process::Child"),
        "store.rs 应当持有子进程槽位"
    );
}
