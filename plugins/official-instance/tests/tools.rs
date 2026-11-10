//! `official-instance` 工具测试。
//!
//! # 测试策略：直接调 handler，不启动框架
//!
//! 规格要求"测试直接调 handler，不启动框架"。因此这里只构造 [`PluginContext`]
//! （指向临时目录、丢弃日志）然后调各工具的 `handle`。
//!
//! 这样测的是**插件自己的业务逻辑**，不受调度器/注册表影响；
//! 框架层的 ACL/超时/崩溃由框架自己的测试覆盖。
//!
//! # 每个工具覆盖维度
//!
//! | 维度 | 说明 |
//! |---|---|
//! | 正常路径 | 成功并落盘 |
//! | 参数非法 | `InvalidArguments`（框架码） |
//! | 状态/业务错误 | 插件业务码（`not_found` / `already_exists` / …） |
//! | 边界值 | 名字长度、数量上限、空列表 |
//! | 失败路径 | 存储损坏 |

use std::sync::Arc;

use launcher_plugin_common::{CapturingLogger, PluginContext};
use launcher_plugin_framework::types::ToolResult;
use official_instance::store::{Instance, InstanceStore, Loader, Store};
use official_instance::tools::{create_instance, delete_instance, list_instances, switch_instance};

/// 造一个临时目录 + 带捕获日志的上下文。
fn ctx(tag: &str) -> (PluginContext, Arc<CapturingLogger>) {
    // 用共享的临时目录（进程退出时统一清理，避免遗留垃圾）。
    let dir = launcher_plugin_common::test_support::temp_path(tag);
    let logger = Arc::new(CapturingLogger::default());
    let c = PluginContext::new("com.example.official-instance", dir, logger.clone());
    (c, logger)
}

