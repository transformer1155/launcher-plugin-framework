//! `official-mod` 工具测试。
//!
//! # 网络测试策略
//!
//! 规格要求"测试直接调 handler，不启动框架"，同理也不能真的打 Modrinth。
//! 这里用 [`common::HttpPort`] 的 mock 实现：
//!
//! - 一个**正常** mock：返回固定 JSON，用来测解析与成功路径；
//! - 一个**失败** mock：模拟 500 / 超时 / 坏 JSON，测上游错误路径；
//! - 一个**白名单** mock：真实用 [`AllowListedHttp`] 包一层，验证"域名不在白名单则
//!   请求根本发不出去"—— 这是网络安全属性的核心断言。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use launcher_plugin_common::{
    AllowListedHttp, CapturingLogger, HttpPort, HttpReply, LogLevel, NetworkDecl, PluginContext,
};
use launcher_plugin_framework::types::ToolResult;
use official_mod::modrinth;
use official_mod::store::{ModIndex, ModStore};
use official_mod::tools::{install_mod, list_installed_mods, remove_mod, search_mods};

// ---------------------------------------------------------------------------
// 测试脚手架
// ---------------------------------------------------------------------------

fn ctx(tag: &str) -> (PluginContext, Arc<CapturingLogger>) {
    // 用共享的临时目录（进程退出时统一清理，避免遗留垃圾）。
    let dir = launcher_plugin_common::test_support::temp_path(tag);
    let logger = Arc::new(CapturingLogger::default());
    let c = PluginContext::new("com.example.official-mod", dir, logger.clone());
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

/// 固定响应 mock。
struct FixedHttp {
    status: u16,
    body: Vec<u8>,
    calls: Arc<AtomicUsize>,
}

impl HttpPort for FixedHttp {
    fn get(
        &self,
        _url: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HttpReply, String>> + Send>>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let status = self.status;
        let body = self.body.clone();
        Box::pin(async move { Ok(HttpReply { status, body }) })
    }
}

/// 传输层失败的 mock。
struct FailingHttp;

impl HttpPort for FailingHttp {
    fn get(
        &self,
        _url: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HttpReply, String>> + Send>>
    {
        Box::pin(async move { Err("connection reset by peer".to_owned()) })
    }
}

const HITS_JSON: &[u8] = br#"{"hits":[
    {"project_id":"P1","title":"JEI","description":"recipe viewer"},
    {"project_id":"P2","title":"Sodium","description":"renderer"},
    {"project_id":"P3","title":"Fabric API","description":"core lib"}
]}"#;

fn search_ctx_with(http: Arc<dyn HttpPort>) -> (PluginContext, Arc<CapturingLogger>) {
    let (c, l) = ctx("search");
    (c.with_http(http), l)
}

fn search_args(
    q: &str,
    loader: &str,
    ver: &str,
    limit: Option<u64>,
) -> search_mods::SearchModsArgs {
    search_mods::SearchModsArgs {
        query: q.to_owned(),
        loader: loader.to_owned(),
        game_version: ver.to_owned(),
        limit,
    }
}

// ===========================================================================
// search_mods
// ===========================================================================

#[tokio::test]
async fn search_returns_parsed_results() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (c, _) = search_ctx_with(Arc::new(FixedHttp {
        status: 200,
        body: HITS_JSON.to_vec(),
        calls: Arc::clone(&calls),
    }));

    let r = as_result(
        search_mods::handle(c.clone(), search_args("jei", "fabric", "1.20.1", None)).await,
    );
    assert!(!r.is_error, "正常搜索应成功");
    let sc = r.structured_content.unwrap();
    let results = sc["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["project_id"], "P1");
    assert_eq!(results[0]["name"], "JEI");
    assert!(results[0]["description"].is_string());
    assert_eq!(sc["source"], "modrinth");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "应当只发一次请求");
}

#[tokio::test]
async fn search_without_network_returns_upstream_error() {
    // 按裁决方案 (a)：环境缺能力 → 业务错误 upstream_error，不是 PermissionDenied。
    let (c, logger) = ctx("no-network");
    let r = as_result(
        search_mods::handle(c.clone(), search_args("jei", "fabric", "1.20.1", None)).await,
    );
    assert!(r.is_error);
    assert_eq!(
        code_of(&r).as_deref(),
        Some("upstream_error"),
        "缺网络能力应是业务错误"
    );
    assert!(!code_of(&r).as_deref().unwrap_or("").contains("Permission"));
    assert!(logger.saw(LogLevel::Warn, "搜索不可用"));
}

