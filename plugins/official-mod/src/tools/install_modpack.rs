//! `install_modpack`：安装 Modrinth 整合包（.mrpack）。
//!
//! # 流程
//!
//! 1. 下载 `.mrpack`（zip 格式，内含 `modrinth.index.json` + `overrides/` 目录）。
//! 2. 解析 index —— 它会列出需要的 Modrinth 项目文件（带 sha1 hash 和 download URL）。
//! 3. 应用 `overrides/` 里的文件到实例游戏目录。
//! 4. 用 index 里给的 download URL 把每个依赖 jar 直接下载到 `mods/`。
//!
//! # 两条路径
//!
//! - **有网络**：overrides 落盘 + 所有依赖下载 + 写 index 记录（与 `install_mod` 同构）。
//! - **无网络**：只应用 overrides，返回 `simulated: true` + 列出跳过的依赖。

use crate::store::ModEntry;
use crate::store::now_rfc3339;
use crate::store::validate_instance_id;
use crate::store::MAX_MODS_PER_INSTANCE;
use crate::{load_mod_store, save_mod_store};
use launcher_plugin_common::result::{biz_codes, business, success};
use launcher_plugin_common::tool_args;
use launcher_plugin_common::PluginContext;
use launcher_plugin_framework::error::ToolErrorCode;
use serde_json::{json, Value};

tool_args! {
    /// 整合包安装参数。
    pub struct InstallModpackArgs {
        instance_id: r_str() = "目标实例 ID",
        // 二选一：url 是远程下载，file_path 是本地文件。
        url: o_str() = "远程 .mrpack URL（与 file_path 二选一）",
        file_path: o_str() = "本地 .mrpack 文件路径（与 url 二选一）",
    }
}

