//! **完整 UI 启动流程**端到端测试（需要真实网络，`#[ignore]`）。
//!
//! 与 `official-launch/tests/mc_e2e.rs`（工具级）不同：本测试通过
//! [`launcher_ui::live::LiveDataSource`]——UI 实际使用的数据源——按 UI 的
//! 真实调用顺序走完全流程：
//!
//! 1. `create_instance`（版本页确认安装的第一步）
//! 2. `install_minecraft`（真实下载客户端 + 库，复用缓存）
//! 3. `launch_readiness`（启动前就绪检查，未安装应拒绝）
//! 4. `set_launch_config`（用户三项：JVM / 游戏参数 / 内存）
//! 5. `launch_game`（真实 spawn）
//! 6. 轮询存活 + 收尾
//!
//! 沙箱 TLS 被 MITM，网络由 curl 子进程承载（`-k` 仅测试用）；
//! 数据目录复用 mc_e2e 缓存，重复运行不会重新下载大文件。

use launcher_plugin_common::context::HttpPort;
use std::path::Path;
use std::sync::Arc;

/// 用 curl 实现 GET（跟随重定向；先取状态码，再取响应体）。
struct CurlHttp;

impl HttpPort for CurlHttp {
    fn get(
        &self,
        url: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<launcher_plugin_common::context::HttpReply, String>> + Send>,
    > {
        Box::pin(async move {
            let status = std::process::Command::new("curl")
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

// 注意：必须是同步测试——`LiveDataSource` 内部持有自己的 tokio runtime 并
// 用 `block_on` 调用插件（与 UI 线程一致），不能再嵌进 tokio 测试 runtime。
#[test]
#[ignore = "需要真实网络与本机桌面（完整 UI 流程：安装 → 就绪检查 → 启动）"]
fn full_ui_flow_install_then_launch() {
    // 与 official-launch 的 mc_e2e 测试共享同一数据目录（下载缓存 + JDK）。
    let plugins_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/target/tmp/mc-e2e");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    eprintln!("== 插件目录：{}", plugins_dir.display());

    let live = launcher_ui::live::LiveDataSource::with_http(plugins_dir.clone(), Arc::new(CurlHttp))
        .expect("LiveDataSource 构建失败");

    // ---- 1. UI「确认安装」第一步：创建实例（时间戳命名，可重复运行）----
    let name = format!("UI流程{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs());
    let inst_id = live
        .create_instance(&name, "1.21.1", "vanilla")
        .expect("创建实例失败");
    eprintln!("== 实例已创建：{inst_id}");

    // ---- 2. 安装前就绪检查：应识别为未装配（这正是 UI 启动前的新守卫）----
    let (kind, java) = live.launch_readiness(&inst_id).expect("readiness 查询失败");
    eprintln!("== 安装前：entry_kind={kind}，java={java:?}");
    assert_eq!(kind, "none", "未安装时应为 none");

    // ---- 3. UI「确认安装」第二步：真实下载 Minecraft（缓存命中则秒回）----
    let info = live
        .install_minecraft(&inst_id, "1.21.1")
        .expect("install_minecraft 失败");
    let libs = info.get("libraries").and_then(|x| x.as_u64()).unwrap_or(0);
    eprintln!("== 安装完成：{libs} 个库");

    // ---- 4. 安装后就绪检查：入口就绪 + Java 可用 ----
    let (kind, java) = live.launch_readiness(&inst_id).expect("readiness 查询失败");
    eprintln!("== 安装后：entry_kind={kind}，java={java:?}");
    assert_eq!(kind, "main_class", "安装后入口应为 main_class");
    let java_path = java.expect("应自动挂载已下载的 JDK");
    assert!(Path::new(&java_path).is_file(), "java 应存在：{java_path}");

    // ---- 5. UI 启动：先 set_launch_config（用户三项）再 launch_game ----
    live.set_launch_config(&inst_id, &["-XX:+UseG1GC".to_owned()], &[], 4096)
        .expect("set_launch_config 失败");
    let pid = live.launch_game(&inst_id).expect("launch_game 失败");
    eprintln!("== 已 spawn java，PID {pid}");

    // 确认 set_launch_config 没有把真实入口重置掉（历史 bug）。
    let (kind, _) = live.launch_readiness(&inst_id).expect("readiness 查询失败");
    assert_eq!(kind, "main_class", "设置启动参数后入口不得被重置");

    // ---- 6. 轮询存活：150 秒仍在运行 = 成功进入主菜单 ----
    let mut outcome = String::from("timeout-still-running");
    for i in 0..50 {
        std::thread::sleep(std::time::Duration::from_secs(3));
        match live.launch_status() {
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
    live.kill_game();

    // 游戏日志尾部（便于诊断）
    let log = plugins_dir
        .join("official-launch")
        .join("instances")
        .join(&inst_id)
        .join("logs")
        .join("latest.log");
    if let Ok(text) = std::fs::read_to_string(&log) {
        let tail: Vec<&str> = text.lines().rev().take(30).collect();
        eprintln!(
            "== 游戏日志尾部：\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
    }

    eprintln!("== 结果：{outcome}");
    assert!(
        outcome == "running" || outcome == "timeout-still-running",
        "Minecraft 进程应在启动后保持运行（实际：{outcome}）"
    );
}
