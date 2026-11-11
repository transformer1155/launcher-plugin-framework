//! `launch_game`：启动游戏（**真实 spawn Java 进程**）。
//!
//! # 与早期"模拟启动"的区别
//!
//! 早期版本只返回 `pid: null` + `simulated: true`，不创建任何 OS 进程
//! （当时框架约束"单进程、无子进程"）。现已按要求改为**真实 spawn**：
//! 本工具用 `std::process::Command` 启动一个真实的 Java 子进程，
//! 返回**真实 PID**，UI 再通过 [`crate::store::launch_status`] 轮询**真实退出码**。
//!
//! # 入口（entry）
//!
//! 启动配置里的 [`crate::store::LaunchEntry`] 决定 `java` 之后拼什么：
//! - `None` → 探测式：`java -version`（真实进程、exit 0，用于验证 spawn 链路）
//! - `Jar(path)` → `java [args] -jar <path>`
//! - `MainClass{class,classpath}` → `java [args] -cp <cp> <class>`
//! - `Raw(args)` → 直接追加原始参数
//!
//! 真正进游戏需要指向真实客户端 jar / 主类，这部分由业务层装配（不在本插件内）。

use crate::load_launch_store;
use crate::store::{
    log_path_for, now_rfc3339, store_child, validate_instance_id, LaunchConfig, LaunchEntry,
};
use launcher_plugin_common::{biz_fail, tool_args, tool_def};
use launcher_plugin_framework::error::ToolErrorCode;
use serde_json::json;
use std::path::Path;
use std::process::Command;

tool_args! {
    pub struct LaunchGameArgs {
        instance_id: r_str() = "实例 ID",
    }
}

/// 把入口解析成 `java [jvm_args]` 之后要追加的参数（不含 jvm_args / -Xmx）。
fn entry_args(entry: &LaunchEntry, game_args: &[String]) -> Vec<String> {
    match entry {
        LaunchEntry::None => vec!["-version".to_owned()],
        LaunchEntry::Jar(path) => {
            let mut v = vec!["-jar".to_owned(), path.clone()];
            v.extend(game_args.iter().cloned());
            v
        }
        LaunchEntry::MainClass { class, classpath } => {
            let mut v = vec!["-cp".to_owned(), classpath.clone(), class.clone()];
            v.extend(game_args.iter().cloned());
            v
        }
        LaunchEntry::Raw(args) => {
            let mut v = args.clone();
            v.extend(game_args.iter().cloned());
            v
        }
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

    // ---- 解析 Java 可执行文件：优先配置里的绝对路径，否则回退到 PATH 上的 `java` ----
    let mut java = config
        .java_path
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "java".to_owned());
    if java != "java" && !Path::new(&java).exists() {
        ctx.log_warn(format!(
            "配置的 java_path 不存在（{java}），回退到 PATH 上的 `java`"
        ));
        java = "java".to_owned();
    }

    let mut cmd = Command::new(&java);
    // Windows：不弹出 Java 自带的控制台黑窗，游戏输出走管道进启动器日志。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // 捕获输出：后台线程读出后写入游戏日志环形缓冲，UI 拉取显示。
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    if config.memory_mb > 0 {
        cmd.arg(format!("-Xmx{}m", config.memory_mb));
    }
    // 启动器注入的系统属性（必须在 -cp 之前）：
    // 1) 临时目录改到游戏目录下——系统 TEMP 被杀软拦"拒绝访问"时
    //    JNA 解压 jnidispatch.dll 会失败（oshi 报错、材质检测全挂）；
    // 2) natives 目录——LWJGL/glfw 的 dll 由 install_minecraft 解压到这里，
    //    缺了会报 "Failed to locate library: lwjgl.dll"，窗口创建失败。
    let game_tmp = ctx
        .data_dir()
        .join("instances")
        .join(&instance_id)
        .join("tmp");
    let _ = std::fs::create_dir_all(&game_tmp);
    cmd.arg(format!("-Djava.io.tmpdir={}", game_tmp.display()));
    cmd.arg(format!("-Djna.tmpdir={}", game_tmp.display()));
    if let Some(natives) = config
        .natives_dir
        .as_deref()
        .filter(|s| Path::new(s).is_dir())
    {
        cmd.arg(format!("-Dorg.lwjgl.librarypath={natives}"));
        cmd.arg(format!("-Djava.library.path={natives}"));
    }
    for a in &config.jvm_args {
        cmd.arg(a);
    }
    for a in entry_args(&config.entry, &config.game_args) {
        cmd.arg(a);
    }
    // Minecraft 标准参数（--gameDir 等，由 install_minecraft 写入）排在用户自定义之后。
    for a in &config.mc_args {
        cmd.arg(a);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            ctx.log_error(format!("spawn Java 失败：{e}"));
            return Ok(launcher_plugin_common::result::business(
                launcher_plugin_common::result::biz_codes::LAUNCH_FAILED,
                format!("无法启动 Java 进程（{java}）：{e}"),
            ));
        }
    };
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let pid = store_child(child);

    // ---- 写一行真实日志（不再是 [simulated]）----
    let log_path = ctx.data_path(&log_path_for(&instance_id))?;
    if let Some(parent) = log_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            ctx.log_error(format!("创建日志目录失败：{e}"));
        }
    }
    let ts = now_rfc3339();
    let line = format!(
        "[{ts}] [launch] spawning java (pid={pid}) for instance {instance_id} \
         (java={java}, memory={}MB, jvm_args={}, entry={:?})\n",
        config.memory_mb,
        config.jvm_args.len(),
        config.entry
    );
    let _ = append_line(&log_path, &line);

    // ---- 后台线程：把游戏输出转发到日志文件 + 环形缓冲（UI 拉取） ----
    for (name, pipe) in [
        ("stdout", out_pipe.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)),
        ("stderr", err_pipe.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)),
    ] {
        let Some(pipe) = pipe else { continue };
        let log_path = log_path.clone();
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let reader = BufReader::new(pipe);
            for line in reader.lines().map_while(Result::ok) {
                let _ = append_line(&log_path, &format!("{line}\n"));
                crate::store::push_game_log(format!("[MC/{name}] {line}"));
            }
            crate::store::push_game_log(format!("[MC/{name}] 输出流已结束（进程可能已退出）"));
        });
    }

    ctx.log_info(format!(
        "[launch] 已启动实例 {instance_id}（pid={pid}，java={java}）—— 真实 OS 进程已创建"
    ));

    Ok(launcher_plugin_common::result::success(json!({
        "pid": pid,
        "simulated": false,
        "instance_id": instance_id,
        "java": java,
        "memory_mb": config.memory_mb,
        "note": "真实启动：已创建 OS 子进程。退出码由 UI 通过 launch_status 轮询。",
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
        description: "启动游戏（真实 spawn Java 进程，返回真实 PID）",
        destructive: true,
        timeout_ms: 10_000,
        args: LaunchGameArgs,
        handler: crate::tools::launch_game::handle
    }
}