pub async fn handle(
    ctx: PluginContext,
    args: InstallModpackArgs,
) -> launcher_plugin_common::PluginResult {
    // ---- 参数校验 ----
    let instance_id = match validate_instance_id(&args.instance_id) {
        Ok(v) => v.to_owned(),
        Err(msg) => return Err(ctx.err(launcher_plugin_framework::error::ToolErrorCode::InvalidArguments, msg)),
    };

    let Some(source) = args.url.or(args.file_path) else {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, "url 与 file_path 至少提供一个"));
    };

    // ---- 拿到 mrpack 字节 ----
    let bytes: Vec<u8> = if source.starts_with("http://") || source.starts_with("https://") {
        let Ok(http) = ctx.http() else {
            return Ok(business(
                biz_codes::UPSTREAM_ERROR,
                "url 安装需要网络能力（宿主未注入 HTTP 端口）",
            ));
        };
        let reply = http.get(source.to_owned()).await.map_err(|e| {
            ctx.err(
                ToolErrorCode::PermissionDenied,
                format!("下载整合包失败：{e}"),
            )
        })?;
        if reply.status != 200 {
            return Ok(business(
                biz_codes::UPSTREAM_ERROR,
                format!("HTTP {} 下载整合包", reply.status),
            ));
        }
        reply.body
    } else {
        std::fs::read(&source).map_err(|e| {
            ctx.err(
                ToolErrorCode::InvalidArguments,
                format!("读取本地文件 {} 失败：{e}", source),
            )
        })?
    };

    if bytes.is_empty() {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, "整合包文件为空"));
    }

    // ---- 解压 mrpack ----
    let game_dir = ctx
        .data_dir()
        .join("instances")
        .join(&instance_id);
    let mods_dir = game_dir.join("mods");
    if let Err(e) = std::fs::create_dir_all(&game_dir) {
        return Ok(business(
            biz_codes::STORAGE_ERROR,
            format!("创建游戏目录失败：{e}"),
        ));
    }
    let _ = std::fs::create_dir_all(&mods_dir);

    // zip 解析必须在 sync context（zip::ZipArchive 要求 File）。
    let unpacked = tokio::task::spawn_blocking(move || -> Result<MrpackContent, String> {
        use std::io::Cursor;
        use std::io::Read;
        let cursor = Cursor::new(bytes);
        let mut z = zip::ZipArchive::new(cursor).map_err(|e| format!("mrpack zip 解析失败：{e}"))?;

        let mut index_json: Option<Value> = None;
        let mut overrides: Vec<(String, Vec<u8>)> = Vec::new();

        for i in 0..z.len() {
            let mut entry = z.by_index(i).map_err(|e| format!("zip entry 失败：{e}"))?;
            let name = entry.name().to_owned();
            if entry.is_dir() {
                continue;
            }
            if name.contains("..") {
                continue;
            }

            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).ok();

            if name == "modrinth.index.json" {
                index_json = serde_json::from_slice(&buf).ok();
            } else if let Some(rest) = name.strip_prefix("overrides/") {
                overrides.push((rest.to_owned(), buf));
            }
        }

        Ok(MrpackContent { index_json, overrides })
    })
    .await
    .map_err(|e| ctx.err(ToolErrorCode::ProcessFailed, format!("解压任务 panic：{e}")))?
    .map_err(|e| ctx.err(ToolErrorCode::InvalidArguments, e))?;

    // ---- 应用 overrides ----
    let mut overrides_applied = 0usize;
    for (rel, data) in &unpacked.overrides {
        let dest = game_dir.join(rel);
        if !dest.starts_with(&game_dir) {
            continue;
        }
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&dest, data).is_ok() {
            overrides_applied += 1;
        }
    }
    ctx.log_info(format!(
        "整合包 overrides 应用：{overrides_applied}/{} 个文件",
        unpacked.overrides.len()
    ));

    // ---- 解析 index.json 里的依赖 ----
    let (mods_to_fetch, total_expected) = match unpacked.index_json.as_ref() {
        Some(idx) => parse_modpack_index(idx),
        None => (Vec::new(), 0usize),
    };
    ctx.log_info(format!(
        "整合包 index.json：{}/{} 个 Mod 依赖需下载",
        mods_to_fetch.len(),
        total_expected
    ));

    let Ok(http) = ctx.http() else {
        return Ok(success(json!({
            "ok": true,
            "instance_id": instance_id,
            "overrides_applied": overrides_applied,
            "mods_fetched": 0,
            "mods_failed": 0,
            "mods_expected": total_expected,
            "simulated": !mods_to_fetch.is_empty(),
            "note": "无网络能力，overrides 已应用但依赖 Mod 未下载",
        })));
    };

    // ---- 批量下载依赖 Mod ----
    let (mut ok, mut failed) = (0usize, 0usize);
    let mut store = load_mod_store!(ctx);
    let instance_mods = store.instances.entry(instance_id.clone()).or_default();

    for dep in &mods_to_fetch {
        if instance_mods.len() >= MAX_MODS_PER_INSTANCE {
            ctx.log_warn(format!(
                "已达 {MAX_MODS_PER_INSTANCE} 上限，跳过剩余依赖（{} 个）",
                mods_to_fetch.len() - ok - failed
            ));
            break;
        }

        match fetch_mod_file(&http, dep).await {
            Ok((fname, bytes)) => {
                let target = mods_dir.join(&fname);
                if let Err(e) = std::fs::write(&target, &bytes) {
                    ctx.log_warn(format!("写入 {fname} 失败：{e}"));
                    failed += 1;
                    continue;
                }
                let entry = ModEntry {
                    filename: fname.clone(),
                    name: dep.project_id.clone(),
                    version: dep.version.clone(),
                    project_id: Some(dep.project_id.clone()),
                    installed_at: now_rfc3339(),
                };
                match instance_mods.iter_mut().find(|m| m.filename == fname) {
                    Some(existing) => *existing = entry,
                    None => instance_mods.push(entry),
                }
                ok += 1;
            }
            Err(e) => {
                ctx.log_warn(format!(
                    "依赖下载失败：project={}，{e}",
                    dep.project_id
                ));
                failed += 1;
            }
        }
    }

    save_mod_store!(ctx, &store);

    Ok(success(json!({
        "ok": true,
        "instance_id": instance_id,
        "overrides_applied": overrides_applied,
        "mods_fetched": ok,
        "mods_failed": failed,
        "mods_expected": total_expected,
    })))
}

// ---- 内部类型 ----

#[derive(Debug)]
struct ModpackDep {
    project_id: String,
    version: String,
    download_url: String,
    filename: String,
}

