//! 真实插件发现：扫描插件目录里的 `plugin.json`，用框架的 [`PluginManifestV1`] 解析，
//! 返回已发现的插件清单。
//!
//! 这是「UI 只是插件能力的呈现」的落地 —— 启动器不再硬编码它能干什么，而是**从磁盘上
//! 真实读到**装了哪些插件。`LiveDataSource` 之后只用「已编译进二进制的那些 id」去激活工具，
//! 其余被发现的插件照常展示，只是标注「需要重新编译启动器才能加载」（单进程单二进制的现实约束）。
//!
//! 内置插件的清单随源码内嵌（`include_str!`），这样即便运行目录还没有 `plugin.json`，
//! 首次启动也能把内置插件铺到插件目录、并被真实扫描到。

use std::path::{Path, PathBuf};

use launcher_plugin_common::manifest::{PluginManifestV1, ReviewKind, TrustKind};

/// 一个被发现的插件（来自磁盘上的 `plugin.json`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    /// `trusted` / `sandboxed`。
    pub trust: String,
    /// `official` / `reviewed` / `unreviewed`。
    pub review: String,
    pub tools: Vec<String>,
    /// 插件目录（含 plugin.json 的那一层）。
    pub dir: PathBuf,
    /// 是否为内置插件（与已编译进二进制的 `build()` 对应）。
    pub builtin: bool,
    /// 是否有已编译的代码可加载（目前仅内置 id 可加载）。
    pub loadable: bool,
    /// 是否被用户禁用（插件目录里存在 `disabled.flag`）。内置/官方插件不可禁用。
    pub disabled: bool,
}

impl DiscoveredPlugin {
    /// 给 UI 用的状态文案。
    pub fn status(&self) -> &'static str {
        if self.disabled {
            "已禁用"
        } else if self.loadable {
            "已加载"
        } else if self.builtin {
            "已加载（内置）"
        } else {
            "已安装 · 需重新编译启动器"
        }
    }

    /// 信任徽章文案。
    pub fn trust_badge(&self) -> &'static str {
        match (self.trust.as_str(), self.review.as_str()) {
            ("trusted", _) => "可信",
            ("sandboxed", "unreviewed") => "沙箱 · 未审核",
            ("sandboxed", _) => "沙箱",
            _ => "未知",
        }
    }
}

/// 已知已编译进二进制的插件 id（其 `build()` 在宿主里静态链接）。
/// `com.example.launcher-ui` 是宿主 UI 自身——没有 `build()`，但当然也编译进了二进制。
pub fn is_compiled_plugin(id: &str) -> bool {
    matches!(
        id,
        "com.example.official-instance"
            | "com.example.official-launch"
            | "com.example.official-mod"
            | "com.example.sample-community"
            | "com.example.launcher-ui"
    )
}

/// 内置插件的清单（编译期内嵌，避免依赖运行目录里是否已有文件）。
const BUILTIN_MANIFESTS: &[&str] = &[
    include_str!("../../../plugins/official-instance/plugin.json"),
    include_str!("../../../plugins/official-launch/plugin.json"),
    include_str!("../../../plugins/official-mod/plugin.json"),
    include_str!("../../../plugins/sample-community/plugin.json"),
    include_str!("../plugin.json"),
];

/// 把一份 `plugin.json` 字节解析成 `DiscoveredPlugin`。解析失败（非法清单）返回 `None`。
pub fn parse_manifest(bytes: &[u8], dir: PathBuf) -> Option<DiscoveredPlugin> {
    let m = PluginManifestV1::parse(bytes).ok()?;
    let compiled = is_compiled_plugin(&m.id);
    Some(DiscoveredPlugin {
        id: m.id.clone(),
        name: m.name.clone(),
        version: m.version.clone(),
        description: m.description.clone(),
        author: m.author.clone(),
        trust: match m.trust {
            TrustKind::Trusted => "trusted",
            TrustKind::Sandboxed => "sandboxed",
        }
        .to_owned(),
        review: match m.review {
            ReviewKind::Official => "official",
            ReviewKind::Reviewed => "reviewed",
            ReviewKind::Unreviewed => "unreviewed",
        }
        .to_owned(),
        tools: m.tools.clone(),
        dir,
        builtin: compiled,
        loadable: compiled,
        disabled: false,
    })
}