/// 从框架形状的结果里取 `structuredContent.code`（业务码与框架码都走这里）。
fn code_of(r: &ToolResult) -> Option<String> {
    r.structured_content
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

fn as_result(r: launcher_plugin_common::PluginResult) -> ToolResult {
    launcher_plugin_common::result::into_framework_result(r)
}

fn create_args(name: &str, ver: &str, loader: &str) -> create_instance::CreateInstanceArgs {
    create_instance::CreateInstanceArgs {
        name: name.to_owned(),
        game_version: ver.to_owned(),
        loader: loader.to_owned(),
    }
}

// ===========================================================================
// list_instances
// ===========================================================================

#[tokio::test]
async fn list_empty_when_no_store() {
    let (c, _) = ctx("list-empty");
    // 目录都不存在：只读工具必须仍然成功，返回空列表。
    assert!(!c.data_dir().exists(), "前置：目录尚未创建");
    let r =
        as_result(list_instances::handle(c.clone(), list_instances::ListInstancesArgs {}).await);
    assert!(!r.is_error, "首次运行应返回空列表而不是报错");
    assert_eq!(
        r.structured_content.unwrap()["instances"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn list_sorts_by_last_played_then_name() {
    let (c, _) = ctx("list-sort");
    let store = InstanceStore::new(c.data_dir());
    store
        .save(&Store {
            schema_version: 1,
            instances: vec![
                Instance {
                    id: "inst-1".into(),
                    name: "Zeta".into(),
                    game_version: "1.20.1".into(),
                    loader: Loader::Vanilla,
                    last_played: None,
                },
                Instance {
                    id: "inst-2".into(),
                    name: "Alpha".into(),
                    game_version: "1.20.1".into(),
                    loader: Loader::Fabric,
                    last_played: None,
                },
                Instance {
                    id: "inst-3".into(),
                    name: "Played".into(),
                    game_version: "1.21".into(),
                    loader: Loader::Forge,
                    last_played: Some("2026-11-07T10:00:00Z".into()),
                },
                Instance {
                    id: "inst-4".into(),
                    name: "PlayedEarlier".into(),
                    game_version: "1.19.2".into(),
                    loader: Loader::Vanilla,
                    last_played: Some("2026-11-06T10:00:00Z".into()),
                },
            ],
        })
        .unwrap();

    let r =
        as_result(list_instances::handle(c.clone(), list_instances::ListInstancesArgs {}).await);
    let arr = r.structured_content.unwrap()["instances"]
        .as_array()
        .unwrap()
        .clone();
    let names: Vec<&str> = arr.iter().map(|v| v["name"].as_str().unwrap()).collect();
    // 最近玩的在前 → 都没玩过的按名字
    assert_eq!(names, vec!["Played", "PlayedEarlier", "Alpha", "Zeta"]);
}

#[tokio::test]
async fn list_reports_storage_error_on_corrupt_file() {
    let (c, logger) = ctx("list-corrupt");
    std::fs::create_dir_all(c.data_dir()).unwrap();
    std::fs::write(c.data_dir().join("instances.json"), b"{broken").unwrap();

    let r =
        as_result(list_instances::handle(c.clone(), list_instances::ListInstancesArgs {}).await);
    assert!(r.is_error);
    assert_eq!(
        code_of(&r).as_deref(),
        Some("storage_error"),
        "损坏的文件应是业务错误，不是框架错误"
    );
    assert!(logger.saw(launcher_plugin_common::LogLevel::Error, "读取实例存储失败"));
}

// ===========================================================================
// create_instance
// ===========================================================================

#[tokio::test]
async fn create_succeeds_and_persists() {
    let (c, _) = ctx("create-ok");
    let r = as_result(
        create_instance::handle(c.clone(), create_args("生存服", "1.20.1", "fabric")).await,
    );
    assert!(!r.is_error, "正常创建应成功");
    let sc = r.structured_content.unwrap();
    let id = sc["instance_id"].as_str().unwrap().to_owned();
    assert!(!id.is_empty());
    assert_eq!(sc["instance"]["loader"], "fabric");

    // 落盘验证：重新读盘应当能读到
    let store = InstanceStore::new(c.data_dir()).load().unwrap();
    assert_eq!(store.instances.len(), 1);
    assert_eq!(store.instances[0].id, id);
    assert_eq!(store.instances[0].name, "生存服");
}

#[tokio::test]
async fn create_rejects_empty_name() {
    let (c, _) = ctx("create-empty-name");
    let r = as_result(
        create_instance::handle(c.clone(), create_args("   ", "1.20.1", "vanilla")).await,
    );
    assert!(r.is_error);
    assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"));
}

#[tokio::test]
async fn create_rejects_path_separators_in_name() {
    let (c, _) = ctx("create-bad-name");
    for bad in ["a/b", "a\\b", "a:b", "..", "a\nb"] {
        let r = as_result(
            create_instance::handle(c.clone(), create_args(bad, "1.20.1", "vanilla")).await,
        );
        assert!(r.is_error, "`{bad}` 应当被拒");
        assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"), "`{bad}`");
    }
}

#[tokio::test]
async fn create_rejects_overlong_name() {
    let (c, _) = ctx("create-long-name");
    let name = "A".repeat(65);
    let r = as_result(
        create_instance::handle(c.clone(), create_args(&name, "1.20.1", "vanilla")).await,
    );
    assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"));
    // 边界：64 个字符应当通过
    let name64 = "A".repeat(64);
    let r2 = as_result(
        create_instance::handle(c.clone(), create_args(&name64, "1.20.1", "vanilla")).await,
    );
    assert!(!r2.is_error, "64 字符应当是上限内的合法值");
}

#[tokio::test]
async fn create_rejects_bad_game_version() {
    let (c, _) = ctx("create-bad-ver");
    for bad in ["", "latest", "1", "1.20.1.4", "1.x", "1..2"] {
        let r =
            as_result(create_instance::handle(c.clone(), create_args("ok", bad, "vanilla")).await);
        assert!(r.is_error, "`{bad}` 应当被拒");
        assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"), "`{bad}`");
    }
}

#[tokio::test]
async fn create_rejects_unknown_loader() {
    let (c, _) = ctx("create-bad-loader");
    let r =
        as_result(create_instance::handle(c.clone(), create_args("ok", "1.20.1", "quilt")).await);
    assert_eq!(
        code_of(&r).as_deref(),
        Some("InvalidArguments"),
        "未知 loader 必须是参数错误"
    );
}

#[tokio::test]
async fn create_rejects_duplicate_name_case_insensitively() {
    let (c, _) = ctx("create-dup");
    let r = as_result(
        create_instance::handle(c.clone(), create_args("Survival", "1.20.1", "vanilla")).await,
    );
    assert!(!r.is_error);

    // 大小写不同、首尾空白不同，仍视为重名
    for dup in ["survival", "SURVIVAL", "  Survival  "] {
        let r = as_result(
            create_instance::handle(c.clone(), create_args(dup, "1.19.2", "forge")).await,
        );
        assert!(r.is_error, "`{dup}` 应被视为重名");
        assert_eq!(
            code_of(&r).as_deref(),
            Some("already_exists"),
            "重名是业务错误，不是参数错误（`{dup}`）"
        );
    }
}

#[tokio::test]
async fn create_enforces_instance_limit() {
    let (c, _) = ctx("create-limit");
    let store = InstanceStore::new(c.data_dir());
    let instances: Vec<Instance> = (0..200)
        .map(|i| Instance {
            id: format!("inst-{i:03}"),
            name: format!("n{i}"),
            game_version: "1.20.1".into(),
            loader: Loader::Vanilla,
            last_played: None,
        })
        .collect();
    store
        .save(&Store {
            schema_version: 1,
            instances,
        })
        .unwrap();

    let r = as_result(
        create_instance::handle(c.clone(), create_args("one-more", "1.20.1", "vanilla")).await,
    );
    assert!(r.is_error);
    assert_eq!(
        code_of(&r).as_deref(),
        Some("limit_exceeded"),
        "达到上限应是业务错误"
    );
}

#[tokio::test]
async fn create_generates_monotonic_ids_after_deletion() {
    // 删掉最后一个再创建，新 ID 不能与旧 ID 撞号。
    let (c, _) = ctx("create-idseq");
    let mut ids = Vec::new();
    for i in 0..3 {
        let r = as_result(
            create_instance::handle(
                c.clone(),
                create_args(&format!("inst{i}"), "1.20.1", "vanilla"),
            )
            .await,
        );
        ids.push(
            r.structured_content.unwrap()["instance_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    // 删中间那个
    let del = as_result(
        delete_instance::handle(
            c.clone(),
            delete_instance::DeleteInstanceArgs {
                instance_id: ids[1].clone(),
                confirm: true,
            },
        )
        .await,
    );
    assert!(!del.is_error);

    let r = as_result(
        create_instance::handle(c.clone(), create_args("newone", "1.21", "fabric")).await,
    );
    let new_id = r.structured_content.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        !ids.contains(&new_id),
        "新 ID 不能与已删除的旧 ID 重复：{new_id}"
    );
}

// ===========================================================================
// switch_instance
// ===========================================================================

#[tokio::test]
async fn switch_updates_last_played() {
    let (c, _) = ctx("switch-ok");
    let created =
        as_result(create_instance::handle(c.clone(), create_args("s1", "1.20.1", "forge")).await);
    let id = created.structured_content.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let r = as_result(
        switch_instance::handle(
            c.clone(),
            switch_instance::SwitchInstanceArgs {
                instance_id: id.clone(),
            },
        )
        .await,
    );
    assert!(!r.is_error);
    let sc = r.structured_content.unwrap();
    assert_eq!(sc["ok"], true);
    let ts = sc["last_played"].as_str().unwrap().to_owned();
    assert!(ts.ends_with('Z'), "时间戳应是 RFC3339 UTC：{ts}");

    // 落盘验证
    let store = InstanceStore::new(c.data_dir()).load().unwrap();
    assert_eq!(store.instances[0].last_played.as_deref(), Some(ts.as_str()));
}

#[tokio::test]
async fn switch_unknown_id_is_business_not_found() {
    let (c, _) = ctx("switch-missing");
    let r = as_result(
        switch_instance::handle(
            c.clone(),
            switch_instance::SwitchInstanceArgs {
                instance_id: "inst-does-not-exist".into(),
            },
        )
        .await,
    );
    assert!(r.is_error);
    assert_eq!(
        code_of(&r).as_deref(),
        Some("not_found"),
        "不存在的实例是业务错误"
    );
}

#[tokio::test]
async fn switch_rejects_empty_id() {
    let (c, _) = ctx("switch-empty");
    let r = as_result(
        switch_instance::handle(
            c.clone(),
            switch_instance::SwitchInstanceArgs {
                instance_id: "   ".into(),
            },
        )
        .await,
    );
    assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"));
}

// ===========================================================================
// delete_instance
// ===========================================================================

#[tokio::test]
async fn delete_requires_confirm_true() {
    // 规格第 4 条：handler 内二次确认，不依赖调用方。
    let (c, logger) = ctx("delete-no-confirm");
    let created = as_result(
        create_instance::handle(c.clone(), create_args("doomed", "1.20.1", "vanilla")).await,
    );
    let id = created.structured_content.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let r = as_result(
        delete_instance::handle(
            c.clone(),
            delete_instance::DeleteInstanceArgs {
                instance_id: id.clone(),
                confirm: false,
            },
        )
        .await,
    );
    assert!(r.is_error, "confirm=false 必须被拒绝");
    assert_eq!(
        code_of(&r).as_deref(),
        Some("InvalidArguments"),
        "未确认应是参数错误"
    );
    assert!(logger.saw(launcher_plugin_common::LogLevel::Warn, "confirm=false"));

    // 实例必须还在（上面的拒绝不能有副作用）
    let store = InstanceStore::new(c.data_dir()).load().unwrap();
    assert_eq!(store.instances.len(), 1, "被拒绝的删除不得删除数据");
}

#[tokio::test]
async fn delete_succeeds_with_confirm() {
    let (c, _) = ctx("delete-ok");
    let created = as_result(
        create_instance::handle(c.clone(), create_args("doomed", "1.20.1", "vanilla")).await,
    );
    let id = created.structured_content.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let r = as_result(
        delete_instance::handle(
            c.clone(),
            delete_instance::DeleteInstanceArgs {
                instance_id: id.clone(),
                confirm: true,
            },
        )
        .await,
    );
    assert!(!r.is_error);
    let sc = r.structured_content.unwrap();
    assert_eq!(sc["ok"], true);
    assert_eq!(sc["remaining"], 0);
    // 明确告知只删索引
    assert!(sc["note"].as_str().unwrap().contains("未删除"));

    let store = InstanceStore::new(c.data_dir()).load().unwrap();
    assert!(store.instances.is_empty());
}

#[tokio::test]
async fn delete_unknown_id_is_business_not_found() {
    let (c, _) = ctx("delete-missing");
    let r = as_result(
        delete_instance::handle(
            c.clone(),
            delete_instance::DeleteInstanceArgs {
                instance_id: "nope".into(),
                confirm: true,
            },
        )
        .await,
    );
    assert_eq!(code_of(&r).as_deref(), Some("not_found"));
}

#[tokio::test]
async fn delete_is_idempotent_on_second_call() {
    // 第二次删除同一 ID：应当报 not_found，而不是崩溃或静默成功。
    let (c, _) = ctx("delete-twice");
    let created =
        as_result(create_instance::handle(c.clone(), create_args("x", "1.20.1", "vanilla")).await);
    let id = created.structured_content.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let first = as_result(
        delete_instance::handle(
            c.clone(),
            delete_instance::DeleteInstanceArgs {
                instance_id: id.clone(),
                confirm: true,
            },
        )
        .await,
    );
    assert!(!first.is_error);

    let second = as_result(
        delete_instance::handle(
            c.clone(),
            delete_instance::DeleteInstanceArgs {
                instance_id: id,
                confirm: true,
            },
        )
        .await,
    );
    assert!(second.is_error);
    assert_eq!(code_of(&second).as_deref(), Some("not_found"));
}

// ===========================================================================
// 插件级：清单一致性 + schema
// ===========================================================================

#[test]
fn tool_set_matches_manifest() {
    let set = official_instance::tools();
    set.validate().expect("工具集合应当自洽");

    let manifest =
        launcher_plugin_common::PluginManifestV1::parse(include_bytes!("../plugin.json"))
            .expect("plugin.json 应当合法");

    // 双向一致性：声明 vs 注册
    manifest
        .check_tools(&set.manifest_tools())
        .expect("plugin.json 的 tools 必须与代码注册的完全一致");
    assert_eq!(
        manifest.tools.len(),
        4,
        "本批应有 4 个工具：create/list/switch/delete"
    );
}

#[test]
fn delete_instance_is_marked_destructive() {
    let set = official_instance::tools();
    let d = set
        .tools
        .iter()
        .find(|t| t.short_name == "delete_instance")
        .expect("delete_instance 必须存在");
    assert!(
        d.destructive,
        "删除必须标记 destructive，UI 才知道要弹确认框"
    );
    assert!(d.timeout_ms > 0);

    // 其他三个不 destructive
    for t in &set.tools {
        if t.short_name != "delete_instance" {
            assert!(!t.destructive, "{} 不应标记 destructive", t.short_name);
        }
    }
}

#[test]
fn all_schemas_are_jsonschema_valid_subset() {
    // 宏生成的 schema 必须落在框架支持子集内，否则 register_tool 会失败。
    let set = official_instance::tools();
    for t in &set.tools {
        launcher_plugin_framework::json::validate_schema(&t.input_schema)
            .unwrap_or_else(|e| panic!("{} 的 schema 不合法：{e}", t.short_name));
    }
}

#[test]
fn create_schema_declares_loader_enum() {
    let set = official_instance::tools();
    let d = set
        .tools
        .iter()
        .find(|t| t.short_name == "create_instance")
        .unwrap();
    let enum_vals = d.input_schema["properties"]["loader"]["enum"]
        .as_array()
        .expect("loader 必须有 enum");
    let vals: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(vals, vec!["vanilla", "fabric", "forge"]);
    // 三个必填
    let required: Vec<&str> = d.input_schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(required, vec!["name", "game_version", "loader"]);
}

#[test]
fn timestamp_format_is_rfc3339_utc() {
    let ts = official_instance::store::now_rfc3339();
    // 形如 2026-11-07T13:53:45Z
    assert_eq!(ts.len(), 20, "长度应为 20，实际 `{ts}`");
    assert!(ts.ends_with('Z'));
    assert_eq!(&ts[4..5], "-");
    assert_eq!(&ts[7..8], "-");
    assert_eq!(&ts[10..11], "T");
    assert_eq!(&ts[13..14], ":");
    assert_eq!(&ts[16..17], ":");

    let year: i32 = ts[0..4].parse().unwrap();
    assert!((2024..=2100).contains(&year), "年份合理：{year}");
    let month: u32 = ts[5..7].parse().unwrap();
    assert!((1..=12).contains(&month), "月份合理：{month}");
    let day: u32 = ts[8..10].parse().unwrap();
    assert!((1..=31).contains(&day), "日期合理：{day}");
}