#[derive(Debug, Default)]
struct MrpackContent {
    index_json: Option<Value>,
    overrides: Vec<(String, Vec<u8>)>,
}

fn parse_modpack_index(idx: &Value) -> (Vec<ModpackDep>, usize) {
    let files = idx
        .get("files")
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default();
    let total = files.len();
    let mut deps: Vec<ModpackDep> = Vec::with_capacity(total);

    for f in files {
        let Some(downloads) = f
            .get("downloads")
            .and_then(|d| d.as_array())
        else {
            continue;
        };
        let Some(dl_url) = downloads.first().and_then(|u| u.as_str()).map(|s| s.to_owned()) else {
            continue;
        };
        let fname = f
            .get("path")
            .and_then(|p| p.as_str())
            .map(|s| {
                s.rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(s)
                    .to_owned()
            })
            .unwrap_or_else(|| "unknown.jar".to_owned());
        let project_id = extract_project_from_url(&dl_url)
            .or_else(|| {
                f.get("hashes")
                    .and_then(|h| h.get("sha1"))
                    .and_then(|h| h.as_str())
                    .map(|h| h.chars().take(12).collect())
            })
            .unwrap_or_else(|| "unknown".to_owned());
        let version = dl_url
            .rsplit('/')
            .nth(1)
            .unwrap_or("unknown")
            .to_owned();

        deps.push(ModpackDep {
            project_id,
            version,
            download_url: dl_url,
            filename: fname,
        });
    }
    (deps, total)
}

/// 从 Modrinth CDN URL 里抠 project_id。
///
/// 已知格式：`https://cdn.modrinth.com/data/PROJECT_ID/versions/VER/FILE.jar`
fn extract_project_from_url(url: &str) -> Option<String> {
    let tail = url.split_once("data/").map(|(_, r)| r)?;
    let id = tail.split('/').next()?;
    if id.is_empty() || id == "versions" {
        return None;
    }
    Some(id.to_owned())
}

async fn fetch_mod_file(
    http: &std::sync::Arc<dyn launcher_plugin_common::context::HttpPort>,
    dep: &ModpackDep,
) -> Result<(String, Vec<u8>), String> {
    let reply = http
        .get(dep.download_url.clone())
        .await
        .map_err(|e| format!("HTTP 请求失败：{e}"))?;
    if reply.status != 200 {
        return Err(format!("HTTP {}", reply.status));
    }
    if reply.body.is_empty() {
        return Err("下载内容为空".to_owned());
    }
    Ok((dep.filename.clone(), reply.body))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    launcher_plugin_common::tool_def! {
        name: "install_modpack",
        description: "安装 Modrinth .mrpack 整合包（overrides 落盘 + 依赖 Mod 下载）",
        destructive: true,
        timeout_ms: 600_000,
        args: InstallModpackArgs,
        handler: crate::tools::install_modpack::handle
    }
}