#[tokio::test]
async fn search_reports_http_error_as_upstream() {
    let (c, logger) = search_ctx_with(Arc::new(FixedHttp {
        status: 503,
        body: b"service unavailable".to_vec(),
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let r =
        as_result(search_mods::handle(c.clone(), search_args("x", "fabric", "1.20.1", None)).await);
    assert!(r.is_error);
    assert_eq!(code_of(&r).as_deref(), Some("upstream_error"));
    assert!(logger.saw(LogLevel::Error, "Modrinth 搜索失败"));
}

#[tokio::test]
async fn search_reports_transport_failure() {
    let (c, _) = search_ctx_with(Arc::new(FailingHttp));
    let r =
        as_result(search_mods::handle(c.clone(), search_args("x", "fabric", "1.20.1", None)).await);
    assert_eq!(code_of(&r).as_deref(), Some("upstream_error"));
}

#[tokio::test]
async fn search_reports_bad_json() {
    let (c, _) = search_ctx_with(Arc::new(FixedHttp {
        status: 200,
        body: b"{not json".to_vec(),
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let r =
        as_result(search_mods::handle(c.clone(), search_args("x", "fabric", "1.20.1", None)).await);
    assert_eq!(code_of(&r).as_deref(), Some("upstream_error"));
}

#[tokio::test]
async fn search_rejects_empty_query() {
    let (c, _) = search_ctx_with(Arc::new(FixedHttp {
        status: 200,
        body: HITS_JSON.to_vec(),
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let r = as_result(
        search_mods::handle(c.clone(), search_args("   ", "fabric", "1.20.1", None)).await,
    );
    assert_eq!(
        code_of(&r).as_deref(),
        Some("InvalidArguments"),
        "空 query 是参数错误"
    );
}

#[tokio::test]
async fn search_rejects_limit_out_of_range() {
    let (c, _) = search_ctx_with(Arc::new(FixedHttp {
        status: 200,
        body: HITS_JSON.to_vec(),
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    for bad in [Some(0u64), Some(51), Some(1000)] {
        let r = as_result(
            search_mods::handle(c.clone(), search_args("jei", "fabric", "1.20.1", bad)).await,
        );
        assert!(r.is_error, "limit={bad:?} 应当被拒");
        assert_eq!(
            code_of(&r).as_deref(),
            Some("InvalidArguments"),
            "limit={bad:?}"
        );
    }
    // 边界：1 与 50 合法
    for ok in [Some(1u64), Some(50)] {
        let r = as_result(
            search_mods::handle(c.clone(), search_args("jei", "fabric", "1.20.1", ok)).await,
        );
        assert!(!r.is_error, "limit={ok:?} 应当合法");
    }
}

#[tokio::test]
async fn search_allowlist_blocks_unlisted_host() {
    // 网络安全核心断言：用真实的 AllowListedHttp 包一层，域名不在白名单则请求发不出去。
    let calls = Arc::new(AtomicUsize::new(0));
    let inner = Arc::new(FixedHttp {
        status: 200,
        body: HITS_JSON.to_vec(),
        calls: Arc::clone(&calls),
    });
    // 白名单里故意只放一个**别的**域名
    let decl = NetworkDecl {
        allow_hosts: vec!["example.com".to_owned()],
        reason: "test".to_owned(),
    };
    let gated = Arc::new(AllowListedHttp::new(inner, &decl));
    let (c, _) = ctx("allowlist-block");
    let c = c.with_http(gated);

    let r = as_result(
        search_mods::handle(c.clone(), search_args("jei", "fabric", "1.20.1", None)).await,
    );
    assert!(r.is_error, "白名单外的域名必须被拒");
    assert_eq!(code_of(&r).as_deref(), Some("upstream_error"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "被白名单拦下的请求绝不能真的到达传输层"
    );
}

#[tokio::test]
async fn search_allowlist_permits_declared_host() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inner = Arc::new(FixedHttp {
        status: 200,
        body: HITS_JSON.to_vec(),
        calls: Arc::clone(&calls),
    });
    let decl = NetworkDecl {
        allow_hosts: vec!["api.modrinth.com".to_owned()],
        reason: "mod search".to_owned(),
    };
    let gated = Arc::new(AllowListedHttp::new(inner, &decl));
    let (c, _) = ctx("allowlist-ok");
    let c = c.with_http(gated);

    let r = as_result(
        search_mods::handle(c.clone(), search_args("jei", "fabric", "1.20.1", None)).await,
    );
    assert!(!r.is_error, "白名单内应当放行");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn search_does_not_log_user_query() {
    // 隐私：日志里不能记录用户搜了什么。
    let (c, logger) = search_ctx_with(Arc::new(FixedHttp {
        status: 200,
        body: HITS_JSON.to_vec(),
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let secret = "my-secret-search-term";
    let _ = as_result(
        search_mods::handle(c.clone(), search_args(secret, "fabric", "1.20.1", None)).await,
    );
    assert!(
        !logger.saw(LogLevel::Info, secret),
        "日志不应包含用户查询原文"
    );
}

// ===========================================================================
// list_installed_mods
// ===========================================================================

#[tokio::test]
async fn list_empty_instance() {
    let (c, _) = ctx("list-empty");
    let r = as_result(
        list_installed_mods::handle(
            c.clone(),
            list_installed_mods::ListInstalledModsArgs {
                instance_id: "inst-1".into(),
            },
        )
        .await,
    );
    assert!(!r.is_error, "无记录应返回空列表");
    assert_eq!(r.structured_content.unwrap()["mods"], serde_json::json!([]));
}

#[tokio::test]
async fn list_rejects_empty_instance_id() {
    let (c, _) = ctx("list-bad-id");
    let r = as_result(
        list_installed_mods::handle(
            c.clone(),
            list_installed_mods::ListInstalledModsArgs {
                instance_id: "  ".into(),
            },
        )
        .await,
    );
    assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"));
}

#[tokio::test]
async fn list_rejects_path_traversal_in_instance_id() {
    let (c, _) = ctx("list-traversal");
    for bad in ["../etc", "a/b", "a\\b", "C:"] {
        let r = as_result(
            list_installed_mods::handle(
                c.clone(),
                list_installed_mods::ListInstalledModsArgs {
                    instance_id: bad.into(),
                },
            )
            .await,
        );
        assert!(r.is_error, "`{bad}` 应被拒");
        assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"), "`{bad}`");
    }
}

// ===========================================================================
// install_mod
// ===========================================================================

#[tokio::test]
async fn install_writes_placeholder_and_records() {
    let (c, logger) = ctx("install-ok");
    let r = as_result(
        install_mod::handle(
            c.clone(),
            install_mod::InstallModArgs {
                instance_id: "inst-1".into(),
                project_id: "P7dR8mSH".into(),
                filename: None,
            },
        )
        .await,
    );
    assert!(!r.is_error);
    let sc = r.structured_content.unwrap();
    assert_eq!(sc["simulated"], true, "必须标注是模拟安装");
    assert_eq!(sc["filename"], "P7dR8mSH.jar");
    assert!(sc["note"].as_str().unwrap().contains("模拟"));

    // 日志必须标 simulated
    assert!(logger.saw(LogLevel::Info, "simulated"));

    // 文件真的写出来了，且内容是占位而非二进制
    let dir = c.data_dir().join("instances/inst-1/mods");
    let path = dir.join("P7dR8mSH.jar");
    assert!(path.exists(), "占位文件应当存在");
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("simulated"), "内容应是占位文本");

    // 索引里有记录
    let store = ModIndex::new(c.data_dir()).load().unwrap();
    let mods = store.instances.get("inst-1").unwrap();
    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].project_id.as_deref(), Some("P7dR8mSH"));
}

#[tokio::test]
async fn install_twice_is_idempotent_overwrite() {
    let (c, _) = ctx("install-twice");
    for _ in 0..2 {
        let r = as_result(
            install_mod::handle(
                c.clone(),
                install_mod::InstallModArgs {
                    instance_id: "inst-1".into(),
                    project_id: "P1".into(),
                    filename: None,
                },
            )
            .await,
        );
        assert!(!r.is_error);
    }
    let store = ModIndex::new(c.data_dir()).load().unwrap();
    assert_eq!(
        store.instances.get("inst-1").unwrap().len(),
        1,
        "重复安装同一个 Mod 应覆盖而不是追加"
    );
}

#[tokio::test]
async fn install_rejects_bad_filename() {
    let (c, _) = ctx("install-bad-name");
    for bad in ["../evil.jar", "a/b.jar", "noext", "x.txt", "a\\b.jar"] {
        let r = as_result(
            install_mod::handle(
                c.clone(),
                install_mod::InstallModArgs {
                    instance_id: "inst-1".into(),
                    project_id: "P1".into(),
                    filename: Some(bad.into()),
                },
            )
            .await,
        );
        assert!(r.is_error, "`{bad}` 应被拒");
        assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"), "`{bad}`");
    }
}

#[tokio::test]
async fn install_sanitizes_project_id_into_filename() {
    // project_id 里出现怪字符时，生成的文件名必须仍然安全。
    let (c, _) = ctx("install-sanitize");
    let r = as_result(
        install_mod::handle(
            c.clone(),
            install_mod::InstallModArgs {
                instance_id: "inst-1".into(),
                project_id: "../evil/../x".into(),
                filename: None,
            },
        )
        .await,
    );
    assert!(!r.is_error);
    let fname = r.structured_content.unwrap()["filename"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        !fname.contains('/') && !fname.contains(".."),
        "文件名必须安全：{fname}"
    );
    // 文件落在 mods 目录内，没有逃逸
    let escaped = c.data_dir().join("instances").join("evil");
    assert!(!escaped.exists(), "绝不能写到上级目录");
}

#[tokio::test]
async fn install_enforces_mod_limit() {
    let (c, _) = ctx("install-limit");
    let dir = c.ensure_data_dir().unwrap().to_path_buf();
    let mut store = ModStore::default();
    let mods: Vec<official_mod::store::ModEntry> = (0..512)
        .map(|i| official_mod::store::ModEntry {
            filename: format!("m{i}.jar"),
            name: format!("m{i}"),
            version: "1".into(),
            project_id: None,
            installed_at: "2026-01-01T00:00:00Z".into(),
        })
        .collect();
    store.instances.insert("inst-1".into(), mods);
    ModIndex::new(&dir).save(&store).unwrap();

    let r = as_result(
        install_mod::handle(
            c.clone(),
            install_mod::InstallModArgs {
                instance_id: "inst-1".into(),
                project_id: "P-new".into(),
                filename: None,
            },
        )
        .await,
    );
    assert!(r.is_error);
    assert_eq!(code_of(&r).as_deref(), Some("limit_exceeded"));
}

// ===========================================================================
// remove_mod
// ===========================================================================

#[tokio::test]
async fn remove_deletes_record_and_file() {
    let (c, _) = ctx("remove-ok");
    // 先装一个
    let _ = as_result(
        install_mod::handle(
            c.clone(),
            install_mod::InstallModArgs {
                instance_id: "inst-1".into(),
                project_id: "P1".into(),
                filename: None,
            },
        )
        .await,
    );
    let file = c.data_dir().join("instances/inst-1/mods/P1.jar");
    assert!(file.exists());

    let r = as_result(
        remove_mod::handle(
            c.clone(),
            remove_mod::RemoveModArgs {
                instance_id: "inst-1".into(),
                filename: "P1.jar".into(),
            },
        )
        .await,
    );
    assert!(!r.is_error);
    let sc = r.structured_content.unwrap();
    assert_eq!(sc["ok"], true);
    assert_eq!(sc["file_deleted"], true);
    assert!(!file.exists(), "磁盘文件应当被删除");

    let store = ModIndex::new(c.data_dir()).load().unwrap();
    assert!(
        !store.instances.contains_key("inst-1"),
        "空实例的 key 应当被清理"
    );
}

#[tokio::test]
async fn remove_missing_file_is_still_successful() {
    // 索引有记录、文件被手动删了 —— 移除仍应成功（幂等友好）。
    let (c, _) = ctx("remove-nofile");
    let _ = as_result(
        install_mod::handle(
            c.clone(),
            install_mod::InstallModArgs {
                instance_id: "inst-1".into(),
                project_id: "P1".into(),
                filename: None,
            },
        )
        .await,
    );
    std::fs::remove_file(c.data_dir().join("instances/inst-1/mods/P1.jar")).unwrap();

    let r = as_result(
        remove_mod::handle(
            c.clone(),
            remove_mod::RemoveModArgs {
                instance_id: "inst-1".into(),
                filename: "P1.jar".into(),
            },
        )
        .await,
    );
    assert!(!r.is_error, "文件缺失不应导致失败");
    assert_eq!(r.structured_content.unwrap()["file_deleted"], false);
}

#[tokio::test]
async fn remove_unknown_mod_is_not_found() {
    let (c, _) = ctx("remove-unknown");
    let _ = as_result(
        install_mod::handle(
            c.clone(),
            install_mod::InstallModArgs {
                instance_id: "inst-1".into(),
                project_id: "P1".into(),
                filename: None,
            },
        )
        .await,
    );
    let r = as_result(
        remove_mod::handle(
            c.clone(),
            remove_mod::RemoveModArgs {
                instance_id: "inst-1".into(),
                filename: "nope.jar".into(),
            },
        )
        .await,
    );
    assert!(r.is_error);
    assert_eq!(code_of(&r).as_deref(), Some("not_found"));
}

#[tokio::test]
async fn remove_from_unknown_instance_is_not_found() {
    let (c, _) = ctx("remove-unknown-inst");
    let r = as_result(
        remove_mod::handle(
            c.clone(),
            remove_mod::RemoveModArgs {
                instance_id: "ghost".into(),
                filename: "a.jar".into(),
            },
        )
        .await,
    );
    assert_eq!(code_of(&r).as_deref(), Some("not_found"));
}

// ===========================================================================
// 插件级断言
// ===========================================================================

#[test]
fn tool_set_matches_manifest() {
    let set = official_mod::tools();
    set.validate().expect("工具集合应自洽");

    let manifest =
        launcher_plugin_common::PluginManifestV1::parse(include_bytes!("../plugin.json"))
            .expect("plugin.json 应合法");
    manifest
        .check_tools(&set.manifest_tools())
        .expect("plugin.json 的 tools 必须与代码注册一致");
    assert_eq!(manifest.tools.len(), 4);
}

#[test]
fn manifest_network_matches_code() {
    // 代码里声明的必需域名必须与 plugin.json 的 network.allow_hosts 一致。
    let manifest =
        launcher_plugin_common::PluginManifestV1::parse(include_bytes!("../plugin.json")).unwrap();
    let net = manifest.network.expect("本插件必须声明 network");
    let declared: Vec<String> = net.allow_hosts.clone();
    let expected: Vec<String> = official_mod::REQUIRED_HOSTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(declared, expected, "代码与清单的域名白名单必须一致");
    assert!(!net.reason.is_empty(), "必须说明为什么要网络");
}

#[test]
fn destructive_flags() {
    let set = official_mod::tools();
    let get = |n: &str| set.tools.iter().find(|t| t.short_name == n).unwrap();
    assert!(
        get("install_mod").destructive,
        "安装会改游戏目录，必须标 destructive"
    );
    assert!(
        get("remove_mod").destructive,
        "移除会删文件，必须标 destructive"
    );
    assert!(!get("list_installed_mods").destructive);
    assert!(!get("search_mods").destructive, "搜索是只读的");
}

#[test]
fn all_schemas_valid_subset() {
    let set = official_mod::tools();
    for t in &set.tools {
        launcher_plugin_framework::json::validate_schema(&t.input_schema)
            .unwrap_or_else(|e| panic!("{} schema 非法：{e}", t.short_name));
    }
}

#[test]
fn search_schema_has_enum_and_range() {
    let set = official_mod::tools();
    let d = set
        .tools
        .iter()
        .find(|t| t.short_name == "search_mods")
        .unwrap();
    let vals: Vec<&str> = d.input_schema["properties"]["loader"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(vals, vec!["vanilla", "fabric", "forge"]);
    assert_eq!(
        d.input_schema["properties"]["limit"]["minimum"], 1.0,
        "limit 必须有下界"
    );
    assert_eq!(
        d.input_schema["properties"]["limit"]["maximum"], 50.0,
        "limit 必须有上界"
    );
}

#[test]
fn modrinth_loader_values_are_single_source() {
    // modrinth 模块的 LOADER_VALUES 必须与 schema 的 enum 一致。
    let set = official_mod::tools();
    let d = set
        .tools
        .iter()
        .find(|t| t.short_name == "search_mods")
        .unwrap();
    let schema_vals: Vec<&str> = d.input_schema["properties"]["loader"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(schema_vals, modrinth::LOADER_VALUES.to_vec());
}
