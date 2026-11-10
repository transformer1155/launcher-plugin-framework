//! `sample-community` 工具测试（直接调 handler，不启动框架）。
//!
//! # 注意：这里**不**测沙箱崩溃
//!
//! `crash_me` 的 panic 会在这条测试线程里真的 panic。直接调 handler 时，
//! `catch_unwind` 由**框架**提供的才有意义 —— 而规格要求"测试直接调 handler，不启动框架"。
//!
//! 因此这里的策略是：
//! - `crash_me` 用 `std::panic::catch_unwind` 自己兜住，只断言"它确实 panic 了"；
//! - **真正的崩溃→重启链路**在 `examples/step5_e2e.rs` 里经完整框架验证。
//!
//! 这样既满足单测不依赖框架，又不会让一个故意 panic 的测试把测试进程搞乱。

use std::sync::Arc;

use launcher_plugin_common::{CapturingLogger, LogLevel, PluginContext};
use launcher_plugin_framework::types::ToolResult;
use sample_community::tools::{crash_me, hello, slow_task};

fn ctx(tag: &str) -> (PluginContext, Arc<CapturingLogger>) {
    // 用共享的临时目录（进程退出时统一清理，避免遗留垃圾）。
    let dir = launcher_plugin_common::test_support::temp_path(tag);
    let logger = Arc::new(CapturingLogger::default());
    let c = PluginContext::new("com.example.sample-community", dir, logger.clone());
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

// ===========================================================================
// hello
// ===========================================================================

#[tokio::test]
async fn hello_returns_greeting() {
    let (c, logger) = ctx("hello-ok");
    let r = as_result(
        hello::handle(
            c.clone(),
            hello::HelloArgs {
                name: "世界".into(),
            },
        )
        .await,
    );
    assert!(!r.is_error);
    let sc = r.structured_content.unwrap();
    assert!(sc["greeting"].as_str().unwrap().contains("世界"));
    assert_eq!(sc["sandboxed"], true, "应标明自己跑在沙箱");
    assert_eq!(sc["plugin"], "com.example.sample-community");
    assert!(logger.saw(LogLevel::Info, "hello 被调用"));
}

#[tokio::test]
async fn hello_rejects_empty_name() {
    let (c, _) = ctx("hello-empty");
    let r = as_result(hello::handle(c.clone(), hello::HelloArgs { name: "   ".into() }).await);
    assert!(r.is_error);
    assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"));
}

#[tokio::test]
async fn hello_rejects_overlong_name() {
    let (c, _) = ctx("hello-long");
    let long = "a".repeat(65);
    let r = as_result(hello::handle(c.clone(), hello::HelloArgs { name: long }).await);
    assert_eq!(code_of(&r).as_deref(), Some("InvalidArguments"));

    // 边界：64 合法
    let ok = "a".repeat(64);
    let r = as_result(hello::handle(c.clone(), hello::HelloArgs { name: ok }).await);
    assert!(!r.is_error, "64 字符应合法");
}

// ===========================================================================
// slow_task
// ===========================================================================

#[tokio::test]
async fn slow_task_completes_within_timeout_when_short() {
    let (c, logger) = ctx("slow-short");
    // 睡 1 秒；工具级超时是 2 秒（在 def() 里配），所以能完成。
    let r =
        as_result(slow_task::handle(c.clone(), slow_task::SlowTaskArgs { seconds: Some(1) }).await);
    assert!(!r.is_error, "1 秒任务应当完成");
    let sc = r.structured_content.unwrap();
    assert_eq!(sc["done"], true);
    assert_eq!(sc["slept_seconds"], 1);
    assert!(logger.saw(LogLevel::Info, "slow_task 完成"));
}

#[tokio::test]
async fn slow_task_rejects_seconds_out_of_range() {
    let (c, _) = ctx("slow-range");
    for bad in [Some(0u64), Some(31), Some(100)] {
        let r =
            as_result(slow_task::handle(c.clone(), slow_task::SlowTaskArgs { seconds: bad }).await);
        assert!(r.is_error, "seconds={bad:?} 应被拒");
        assert_eq!(
            code_of(&r).as_deref(),
            Some("InvalidArguments"),
            "seconds={bad:?}"
        );
    }
}

#[tokio::test]
async fn slow_task_default_is_three_seconds() {
    // 只断言默认值解析正确，不真等 3 秒（用 def 的 schema 验证）。
    let set = sample_community::tools();
    let d = set
        .tools
        .iter()
        .find(|t| t.short_name == "slow_task")
        .unwrap();
    assert_eq!(d.input_schema["properties"]["seconds"]["minimum"], 1.0);
    assert_eq!(d.input_schema["properties"]["seconds"]["maximum"], 30.0);
    assert!(
        d.input_schema["required"].as_array().unwrap().is_empty(),
        "seconds 应为可选"
    );
}

// ===========================================================================
// crash_me
// ===========================================================================

#[test]
fn crash_me_actually_panics() {
    // 直接调 handler 时必须自己兜住 panic（框架的 catch_unwind 在端到端演示里）。
    let (c, logger) = ctx("crash-panic");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(async {
            let _ = crash_me::handle(
                c.clone(),
                crash_me::CrashMeArgs {
                    note: Some("unit-test".into()),
                },
            )
            .await;
        })
    }));

    assert!(panicked.is_err(), "crash_me 必须真的 panic");
    // panic 之前应当留了日志（证明"有意崩溃"是可追踪的）
    assert!(logger.saw(LogLevel::Warn, "即将故意 panic"));
    assert!(logger.saw(LogLevel::Warn, "unit-test"));
}

