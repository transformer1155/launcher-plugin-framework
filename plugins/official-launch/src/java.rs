//! 本机 Java 运行时探测。
//!
//! 扫描 PATH、`JAVA_HOME`、常见安装目录、Windows 注册表与 Minecraft 自带 runtime，
//! 对每个候选执行 `java -version`（输出在 stderr）解析出版本 / 厂商 / 位数。
//! 解析失败或执行失败（权限、不存在）的候选会被跳过，不影响其它候选。
//!
//! 这是"真实启动"的前置能力：没有可用的 `java`，`launch_game` 就只能返回 `launch_failed`。
//! 探测结果会被 UI 用来填充 Java 选择下拉框，并在首次引导里自动挑选。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 探测到的一个 Java 运行时。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DetectedJava {
    /// `java` 可执行文件的绝对路径。
    pub path: String,
    /// 该 java 所属的 JAVA_HOME（`bin/java` 的父目录）。
    pub home: String,
    /// 版本号，如 `17.0.9`、`1.8.0_391`。
    pub version: String,
    /// 厂商，如 `Eclipse Adoptium`、`Oracle`、`Azul Zulu`、`Amazon Corretto`。
    pub vendor: String,
    /// 架构：`x64` / `x86` / `aarch64` / ``（未知）。
    pub arch: String,
}

impl DetectedJava {
    /// 给 UI 用的展示名：`厂商 版本 (arch)`。
    pub fn display(&self) -> String {
        let arch = if self.arch.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.arch)
        };
        format!("{} {}{}", self.vendor, self.version, arch)
            .trim()
            .to_owned()
    }
}

/// 默认扫描根目录。
///
/// 不含对 PATH 的逐目录拆解（[`find_java_installations`] 会自行加入 PATH 各目录），
/// 这样测试可以只注入临时目录而不触发整盘扫描。
fn default_search_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();

    // 1) 常见安装目录（Windows）。
    if let Some(pf) = env_var_path("ProgramFiles") {
        roots.push(pf.join("Java"));
        roots.push(pf.join("Eclipse Adoptium"));
        roots.push(pf.join("AdoptOpenJDK"));
        roots.push(pf.join("Microsoft").join("jdk"));
        roots.push(pf.join("Zulu"));
        roots.push(pf.join("Azul"));
        roots.push(pf.join("BellSoft"));
        roots.push(pf.join("Amazon Corretto"));
        roots.push(pf.join("JavaSoft"));
    }
    if let Some(pfx86) = env_var_path("ProgramFiles(x86)") {
        roots.push(pfx86.join("Java"));
    }

    // 2) JAVA_HOME。
    if let Some(h) = env_var("JAVA_HOME") {
        if !h.is_empty() {
            roots.push(PathBuf::from(h));
        }
    }

    // 3) 包管理器（scoop / chocolatey）。
    if let Some(local) = env_var_path("LOCALAPPDATA") {
        roots.push(local.join("Programs").join("Eclipse Adoptium"));
        roots.push(local.join("scoop").join("apps"));
    }
    roots.push(PathBuf::from("C:\\ProgramData\\chocolatey\\lib"));

    // 4) Minecraft 自带 runtime（随官方启动器安装）。
    if let Some(appdata) = env_var("APPDATA") {
        roots.push(PathBuf::from(&appdata).join(".minecraft").join("runtime"));
    }

    // 5) PATH 里直接能找到的 java。
    if let Some(path_dirs) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_dirs) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            roots.push(dir);
        }
    }

    // 6) 注册表（Windows）：HKLM\SOFTWARE\JavaSoft。
    //    reg 查询在受限环境可能被拦截，失败就跳过，不阻塞整体探测。
    collect_registry_roots(&mut roots);

    roots
}

