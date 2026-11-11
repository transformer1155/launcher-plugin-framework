//! `download_java`：从 Adoptium（Temurin）下载 JDK 并解压到插件数据目录。
//!
//! 流程：构造 Adoptium 二进制端点 URL（会 302 到 github，宿主 HTTP 实现已
//! 自动跟随重定向）→ 下载 zip 到数据目录 → 用系统自带 `tar.exe` 解压
//! （Windows 10 1803+ 内置 bsdtar，支持 zip，免去引入 zip 解码依赖）→
//! 找到解压根目录里的 `bin/java.exe` → 运行 `java -version` 读取版本号。
//!
//! # 为什么走 `tar.exe` 而不是手写 zip 解码
//!
//! zip 的 deflate 解码要么依赖第三方 crate（本机编不过 ring 系），要么手写
//! inflate（几百行且难测对）。系统 `tar -xf` 一行解决，且官方支持。

use launcher_plugin_common::result::{self, biz_codes};
use launcher_plugin_common::tool_args;
use launcher_plugin_common::PluginContext;
use launcher_plugin_framework::types::ToolResult;
use launcher_plugin_framework::error::ToolError;
use serde_json::json;

tool_args! {
    /// 下载参数。
    pub struct DownloadJavaArgs {
        major: o_u64() = "要下载的 JDK 大版本号（17 / 21 / 25…），缺省为 21",
    }
}

/// JDK 安装根目录（插件数据目录下的 `java/`）。
fn java_root(ctx: &PluginContext) -> std::path::PathBuf {
    ctx.data_dir().join("java")
}

/// 已存在的 JDK 目录名。
fn target_dir(ctx: &PluginContext, major: u32) -> std::path::PathBuf {
    java_root(ctx).join(format!("jdk-{major}"))
}

/// 在 `dir` 里找包含 `bin/java.exe` 的目录（解压根）。
fn find_java_home(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    if dir.join("bin").join("java.exe").is_file() {
        return Some(dir.to_path_buf());
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_java_home(&p) {
                return Some(found);
            }
        }
    }
    None
}

/// 运行 `java -version`，从 stderr 里抠出版本串（如 `21.0.5`）。
fn probe_version(java_exe: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new(java_exe)
        .arg("-version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    // 形如：openjdk version "21.0.5" 2024-10-15 LTS
    let line = text.lines().next()?;
    line.split('"').nth(1).map(|s| s.to_owned())
}

/// 业务失败（is_error 结构化结果）。
fn fail(code: &str, msg: impl Into<String>) -> Result<ToolResult, ToolError> {
    Ok(result::business(code, msg))
}

pub async fn handle(
    ctx: PluginContext,
    args: DownloadJavaArgs,
) -> Result<ToolResult, ToolError> {
    let major = args.major.filter(|m| *m > 0).unwrap_or(21) as u32;
    let dest = target_dir(&ctx, major);
    if dest.exists() {
        return fail(
            biz_codes::ALREADY_EXISTS,
            format!("JDK {major} 已下载过：{}", dest.display()),
        );
    }

    // ---- 下载（宿主必须注入 HttpPort；LiveDataSource 已注入带白名单的实现）----
    let Ok(http) = ctx.http() else {
        return fail(
            biz_codes::UPSTREAM_ERROR,
            "网络能力不可用（宿主未注入 HTTP 端口）",
        );
    };
    let url = format!(
        "https://api.adoptium.net/v3/binary/latest/{major}/windows/x64/jdk/hotspot/normal/eclipse"
    );
    let reply = match http.get(url.clone()).await {
        Ok(r) => r,
        Err(e) => return fail(biz_codes::UPSTREAM_ERROR, format!("下载失败：{e}")),
    };
    if reply.status != 200 {
        return fail(
            biz_codes::UPSTREAM_ERROR,
            format!("下载失败：HTTP {}（{url}）", reply.status),
        );
    }
    if reply.body.len() < 1_000_000 {
        return fail(
            biz_codes::UPSTREAM_ERROR,
            format!("下载内容过小（{} 字节），不是有效的 JDK 压缩包", reply.body.len()),
        );
    }

    // ---- 落盘 ----
    let root = java_root(&ctx);
    if let Err(e) = std::fs::create_dir_all(&root) {
        return fail(biz_codes::STORAGE_ERROR, format!("创建目录失败：{e}"));
    }
    let zip = root.join(format!("jdk-{major}.zip"));
    if let Err(e) = std::fs::write(&zip, &reply.body) {
        return fail(biz_codes::STORAGE_ERROR, format!("写入 zip 失败：{e}"));
    }

    // ---- 解压（系统 tar.exe）----
    let extract_to = root.join(format!("_extract-{major}"));
    let _ = std::fs::remove_dir_all(&extract_to);
    if let Err(e) = std::fs::create_dir_all(&extract_to) {
        return fail(
            biz_codes::STORAGE_ERROR,
            format!("创建解压目录失败：{e}"),
        );
    }
    let tar = std::process::Command::new("tar")
        .args(["-xf"])
        .arg(&zip)
        .arg("-C")
        .arg(&extract_to)
        .output();
    let Ok(out) = tar else {
        return fail(
            biz_codes::LAUNCH_FAILED,
            "无法运行 tar.exe（解压需要 Windows 10 1803+ 自带的 bsdtar）",
        );
    };
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        return fail(
            biz_codes::LAUNCH_FAILED,
            format!("解压失败：{}", msg.trim()),
        );
    }

    // ---- 定位 java.exe 并归位 ----
    let Some(home) = find_java_home(&extract_to) else {
        return fail(
            biz_codes::STORAGE_ERROR,
            "解压完成但没找到 bin/java.exe".to_owned(),
        );
    };
    let _ = std::fs::remove_dir_all(&dest);
    if let Err(e) = std::fs::rename(&home, &dest) {
        return fail(
            biz_codes::STORAGE_ERROR,
            format!("归位 JDK 目录失败：{e}"),
        );
    }
    let _ = std::fs::remove_dir_all(&extract_to);
    let _ = std::fs::remove_file(&zip);

    let java_exe = dest.join("bin").join("java.exe");
    let version = probe_version(&java_exe).unwrap_or_else(|| format!("{major}（版本探测失败）"));

    Ok(result::success(json!({
        "major": major,
        "version": version,
        "path": dest.display().to_string(),
        "bytes": reply.body.len(),
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    use launcher_plugin_common::tool_def;
    tool_def! {
        name: "download_java",
        description: "从 Adoptium 下载指定大版本的 JDK（真实 HTTPS 下载 + 系统tar解压）",
        destructive: false,
        timeout_ms: 900_000,
        args: DownloadJavaArgs,
        handler: crate::tools::download_java::handle
    }
}