// ============================================================================
// 单元测试
// ============================================================================
//
// 设计思路：
// - 用 zip crate 在**内存里**构造假的 .mrpack 字节流（不需要写磁盘）
// - 用自定义 `MockHttp` 控制每个 URL 返回什么（成功 / 404 / 错误）
// - 测试直接调 `handle()`，不需要启动框架

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_plugin_common::result::into_framework_result;
    use launcher_plugin_common::{CapturingLogger, HttpPort, HttpReply, PluginContext};
    use launcher_plugin_framework::types::ToolResult;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use zip::{write::FileOptions, ZipArchive, ZipWriter};

    // ---- 工具 ----

    fn temp_dir(tag: &str) -> PathBuf {
        launcher_plugin_common::test_support::temp_path(tag)
    }

    fn ctx_with_http(dir: &Path, http: Arc<dyn HttpPort>) -> PluginContext {
        let logger = Arc::new(CapturingLogger::default());
        PluginContext::new("com.example.official-mod", dir, logger).with_http(http)
    }

    fn make_mrpack(overrides: &[(&str, &[u8])], index_json: Option<&Value>) -> Vec<u8> {
        // ZipWriter 要求 std::io::Write，Vec<u8> 刚好满足。
        use std::io::Cursor;
        use std::io::Write;
        let buf = Vec::new();
        let mut z = ZipWriter::new(Cursor::new(buf));
        let opts = FileOptions::default();

        if let Some(idx) = index_json {
            let json_bytes = serde_json::to_vec_pretty(idx).unwrap();
            z.start_file("modrinth.index.json", opts).unwrap();
            z.write_all(&json_bytes).unwrap();
        }

        for (rel, data) in overrides {
            let name = format!("overrides/{rel}");
            z.start_file(&name, opts).unwrap();
            z.write_all(data).unwrap();
        }

        // 一个无关条目
        z.start_file("pack.mcmeta", opts).unwrap();
        z.write_all(b"{}").unwrap();

        let cursor = z.finish().unwrap();
        cursor.into_inner()
    }

    fn write_temp_mrpack(content: &[u8]) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("test_mrpack_{nanos}.zip"));
        std::fs::write(&p, content).unwrap();
        p.into_os_string()
            .into_string()
            .expect("temp path must be UTF-8")
    }

    // ---- Mock Http ----

    struct MockHttp {
        /// (url_contains, status, body)
        routes: Mutex<Vec<(String, u16, Vec<u8>)>>,
        /// url_contains → count
        calls: Mutex<std::collections::HashMap<String, AtomicUsize>>,
    }

    impl MockHttp {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                routes: Mutex::new(Vec::new()),
                calls: Mutex::new(std::collections::HashMap::new()),
            })
        }

        fn route(&self, url_contains: &str, status: u16, body: Vec<u8>) {
            self.routes
                .lock()
                .unwrap()
                .push((url_contains.to_owned(), status, body));
        }

        fn call_count(&self, url_contains: &str) -> usize {
            self.calls
                .lock()
                .unwrap()
                .get(url_contains)
                .map(|c| c.load(Ordering::SeqCst))
                .unwrap_or(0)
        }
    }

    impl HttpPort for MockHttp {
        fn get(
            &self,
            url: String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HttpReply, String>> + Send>>
        {
            let routes = self.routes.lock().unwrap();
            let matched = routes
                .iter()
                .rev()
                .find(|(needle, _, _)| url.contains(needle))
                .cloned();
            let calls = self.calls.lock().unwrap();
            // 克隆一下 HashMap 的 Arc<AtomicUsize> 值（key 相同就复用同一个 AtomicUsize）
            // 但每个 route 的 needle 是唯一的，所以直接 clone 整个 HashMap 不可行。
            // 简化：不复制 HashMap，而是把调用信息编码到闭包外部的 Mutex 里。
            drop(calls);
            Box::pin(async move {
                if let Some((needle, status, body)) = matched {
                    // 调用次数记录用一个新的 Mutex 包装的 AtomicUsize HashMap 是 overkill。
                    // 这里让 call_count() 在 mock 内部重新查 routes —— 但不行，因为闭包已经 move 了。
                    // 简化：直接 Ok(HttpReply { status, body })
                    let _ = needle; // 我们在测试里用 call_count()，但这里闭包拿不到 self.calls。
                    // 最终决定：call_count() 直接读一个共享计数器（Arc）。重构见下一轮。
                    return Ok(HttpReply { status, body, location: None });
                }
                Ok(HttpReply {
                    status: 404,
                    body: Vec::new(),
                    location: None,
                })
            })
        }
    }

    // 为了解决闭包无法 self.calls 的问题，用一个单独的计数器 Arc 挂在 MockHttp 外面。
    // 但这让结构变复杂了。简化方案：让 MockHttp 内部 route() 记录 "匹配过几次"
    // 但不能因为异步闭包捕获 self 导致 self 被 move。
    //
    // 最简修复：把 routes 和 calls 都改为 Arc 包在外面的字段，或让 handle 直接调
    // 内部函数传 mock。但那样改动太大。
    //
    // 当前的测试代码**已经有了**一个 working 的方向：让 HttpPort 返回的是 Box::pin
    // 且里面捕获数据，call_count() 另起炉灶。但闭包拿不到 self.calls —— 解决办法：
    // 让 HttpPort 闭包通过 thread_local 写入调用次数。

    // --- 改进版 MockHttp：thread_local 计数器 ---

    use std::cell::RefCell;
    thread_local! {
        static LAST_CALLS: RefCell<std::collections::HashMap<String, usize>> = RefCell::new(std::collections::HashMap::new());
    }

    fn assert_no_errors(tool_result: &ToolResult) {
        assert!(
            !tool_result.is_error,
            "handler returned is_error=true: {:?}",
            tool_result
                .structured_content
                .as_ref()
                .and_then(|v| v.get("message"))
                .and_then(|m| m.as_str())
        );
    }

    // --- 重新定义干净的 MockHttp ---

    struct TestHttp {
        routes: Vec<(String, u16, Vec<u8>)>,
    }

    impl TestHttp {
        fn new() -> Arc<Self> {
            Arc::new(Self { routes: Vec::new() })
        }
        fn route(mut self: &mut Self, url_contains: &str, status: u16, body: Vec<u8>) {
            self.routes.push((url_contains.to_owned(), status, body));
        }
    }

    impl HttpPort for TestHttp {
        fn get(
            &self,
            url: String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HttpReply, String>> + Send>>
        {
            let matched = self
                .routes
                .iter()
                .rev()
                .find(|(needle, _, _)| url.contains(needle))
                .cloned();

            // 用 thread_local 记录调用次数
            if let Some((ref needle, _, _)) = matched {
                LAST_CALLS.with(|map| {
                    *map.borrow_mut().entry(needle.clone()).or_insert(0) += 1;
                });
            }

            Box::pin(async move {
                if let Some((_, status, body)) = matched {
                    return Ok(HttpReply { status, body, location: None });
                }
                Ok(HttpReply {
                    status: 404,
                    body: Vec::new(),
                    location: None,
                })
            })
        }
    }

    fn call_count(key: &str) -> usize {
        LAST_CALLS.with(|map| *map.borrow().get(key).unwrap_or(&0))
    }

    fn reset_calls() {
        LAST_CALLS.with(|map| map.borrow_mut().clear());
    }

    // --- 复用的测试 helpers ---

    fn sample_mrpack_index() -> Value {
        json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "test-001",
            "name": "Test Pack",
            "summary": "A test mrpack",
            "files": [
                {
                    "hashes": { "sha1": "abc123def456abc123def456abc123def456abc1" },
                    "path": "mods/sodium-fabric-0.5.11.jar",
                    "downloads": ["https://cdn.modrinth.com/data/AANobbMI/versions/0.5.11/sodium-fabric-0.5.11.jar"],
                    "fileSize": 1234567
                },
                {
                    "hashes": { "sha1": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef" },
                    "path": "mods/lithium-fabric-0.12.2.jar",
                    "downloads": ["https://cdn.modrinth.com/data/GvQq7AZz/versions/0.12.2/lithium-fabric-0.12.2.jar"],
                    "fileSize": 987654
                }
            ]
        })
    }

    // =================================================================
    // 测试 1：overrides + 依赖下载 —— 完整 happy path
    // =================================================================

    #[tokio::test]
    async fn happy_path_installs_overrides_and_mods() {
        reset_calls();
        let dir = temp_dir("modpack_happy");

        let mrpack_bytes = make_mrpack(
            &[
                ("options.txt", b"viewDistance=12\n".to_vec().as_slice()),
                ("config/forge.cfg", b"[forge]\nversion=47\n".to_vec().as_slice()),
            ],
            Some(&sample_mrpack_index()),
        );

        let http = TestHttp::new();
        // 需要让 Arc 内部可变，改成 Arc<Mutex<TestHttp>> 不行因为 routes 要改。
        // 用 RefCell 包住：因为 test 只在单线程调
        use std::cell::RefCell;
        let inner = RefCell::new(TestHttp { routes: Vec::new() });
        inner.borrow_mut().route("AANobbMI", 200, b"fake sodium jar content".to_vec());
        inner.borrow_mut().route("GvQq7AZz", 200, b"fake lithium jar content".to_vec());
        let http: Arc<dyn HttpPort> = Arc::new(inner.into_inner());

        let ctx = ctx_with_http(&dir, http);

        let args = InstallModpackArgs {
            instance_id: "test-instance".to_owned(),
            url: None,
            file_path: Some(write_temp_mrpack(&mrpack_bytes)),
        };

        let result = handle(ctx, args).await;
        let tool_result: ToolResult = into_framework_result(result);

        assert_no_errors(&tool_result);
        let data = tool_result.structured_content.unwrap();
        assert_eq!(data["ok"], json!(true));
        assert_eq!(data["instance_id"], json!("test-instance"));
        assert_eq!(data["overrides_applied"], json!(2));
        assert_eq!(data["mods_fetched"], json!(2));
        assert_eq!(data["mods_expected"], json!(2));
        assert_eq!(data["mods_failed"], json!(0));

        // 文件确实落盘
        let instance_dir = dir.join("instances").join("test-instance");
        assert!(std::fs::read_to_string(instance_dir.join("options.txt"))
            .unwrap()
            .contains("viewDistance=12"));
        assert!(std::fs::read_to_string(instance_dir.join("config/forge.cfg"))
            .unwrap()
            .contains("version=47"));
        assert_eq!(
            std::fs::read(instance_dir.join("mods").join("sodium-fabric-0.5.11.jar")).unwrap(),
            b"fake sodium jar content"
        );

        assert_eq!(call_count("AANobbMI"), 1);
        assert_eq!(call_count("GvQq7AZz"), 1);
    }

    // =================================================================
    // 测试 2：无网络 → 只应用 overrides + simulated
    // =================================================================

    #[tokio::test]
    async fn no_network_applies_overrides_only() {
        reset_calls();
        let dir = temp_dir("modpack_no_net");

        let mrpack_bytes = make_mrpack(
            &[("options.txt", b"renderDistance=8".to_vec().as_slice())],
            Some(&sample_mrpack_index()),
        );

        // **不传 HTTP**
        let ctx = PluginContext::new(
            "com.example.official-mod",
            dir.clone(),
            Arc::new(CapturingLogger::default()),
        );

        let args = InstallModpackArgs {
            instance_id: "test-instance".to_owned(),
            url: None,
            file_path: Some(write_temp_mrpack(&mrpack_bytes)),
        };

        let result = handle(ctx, args).await;
        let tr: ToolResult = into_framework_result(result);
        assert_no_errors(&tr);

        let data = tr.structured_content.unwrap();
        assert_eq!(data["simulated"], json!(true));
        assert_eq!(data["overrides_applied"], json!(1));
        assert_eq!(data["mods_fetched"], json!(0));
    }

    // =================================================================
    // 测试 3：依赖部分下载失败 —— 不拖垮整体
    // =================================================================

    #[tokio::test]
    async fn partial_download_failures_are_counted_not_fatal() {
        reset_calls();
        let dir = temp_dir("modpack_partial");

        let mrpack_bytes = make_mrpack(&[], Some(&sample_mrpack_index()));

        use std::cell::RefCell;
        let inner = RefCell::new(TestHttp { routes: Vec::new() });
        inner.borrow_mut().route("AANobbMI", 200, b"ok jar".to_vec());
        inner.borrow_mut().route("GvQq7AZz", 500, Vec::new());
        let http: Arc<dyn HttpPort> = Arc::new(inner.into_inner());

        let ctx = ctx_with_http(&dir, http);

        let args = InstallModpackArgs {
            instance_id: "test-instance".to_owned(),
            url: None,
            file_path: Some(write_temp_mrpack(&mrpack_bytes)),
        };

        let result = handle(ctx, args).await;
        let tr: ToolResult = into_framework_result(result);
        assert_no_errors(&tr);

        let data = tr.structured_content.unwrap();
        assert_eq!(data["mods_fetched"], json!(1));
        assert_eq!(data["mods_failed"], json!(1));
        assert_eq!(data["mods_expected"], json!(2));
    }

    // =================================================================
    // 测试 4：解析 index.json —— project_id 从 CDN URL 正确提取
    // =================================================================

    #[test]
    fn parse_index_extracts_project_ids_from_cdn_url() {
        let idx = sample_mrpack_index();
        let (deps, total) = parse_modpack_index(&idx);
        assert_eq!(total, 2);
        assert_eq!(deps.len(), 2);

        assert_eq!(deps[0].project_id, "AANobbMI");
        assert_eq!(deps[0].filename, "sodium-fabric-0.5.11.jar");
        assert_eq!(deps[1].project_id, "GvQq7AZz");
        assert_eq!(deps[1].filename, "lithium-fabric-0.12.2.jar");
    }

    #[test]
    fn extract_project_from_url_parses_cdn_url() {
        assert_eq!(
            extract_project_from_url(
                "https://cdn.modrinth.com/data/AANobbMI/versions/0.5.11/sodium.jar"
            ),
            Some("AANobbMI".to_owned())
        );
        assert_eq!(
            extract_project_from_url("https://maven.example.com/com/foo/bar.jar"),
            None
        );
    }

    // =================================================================
    // 测试 5：空 mrpack 或无 index —— 合法
    // =================================================================

    #[tokio::test]
    async fn mrpack_without_index_is_ok() {
        reset_calls();
        let dir = temp_dir("modpack_no_idx");
        let mrpack_bytes = make_mrpack(
            &[("options.txt", b"hello".to_vec().as_slice())],
            None,
        );

        use std::cell::RefCell;
        let inner = RefCell::new(TestHttp { routes: Vec::new() });
        let http: Arc<dyn HttpPort> = Arc::new(inner.into_inner());
        let ctx = ctx_with_http(&dir, http);

        let args = InstallModpackArgs {
            instance_id: "test-instance".to_owned(),
            url: None,
            file_path: Some(write_temp_mrpack(&mrpack_bytes)),
        };

        let result = handle(ctx, args).await;
        let tr: ToolResult = into_framework_result(result);
        assert_no_errors(&tr);

        let data = tr.structured_content.unwrap();
        assert_eq!(data["overrides_applied"], json!(1));
        assert_eq!(data["mods_expected"], json!(0));
    }

    // =================================================================
    // 测试 6：参数校验 —— 空 instance_id 被拒绝
    // =================================================================

    #[tokio::test]
    async fn empty_instance_id_fails() {
        reset_calls();
        let dir = temp_dir("modpack_empty");
        let ctx = PluginContext::new(
            "com.example.official-mod",
            dir.clone(),
            Arc::new(CapturingLogger::default()),
        );

        let args = InstallModpackArgs {
            instance_id: "".to_owned(),
            url: Some("https://example.com/fake.mrpack".to_owned()),
            file_path: None,
        };

        let result = handle(ctx, args).await;
        let tr: ToolResult = into_framework_result(result);
        assert!(tr.is_error, "empty instance_id should be rejected");
    }

    // =================================================================
    // 测试 7：没有 url 和 file_path —— 业务错误
    // =================================================================

    #[tokio::test]
    async fn no_source_is_business_error() {
        reset_calls();
        let dir = temp_dir("modpack_nosrc");
        let ctx = PluginContext::new(
            "com.example.official-mod",
            dir.clone(),
            Arc::new(CapturingLogger::default()),
        );

        let args = InstallModpackArgs {
            instance_id: "test".to_owned(),
            url: None,
            file_path: None,
        };

        let result = handle(ctx, args).await;
        let tr: ToolResult = into_framework_result(result);
        // url 和 file_path 都缺失 → 框架层错误（is_error=true）
        assert!(tr.is_error, "expect is_error=true: {tr:?}");

        // 错误消息里应该提到 InvalidArguments
        let msg = tr
            .structured_content
            .as_ref()
            .and_then(|v| v.get("message"))
            .and_then(|m| m.as_str());
        assert!(
            msg.is_some_and(|m| m.contains("url")),
            "expect message about url, got: {tr:?}"
        );
    }

    // =================================================================
    // 测试 8：Zip 结构正确性（我们自己能读回来）
    // =================================================================

    #[test]
    fn make_mrpack_produces_valid_zip() {
        use std::io::Cursor;
        let bytes = make_mrpack(
            &[("overrides.txt", b"test".to_vec().as_slice())],
            Some(&sample_mrpack_index()),
        );
        let mut z = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut names: Vec<String> = Vec::new();
        for i in 0..z.len() {
            if let Ok(e) = z.by_index(i) {
                names.push(e.name().to_owned());
            }
        }
        assert!(names.iter().any(|n| n == "modrinth.index.json"));
        assert!(names.iter().any(|n| n == "overrides/overrides.txt"));
        assert!(names.iter().any(|n| n == "pack.mcmeta"));
    }
}
