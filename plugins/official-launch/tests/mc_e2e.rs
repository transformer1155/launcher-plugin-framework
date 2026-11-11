//! Minecraft 真实启动端到端测试（需要真实网络，`#[ignore]`）。
//!
//! 运行：`cargo test -p official-launch --test mc_e2e -- --ignored --nocapture`
//!
//! 沙箱 TLS 会被中间人拦截（rustls 不认 MITM 证书），因此用 **curl 子进程**
//! 实现 `HttpPort`——`install_minecraft` 的全部装配逻辑真实运行，网络由 curl 承载。
//! 测试步骤：安装 1.21.1 → 校验产物 → `launch_game` 真实 spawn → 轮询存活 → 收尾。

use std::sync::Arc;

use launcher_plugin_common::context::{HttpPort, PluginContext, PluginLogger};
use launcher_plugin_common::NetworkDecl;
use official_launch::store::{kill_child, launch_status, LaunchEntry, LaunchIndex};

/// 用 curl 实现 GET（跟随重定向；一次调用同时取状态码与响应体）。
struct CurlHttp;

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl HttpPort for CurlHttp {
    fn get(
        &self,
        url: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<launcher_plugin_common::context::HttpReply, String>> + Send>>
    {
        Box::pin(async move {
            let status = std::process::Command::new("curl")
                // -k：沙箱 TLS 被中间人拦截，测试环境放行（仅测试用）。
                .args(["-skL", "-o", "NUL", "-w", "%{http_code}", "--max-time", "60"])
                .arg(&url)
                .output()
                .map_err(|e| format!("curl 启动失败：{e}"))?;
            let status: u16 = String::from_utf8_lossy(&status.stdout)
                .trim()
                .parse()
                .map_err(|e| format!("curl 状态码解析失败：{e}"))?;
            if status != 200 {
                return Ok(launcher_plugin_common::context::HttpReply {
                    status,
                    body: Vec::new(),
                    location: None,
                });
            }
            let body = std::process::Command::new("curl")
                .args(["-skL", "--max-time", "900"])
                .arg(&url)
                .output()
                .map_err(|e| format!("curl 启动失败：{e}"))?;
            Ok(launcher_plugin_common::context::HttpReply {
                status,
                body: body.stdout,
                location: None,
            })
        })
    }
}

struct QuietLogger;

impl PluginLogger for QuietLogger {
    fn log(&self, level: launcher_plugin_common::LogLevel, plugin_id: &str, message: &str) {
        eprintln!("[{level:?}] {plugin_id}: {message}");
    }
}

fn data_dir() -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("mc-e2e");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn count_files_recursive(root: &std::path::Path) -> usize {
    let mut n = 0;
    if let Ok(it) = std::fs::read_dir(root) {
        for e in it.flatten() {
            let p = e.path();
            if p.is_dir() {
                n += count_files_recursive(&p);
            } else {
                n += 1;
            }
        }
    }
    n
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

#[tokio::test]
#[ignore = "需要真实网络与本机桌面（真实下载 + 真实启动 Minecraft）"]
async fn install_then_launch_real_minecraft() {
    let dir = data_dir();
    eprintln!("== 数据目录：{}", dir.display());

    let decl = NetworkDecl {
        allow_hosts: vec![
            "piston-meta.mojang.com".to_owned(),
            "piston-data.mojang.com".to_owned(),
            "launchermeta.mojang.com".to_owned(),
            "libraries.minecraft.net".to_owned(),
            "resources.download.minecraft.net".to_owned(),
        ],
        reason: "测试".to_owned(),
    };
    let http = Arc::new(launcher_plugin_common::AllowListedHttp::new(
        Arc::new(CurlHttp),
        &decl,
    ));
    let ctx = PluginContext::new(official_launch::PLUGIN_ID, dir.clone(), Arc::new(QuietLogger))
        .with_http(http);

    // ---- 1. 安装 1.21.1（重复运行时已下载的文件会被跳过）----
    let args = official_launch::tools::install_minecraft::InstallMinecraftArgs {
        instance_id: "mc-e2e".to_owned(),
        version: Some("1.21.1".to_owned()),
    };
    let result = official_launch::tools::install_minecraft::handle(ctx.clone(), args)
        .await
        .expect("handler 不应返回 ToolError");
    let tr = result; // handle 已返回 ToolResult
    assert!(!tr.is_error, "install_minecraft 失败：{tr:?}");
    let info = tr.structured_content.clone().unwrap();
    eprintln!("== 安装结果：{info}");
    assert!(info.get("client_bytes").and_then(|x| x.as_u64()).unwrap_or(0) > 10_000_000,
        "客户端 jar 应大于 10MB");
    assert!(info.get("libraries").and_then(|x| x.as_u64()).unwrap_or(0) > 40,
        "应有几十个库");

    // ---- 2. 启动配置应已就绪 ----
    let store = LaunchIndex::new(&dir).load().unwrap();
    let config = store.configs.get("mc-e2e").expect("应有 mc-e2e 配置");
    let LaunchEntry::MainClass { class, classpath } = &config.entry else {
        panic!("入口应为 MainClass，实际 {:?}", config.entry);
    };
    assert!(class.contains("net.minecraft.client.main.Main"), "主类：{class}");
    assert!(classpath.matches(';').count() > 40, "classpath 应包含几十个库");
    assert!(!config.mc_args.is_empty(), "应有标准游戏参数");
    // natives：LWJGL 等的 Windows DLL 必须解压到位，否则游戏无法创建窗口。
    let natives = config
        .natives_dir
        .clone()
        .expect("应写入 natives_dir");
    let dll_count = std::fs::read_dir(&natives)
        .map(|it| it.filter_map(|e| e.ok()).filter(|e| e.path().extension().map(|x| x == "dll").unwrap_or(false)).count())
        .unwrap_or(0);
    assert!(dll_count >= 5, "natives 目录应有至少 5 个 dll（实际 {dll_count}）：{natives}");
    eprintln!("== natives：{natives}（{dll_count} 个 dll）");
    // 资源对象：验证下载管线可用即可（curl 逐文件太慢，不要求全量 4000）。
    // 全量补齐由真实应用的 HttpsPort 完成（原生 TLS，速度远快于 curl 子进程）。
    let obj_count = count_files_recursive(&dir.join("assets").join("objects"));
    assert!(obj_count > 100, "资源对象下载管线应可用（实际 {obj_count} 个）");
    eprintln!("== 资源对象：{obj_count} 个（管线验证通过）");
    let java = config
        .java_path
        .clone()
        .expect("应自动找到已下载的 JDK（先运行 download_java 或手动放置）");
    assert!(std::path::Path::new(&java).is_file(), "java 应存在：{java}");
    eprintln!("== 主类：{class}");
    eprintln!("== classpath 长度：{}", classpath.len());
    eprintln!("== java：{java}");

    // ---- 3. 真实 spawn（本机桌面会弹出 Minecraft 窗口）----
    let largs = official_launch::tools::launch_game::LaunchGameArgs {
        instance_id: "mc-e2e".to_owned(),
    };
    let result = official_launch::tools::launch_game::handle(ctx, largs)
        .await
        .expect("launch handler 不应返回 ToolError");
    let tr = result;
    assert!(!tr.is_error, "launch_game 失败：{tr:?}");
    let pid = tr
        .structured_content
        .as_ref()
        .and_then(|s| s.get("pid"))
        .and_then(|p| p.as_u64())
        .expect("应有真实 PID");
    eprintln!("== 已 spawn java，PID {pid}，等待启动…");

    // ---- 4. 轮询：150 秒内仍在运行 = 启动成功（MC 正常要几十秒进到主界面）----
    let mut outcome = String::from("timeout-still-running");
    for i in 0..50 {
        std::thread::sleep(std::time::Duration::from_secs(3));
        match launch_status() {
            None => {
                if i > 5 {
                    outcome = "running".to_owned();
                    break;
                }
            }
            Some(code) => {
                outcome = format!("exited-code-{code}");
                break;
            }
        }
    }
    kill_child();

    // 游戏日志（无论成败都打出来，便于诊断）
    let log = dir.join("instances").join("mc-e2e").join("logs").join("latest.log");
    if let Ok(text) = std::fs::read_to_string(&log) {
        let tail: Vec<&str> = text.lines().rev().take(40).collect();
        eprintln!("== 游戏日志尾部：\n{}", tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
    }

    eprintln!("== 结果：{outcome}");
    assert!(
        outcome == "running" || outcome == "timeout-still-running",
        "Minecraft 进程应在启动后保持运行（实际：{outcome}）"
    );
}