/// 扫描 `plugins_dir` 下每个子目录的 `plugin.json`，返回已发现的插件。
pub fn scan_plugins(plugins_dir: &Path) -> Vec<DiscoveredPlugin> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return out;
    };
    for e in entries.flatten() {
        let sub = e.path();
        if !sub.is_dir() {
            continue;
        }
        let manifest = sub.join("plugin.json");
        let Ok(bytes) = std::fs::read(&manifest) else {
            continue;
        };
        if let Some(mut p) = parse_manifest(&bytes, sub.clone()) {
            // 禁用标记：目录里放了 disabled.flag 的插件视为已禁用。
            p.disabled = sub.join("disabled.flag").exists();
            out.push(p);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 内置插件清单（id → JSON 文本），供首次启动铺目录与演示模式使用。
pub fn builtin_manifests() -> Vec<(String, String)> {
    BUILTIN_MANIFESTS
        .iter()
        .filter_map(|raw| {
            let m = PluginManifestV1::parse(raw.as_bytes()).ok()?;
            Some((m.id, raw.to_string()))
        })
        .collect()
}

/// 内置插件（以 `DiscoveredPlugin` 形式，用于演示模式直接展示）。
pub fn builtin_plugins() -> Vec<DiscoveredPlugin> {
    builtin_manifests()
        .into_iter()
        .filter_map(|(id, json)| parse_manifest(json.as_bytes(), PathBuf::from(id)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_builtin_manifests() {
        let all = builtin_plugins();
        assert_eq!(all.len(), 5, "应内嵌 5 个内置插件清单（含启动器 UI 自身）");
        let ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"com.example.official-instance"));
        assert!(ids.contains(&"com.example.official-launch"));
        assert!(ids.contains(&"com.example.official-mod"));
        assert!(ids.contains(&"com.example.sample-community"));
        assert!(ids.contains(&"com.example.launcher-ui"));
    }

    #[test]
    fn builtins_are_loadable() {
        for p in builtin_plugins() {
            assert!(p.loadable, "{} 应可加载", p.id);
            assert!(p.builtin);
            assert!(!p.tools.is_empty(), "{} 应声明工具", p.id);
        }
    }

    #[test]
    fn scans_a_directory() {
        let tmp = launcher_plugin_common::test_support::temp_path("plugin-scan");
        for (id, json) in builtin_manifests() {
            let dir = tmp.join(&id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("plugin.json"), json).unwrap();
        }
        let found = scan_plugins(&tmp);
        assert_eq!(
            found.len(),
            5,
            "应扫到 5 个插件（含启动器 UI 自身）：{found:?}"
        );
        // 随便挑一个验证字段
        let launch = found
            .iter()
            .find(|p| p.id == "com.example.official-launch")
            .unwrap();
        assert_eq!(launch.name, "启动管理");
        assert!(launch.tools.contains(&"detect_java".to_owned()));
        assert_eq!(launch.status(), "已加载");
        let ui = found
            .iter()
            .find(|p| p.id == "com.example.launcher-ui")
            .expect("应扫到启动器 UI 自身的插件清单");
        assert!(ui.tools.contains(&"page_plugins".to_owned()));
    }

    #[test]
    fn skips_dirs_without_manifest() {
        let tmp = launcher_plugin_common::test_support::temp_path("plugin-scan-empty");
        std::fs::create_dir_all(tmp.join("not-a-plugin")).unwrap();
        assert!(scan_plugins(&tmp).is_empty());
    }
}