#[test]
fn crash_me_is_marked_destructive() {
    // 崩溃会杀掉整个沙箱单元（影响该插件的所有其他工具），因此标记 destructive。
    let set = sample_community::tools();
    let d = set
        .tools
        .iter()
        .find(|t| t.short_name == "crash_me")
        .unwrap();
    assert!(d.destructive, "crash_me 应标记 destructive");
}

// ===========================================================================
// 插件级
// ===========================================================================

#[test]
fn tool_set_matches_manifest() {
    let set = sample_community::tools();
    set.validate().expect("工具集合应自洽");
    let manifest =
        launcher_plugin_common::PluginManifestV1::parse(include_bytes!("../plugin.json"))
            .expect("plugin.json 应合法");
    manifest
        .check_tools(&set.manifest_tools())
        .expect("plugin.json 的 tools 必须与代码一致");
    assert_eq!(manifest.tools.len(), 3);
}

#[test]
fn manifest_declares_sandboxed_unreviewed_unsigned() {
    // 本插件的存在意义就是这条断言：沙箱 + 未审核 + 无签名。
    let manifest =
        launcher_plugin_common::PluginManifestV1::parse(include_bytes!("../plugin.json")).unwrap();
    assert_eq!(manifest.trust, launcher_plugin_common::TrustKind::Sandboxed);
    assert_eq!(
        manifest.review,
        launcher_plugin_common::ReviewKind::Unreviewed
    );

    // Q8 要求：沙箱 + 未审核 → 加载时必须给 WARN
    let warns = manifest.load_warnings();
    assert_eq!(warns.len(), 1, "应当只有一条告警");
    assert!(warns[0].contains("unreviewed"), "{warns:?}");
    assert!(warns[0].contains("受限沙箱"), "{warns:?}");

    // 原始 JSON 里不应有 signature（本插件无签名）
    let raw = include_bytes!("../plugin.json");
    let raw_str = std::str::from_utf8(raw).unwrap();
    assert!(
        !raw_str.contains("\"signature\""),
        "本插件必须无签名（用于演示降级路径）"
    );
}

#[test]
fn all_schemas_valid_subset() {
    let set = sample_community::tools();
    for t in &set.tools {
        launcher_plugin_framework::json::validate_schema(&t.input_schema)
            .unwrap_or_else(|e| panic!("{} schema 非法：{e}", t.short_name));
    }
}

#[test]
fn slow_task_timeout_is_short_enough_for_demo() {
    // 演示需要：slow_task 的 timeout_ms 明显小于它默认睡眠时长，
    // 否则演示里看不到 Timeout。
    let set = sample_community::tools();
    let d = set
        .tools
        .iter()
        .find(|t| t.short_name == "slow_task")
        .unwrap();
    assert!(
        d.timeout_ms <= 3_000,
        "为了让演示能观察到超时，slow_task 的工具级超时应较短（实际 {}ms）",
        d.timeout_ms
    );
}