fn env_var(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn env_var_path(k: &str) -> Option<PathBuf> {
    std::env::var(k)
        .ok()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

#[cfg(windows)]
fn collect_registry_roots(roots: &mut Vec<PathBuf>) {
    // 读取 JavaSoft 注册表下各 JDK/JRE 的 JavaHome 值。
    let keys = [
        "HKLM\\SOFTWARE\\JavaSoft\\Java Development Kit",
        "HKLM\\SOFTWARE\\JavaSoft\\Java Runtime Environment",
        "HKLM\\SOFTWARE\\JavaSoft\\JDK",
    ];
    for key in keys {
        // reg 命令在受限环境会被沙箱拦截；用 Command::output 捕获失败并忽略即可。
        if let Ok(out) = Command::new("reg").arg("query").arg(key).output() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                let line = line.trim();
                // 形如：`    JavaHome    REG_SZ    C:\Program Files\Java\jdk-17`
                if let Some(rest) = line.strip_prefix("JavaHome") {
                    if let Some(v) = rest.split_once("REG_SZ") {
                        let p = v.1.trim();
                        if !p.is_empty() {
                            roots.push(PathBuf::from(p));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(not(windows))]
fn collect_registry_roots(_roots: &mut Vec<PathBuf>) {}

/// 候选可执行名（按平台）。
fn java_exec_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["java.exe", "java.cmd", "java.bat"]
    } else {
        &["java"]
    }
}

/// 在给定根目录里收集所有 java 候选可执行文件（深度受限，避免扫全盘）。
fn collect_candidates(roots: &[PathBuf]) -> Vec<PathBuf> {
    let names = java_exec_names();
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, u32)> = roots
        .iter()
        .filter(|p| p.is_dir())
        .map(|p| (p.clone(), 0))
        .collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 4 {
                    stack.push((p, depth + 1));
                }
            } else if p.is_file() {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    let lower = name.to_ascii_lowercase();
                    if names.iter().any(|n| *n == lower) {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}

/// 解析 `java -version` 的输出（stdout + stderr），返回 `(version, vendor, arch)`。
///
/// `java -version` 文本写到 stderr，格式例如：
/// ```text
/// openjdk version "17.0.9" 2023-10-17
/// OpenJDK Runtime Environment Temurin-17.0.9+9 (build 17.0.9+9)
/// OpenJDK 64-Bit Server VM (build 17.0.9+9, mixed mode)
/// ```
pub fn parse_java_version(text: &str) -> Option<(String, String, String)> {
    // 版本：取第一个含 "version" 的行里的引号字符串。
    let mut version = String::new();
    for line in text.lines() {
        if let Some(idx) = line.find("version") {
            let tail = &line[idx..];
            if let Some(start) = tail.find('"') {
                let rest = &tail[start + 1..];
                if let Some(end) = rest.find('"') {
                    version = rest[..end].to_owned();
                    break;
                }
            }
        }
    }
    if version.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let vendor = if lower.contains("java(tm) se") || lower.contains("oracle") {
        "Oracle"
    } else if lower.contains("temurin")
        || lower.contains("adoptium")
        || lower.contains("adoptopenjdk")
    {
        "Eclipse Adoptium"
    } else if lower.contains("zulu") {
        "Azul Zulu"
    } else if lower.contains("liberica") {
        "BellSoft Liberica"
    } else if lower.contains("corretto") {
        "Amazon Corretto"
    } else if lower.contains("microsoft") {
        "Microsoft"
    } else if lower.contains("openjdk") {
        "OpenJDK"
    } else {
        "Unknown"
    }
    .to_owned();
    let arch = if lower.contains("aarch64") {
        "aarch64"
    } else if lower.contains("64-bit") || lower.contains("amd64") {
        "x64"
    } else if lower.contains("32-bit") || lower.contains("i386") || lower.contains("x86") {
        "x86"
    } else {
        ""
    }
    .to_owned();
    Some((version, vendor, arch))
}

fn run_and_parse(java: &Path) -> Option<DetectedJava> {
    let out = Command::new(java).arg("-version").output().ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let text = format!("{stderr}{stdout}");
    let (version, vendor, arch) = parse_java_version(&text)?;
    let home = java.parent()?.parent()?.to_path_buf();
    Some(DetectedJava {
        path: java.to_string_lossy().into_owned(),
        home: home.to_string_lossy().into_owned(),
        version,
        vendor,
        arch,
    })
}

/// 探测全部 Java 运行时（默认根目录）。
pub fn find_java_installations() -> Vec<DetectedJava> {
    find_java_in_roots(&default_search_roots())
}

/// 在指定的根目录里探测（便于测试注入临时目录）。
pub fn find_java_in_roots(roots: &[PathBuf]) -> Vec<DetectedJava> {
    let candidates = collect_candidates(roots);
    let mut seen_home: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for c in candidates {
        if let Some(dj) = run_and_parse(&c) {
            let key = dj.home.to_ascii_lowercase();
            if seen_home.insert(key) {
                out.push(dj);
            }
        }
    }
    out.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
    out
}

/// 版本比较键：把 `1.8.0_391` / `17.0.9` 归一成 `(major, minor, patch)` 三元组，
/// 用于按"新版本优先"排序。
fn version_key(v: &str) -> (u32, u32, u32) {
    let mut parts = v.split(['.', '_']);
    let parse = |s: Option<&str>| -> u32 {
        s.and_then(|s| {
            s.trim_start_matches(|c: char| !c.is_ascii_digit())
                .parse::<u32>()
                .ok()
        })
        .unwrap_or(0)
    };
    (
        parse(parts.next()),
        parse(parts.next()),
        parse(parts.next()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openjdk_temurin() {
        let text = "\
openjdk version \"17.0.9\" 2023-10-17
OpenJDK Runtime Environment Temurin-17.0.9+9 (build 17.0.9+9)
OpenJDK 64-Bit Server VM (build 17.0.9+9, mixed mode)";
        let (v, vendor, arch) = parse_java_version(text).unwrap();
        assert_eq!(v, "17.0.9");
        assert_eq!(vendor, "Eclipse Adoptium");
        assert_eq!(arch, "x64");
    }

    #[test]
    fn parses_oracle_8() {
        let text = "\
java version \"1.8.0_391\"
Java(TM) SE Runtime Environment (build 1.8.0_391-b13)
Java HotSpot(TM) 64-Bit Server VM (build 25.391-b13, mixed mode)";
        let (v, vendor, arch) = parse_java_version(text).unwrap();
        assert_eq!(v, "1.8.0_391");
        assert_eq!(vendor, "Oracle");
        assert_eq!(arch, "x64");
    }

    #[test]
    fn parses_corretto_aarch64() {
        let text = "\
openjdk version \"21.0.1\" 2023-10-17
OpenJDK Runtime Environment Corretto-21.0.1.12.1 (build 21.0.1+12)
OpenJDK 64-Bit Server VM (build 21.0.1+12, mixed mode, aarch64)";
        let (v, vendor, arch) = parse_java_version(text).unwrap();
        assert_eq!(v, "21.0.1");
        assert_eq!(vendor, "Amazon Corretto");
        assert_eq!(arch, "aarch64");
    }

    #[test]
    fn parses_zulu_and_unknown() {
        let text = "openjdk version \"11.0.21\"\nZulu11.62+17-CA (build 11.0.21+9)\nOpenJDK 64-Bit Server VM";
        let (v, vendor, _) = parse_java_version(text).unwrap();
        assert_eq!(v, "11.0.21");
        assert_eq!(vendor, "Azul Zulu");

        let text2 = "openjdk version \"22\"\nSomeUnknownJVM 64-Bit Server VM";
        assert_eq!(parse_java_version(text2).unwrap().1, "OpenJDK");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_java_version("this is not java output").is_none());
        assert!(parse_java_version("").is_none());
    }

    #[test]
    fn version_key_sorts_newest_first() {
        let mut v = vec!["1.8.0_391", "17.0.9", "21.0.1", "11.0.21"];
        v.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
        assert_eq!(v, vec!["21.0.1", "17.0.9", "11.0.21", "1.8.0_391"]);
    }

    #[test]
    fn finds_java_from_fake_shim() {
        let tmp = launcher_plugin_common::test_support::temp_path("java-scan");
        let _ = std::fs::create_dir_all(tmp.join("jdk17/bin"));
        #[cfg(windows)]
        let shim = tmp.join("jdk17/bin/java.cmd");
        #[cfg(not(windows))]
        let shim = tmp.join("jdk17/bin/java");
        #[cfg(windows)]
        let body = "@echo off\r\n\
            echo openjdk version \"17.0.9\" 1>&2\r\n\
            echo OpenJDK Runtime Environment Temurin-17.0.9+9 1>&2\r\n\
            echo OpenJDK 64-Bit Server VM 1>&2\r\n";
        #[cfg(not(windows))]
        let body = "#!/bin/sh\n\
            echo 'openjdk version \"17.0.9\"'\n\
            echo 'OpenJDK Runtime Environment Temurin-17.0.9+9'\n\
            echo 'OpenJDK 64-Bit Server VM'\n";
        std::fs::write(&shim, body).unwrap();
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&shim).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&shim, perms).unwrap();
        }

        let found = find_java_in_roots(&[tmp.clone()]);
        assert_eq!(found.len(), 1, "应找到 1 个 Java：{found:?}");
        assert_eq!(found[0].version, "17.0.9");
        assert_eq!(found[0].vendor, "Eclipse Adoptium");
        assert_eq!(found[0].arch, "x64");
        // home = <tmp>/jdk17
        assert!(found[0].home.ends_with("jdk17"), "{}", found[0].home);
    }

    #[test]
    fn finds_nothing_in_empty_dir() {
        let tmp = launcher_plugin_common::test_support::temp_path("java-empty");
        let _ = std::fs::create_dir_all(&tmp);
        assert!(find_java_in_roots(&[tmp]).is_empty());
    }
}