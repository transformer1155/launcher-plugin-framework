//! `plugin.json` 的解析与校验。
//!
//! # 为什么插件清单是框架清单的超集
//!
//! 框架的 [`launcher_plugin_framework::signature::PluginManifest`] 只有
//! `id / name / version / description / tools` 五个字段（它刻意保持最小，业务字段不属于框架）。
//! 插件清单在本规格里多了 `author / trust / review / min_framework_version / network`。
//!
//! 因此这里定义 [`PluginManifestV1`]（完整清单），并提供
//! [`PluginManifestV1::to_framework_manifest`] 转成框架形态。
//!
//! **签名覆盖的是 `plugin.json` 的原始字节**，不是这个结构体重新序列化的结果 ——
//! 所以解析失败绝不能被吞掉：签名校验用的 `manifest_raw` 必须与磁盘上一致。
//!
//! # `review` 字段（Q8 的落地位置）
//!
//! 规格裁决 Q8 要求"沙箱插件准入需人工 review，此约束写入插件清单的 review 字段"。
//! 本模块的强制力如下：
//!
//! | trust | review | 结果 |
//! |---|---|---|
//! | trusted | official / reviewed | 合法 |
//! | trusted | unreviewed | **拒绝加载**（不能自称可信却没过审） |
//! | sandboxed | official / reviewed | 合法 |
//! | sandboxed | unreviewed | 合法，但加载时打 WARN（Q8 要求） |

use serde::{Deserialize, Serialize};

use launcher_plugin_framework::signature::PluginManifest;

/// 插件声明的信任意图。
///
/// **注意**：这是插件的"声明"，不是"结论"。真正的信任级别由签名校验结果决定
/// （见框架 `docs/security-boundary.md`）。一个声明 `trusted` 但没签名的插件
/// 仍然会被 loader 降级到沙箱。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustKind {
    Trusted,
    Sandboxed,
}

/// 人工审核状态（Q8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewKind {
    /// 官方自己维护，等同过审。
    Official,
    /// 第三方但已人工审核。
    Reviewed,
    /// 未审核。沙箱插件允许但加载时 WARN。
    Unreviewed,
}

/// 插件声明需要访问的域名（沙箱 API 限制的一部分）。
///
/// 框架本身**没有** HTTP 能力（见 Step 1 报告的"框架层接口不足"），
/// 这里只落地"声明 + 白名单校验"这一半，实际请求由宿主注入的
/// [`crate::PluginContext`] HTTP 端口承担。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkDecl {
    /// 允许访问的主机名（精确匹配，不含通配符）。
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// 声明为什么需要网络（给 review 的人看）。
    #[serde(default)]
    pub reason: String,
}

/// 完整插件清单（`plugin.json` 的超集版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifestV1 {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    pub trust: TrustKind,
    pub review: ReviewKind,
    #[serde(default = "default_min_framework_version")]
    pub min_framework_version: String,
    /// 声明的工具短名。必须与代码里注册的完全一致（[`Self::check_tools`]）。
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub network: Option<NetworkDecl>,
}

fn default_min_framework_version() -> String {
    "0.1.0".to_owned()
}

/// 清单校验失败。全部属于**加载期**错误，应当拒绝加载（不是降级）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Parse(String),
    BadId(String),
    BadVersion(String),
    BadToolName(String),
    EmptyTools,
    DuplicateTool(String),
    /// `trust = trusted` 却 `review = unreviewed`。
    TrusteeWithoutReview,
    /// 声明的工具集合与代码注册的不一致。
    ToolMismatch {
        declared: Vec<String>,
        registered: Vec<String>,
    },
    /// 框架版本低于插件要求。
    FrameworkTooOld {
        required: String,
        actual: String,
    },
    EmptyAllowHosts,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(m) => write!(f, "plugin.json 解析失败：{m}"),
            Self::BadId(id) => write!(
                f,
                "plugin_id `{id}` 非法：只允许 ASCII 字母数字与 . _ -，且不能以 . 开头/结尾、不能含 .."
            ),
            Self::BadVersion(v) => write!(f, "version `{v}` 必须是 x.y.z 三段数字"),
            Self::BadToolName(t) => write!(
                f,
                "工具名 `{t}` 非法：只允许小写字母、数字、下划线，且不能以数字开头"
            ),
            Self::EmptyTools => f.write_str("tools 不能为空"),
            Self::DuplicateTool(t) => write!(f, "工具名 `{t}` 重复声明"),
            Self::TrusteeWithoutReview => f.write_str(
                "trust=trusted 的插件必须 review=official 或 reviewed（不能把未审核插件自称可信）",
            ),
            Self::ToolMismatch {
                declared,
                registered,
            } => write!(
                f,
                "清单声明的工具与代码注册的不一致：声明 {declared:?}，注册 {registered:?}"
            ),
            Self::FrameworkTooOld { required, actual } => write!(
                f,
                "插件要求框架 >= {required}，当前框架为 {actual}"
            ),
            Self::EmptyAllowHosts => f.write_str("声明了 network 但 allow_hosts 为空"),
        }
    }
}

impl std::error::Error for ManifestError {}

impl PluginManifestV1 {
    /// 从 `plugin.json` 的**原始字节**解析（保留原始字节用于签名，见模块文档）。
    pub fn parse(raw: &[u8]) -> Result<Self, ManifestError> {
        let m: Self =
            serde_json::from_slice(raw).map_err(|e| ManifestError::Parse(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    /// 结构校验（不含"与代码是否一致"，那需要 [`Self::check_tools`]）。
    pub fn validate(&self) -> Result<(), ManifestError> {
        validate_plugin_id(&self.id)?;
        parse_version(&self.version)
            .ok_or_else(|| ManifestError::BadVersion(self.version.clone()))?;
        if self.min_framework_version.is_empty() {
            // 空串等价于"不要求"，但为了可诊断，仍要求它是合法版本号。
            return Err(ManifestError::BadVersion(String::new()));
        }
        parse_version(&self.min_framework_version)
            .ok_or_else(|| ManifestError::BadVersion(self.min_framework_version.clone()))?;

        if self.tools.is_empty() {
            return Err(ManifestError::EmptyTools);
        }
        let mut seen = std::collections::BTreeSet::new();
        for t in &self.tools {
            validate_tool_name(t)?;
            if !seen.insert(t.clone()) {
                return Err(ManifestError::DuplicateTool(t.clone()));
            }
        }

        // Q8：未审核的插件不得自称可信。
        if self.trust == TrustKind::Trusted && self.review == ReviewKind::Unreviewed {
            return Err(ManifestError::TrusteeWithoutReview);
        }

        if let Some(net) = &self.network {
            if net.allow_hosts.is_empty() {
                return Err(ManifestError::EmptyAllowHosts);
            }
            for h in &net.allow_hosts {
                // 只允许精确主机名，禁止通配符/协议/端口混入，避免"看起来像白名单"。
                if h.contains('*') || h.contains('/') || h.contains(':') || h.contains(' ') {
                    return Err(ManifestError::Parse(format!(
                        "allow_hosts 里 `{h}` 非法：必须是精确主机名（无通配符、无协议、无端口）"
                    )));
                }
            }
        }

        Ok(())
    }

    /// 框架版本兼容性检查。
    pub fn ensure_framework_compatible(
        &self,
        framework_version: &str,
    ) -> Result<(), ManifestError> {
        let required = parse_version(&self.min_framework_version)
            .ok_or_else(|| ManifestError::BadVersion(self.min_framework_version.clone()))?;
        let actual = parse_version(framework_version)
            .ok_or_else(|| ManifestError::BadVersion(framework_version.to_owned()))?;
        if actual < required {
            return Err(ManifestError::FrameworkTooOld {
                required: self.min_framework_version.clone(),
                actual: framework_version.to_owned(),
            });
        }
        Ok(())
    }

    /// **工具一致性校验**（规格硬要求：`tools` 必须与代码注册的完全一致）。
    ///
    /// 双向比较而不是只查"声明的都注册了"：多注册一个没声明的工具同样是不一致 ——
    /// 那意味着签名覆盖的清单没有描述插件实际能做的一切。
    pub fn check_tools(&self, registered: &[&str]) -> Result<(), ManifestError> {
        let declared: std::collections::BTreeSet<&str> =
            self.tools.iter().map(|s| s.as_str()).collect();
        let reg: std::collections::BTreeSet<&str> = registered.iter().copied().collect();
        if declared != reg {
            return Err(ManifestError::ToolMismatch {
                declared: self.tools.clone(),
                registered: registered.iter().map(|s| (*s).to_owned()).collect(),
            });
        }
        Ok(())
    }

    /// 转成框架的清单形态。
    pub fn to_framework_manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            tools: self.tools.clone(),
        }
    }

    /// 加载时的告警（不阻断加载，只提示）。
    pub fn load_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.trust == TrustKind::Sandboxed && self.review == ReviewKind::Unreviewed {
            // Q8 要求：沙箱插件 unreviewed 时打 WARN。
            out.push(format!(
                "插件 {} 为沙箱级且未人工审核（review=unreviewed），运行在受限沙箱；\
                 请勿给它授予额外能力",
                self.id
            ));
        }
        if self.trust == TrustKind::Trusted && self.review == ReviewKind::Official {
            // 声明可信但实际是否可信由验签决定，这里只提示宿主。
            out.push(format!(
                "插件 {} 声明 trust=trusted；实际信任级别由签名校验决定",
                self.id
            ));
        }
        out
    }
}

/// `plugin_id` 规则：ASCII 字母数字 + `.` `_` `-`，不能以 `.` 开头/结尾，不能含 `..`。
///
/// 之所以要这些限制：工具名是 `"{plugin_id}.{tool}"`，而框架用"第一个点之后不再含点"
/// 来判定归属（`PluginId::owns_tool_name`）。`..` 会让解析出现歧义。
pub fn validate_plugin_id(id: &str) -> Result<(), ManifestError> {
    if id.is_empty()
        || id.starts_with('.')
        || id.ends_with('.')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(ManifestError::BadId(id.to_owned()));
    }
    Ok(())
}

/// 工具短名规则：小写字母开头，仅小写字母/数字/下划线。
pub fn validate_tool_name(t: &str) -> Result<(), ManifestError> {
    let mut chars = t.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return Err(ManifestError::BadToolName(t.to_owned())),
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(ManifestError::BadToolName(t.to_owned()));
    }
    Ok(())
}

/// 解析 `x.y.z` 为可比较的三元组。不做语义化版本的全部规则（预发布号等），
/// 因为插件只要求"框架至少这么新"，够用即可。
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((a, b, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_raw() -> Vec<u8> {
        // 注意：不能写成 `br#"..."#` —— 字节串字面量必须是纯 ASCII，
        // 而清单里有中文 name/description。用 `r#"..."#` 再转 Vec<u8> 才正确。
        r#"{
            "id": "com.example.official-instance",
            "name": "实例管理",
            "version": "0.1.0",
            "author": "official",
            "description": "创建、列出、切换、删除游戏实例",
            "trust": "trusted",
            "review": "official",
            "min_framework_version": "0.1.0",
            "tools": ["list_instances"]
        }"#
        .as_bytes()
        .to_vec()
    }

    #[test]
    fn parses_valid_manifest() {
        let m = PluginManifestV1::parse(&base_raw()).unwrap();
        assert_eq!(m.id, "com.example.official-instance");
        assert_eq!(m.trust, TrustKind::Trusted);
        assert_eq!(m.review, ReviewKind::Official);
        assert_eq!(m.tools, vec!["list_instances"]);
    }

    #[test]
    fn rejects_trusted_without_review() {
        let mut v: serde_json::Value = serde_json::from_slice(&base_raw()).unwrap();
        v["review"] = serde_json::json!("unreviewed");
        let raw = serde_json::to_vec(&v).unwrap();
        let err = PluginManifestV1::parse(&raw).unwrap_err();
        assert_eq!(err, ManifestError::TrusteeWithoutReview);
    }

    #[test]
    fn allows_sandboxed_unreviewed_with_warning() {
        let mut v: serde_json::Value = serde_json::from_slice(&base_raw()).unwrap();
        v["trust"] = serde_json::json!("sandboxed");
        v["review"] = serde_json::json!("unreviewed");
        let raw = serde_json::to_vec(&v).unwrap();
        let m = PluginManifestV1::parse(&raw).expect("沙箱 + 未审核应当允许");
        let warns = m.load_warnings();
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("unreviewed"), "{warns:?}");
    }

    #[test]
    fn rejects_bad_plugin_ids() {
        for bad in ["", ".abc", "abc.", "a..b", "a b", "a/b"] {
            assert!(validate_plugin_id(bad).is_err(), "`{bad}` 应当被拒");
        }
        for good in ["com.example.x", "official-instance", "a_b-c.d"] {
            assert!(validate_plugin_id(good).is_ok(), "`{good}` 应当通过");
        }
    }

    #[test]
    fn rejects_bad_tool_names() {
        for bad in ["", "List", "1abc", "a-b", "a.b"] {
            assert!(validate_tool_name(bad).is_err(), "`{bad}` 应当被拒");
        }
        for good in ["list_instances", "a", "a1_b"] {
            assert!(validate_tool_name(good).is_ok(), "`{good}` 应当通过");
        }
    }

    #[test]
    fn rejects_duplicate_and_empty_tools() {
        let mut v: serde_json::Value = serde_json::from_slice(&base_raw()).unwrap();
        v["tools"] = serde_json::json!(["a_tool", "a_tool"]);
        assert_eq!(
            PluginManifestV1::parse(&serde_json::to_vec(&v).unwrap()).unwrap_err(),
            ManifestError::DuplicateTool("a_tool".into())
        );

        v["tools"] = serde_json::json!([]);
        assert_eq!(
            PluginManifestV1::parse(&serde_json::to_vec(&v).unwrap()).unwrap_err(),
            ManifestError::EmptyTools
        );
    }

    #[test]
    fn tool_consistency_is_bidirectional() {
        let m = PluginManifestV1::parse(&base_raw()).unwrap();

        // 完全一致
        assert!(m.check_tools(&["list_instances"]).is_ok());

        // 声明了但没注册
        let err = m.check_tools(&[]).unwrap_err();
        assert!(matches!(err, ManifestError::ToolMismatch { .. }));

        // 注册了但没声明（多注册同样不一致）
        let err2 = m
            .check_tools(&["list_instances", "extra_tool"])
            .unwrap_err();
        assert!(matches!(err2, ManifestError::ToolMismatch { .. }));
        assert!(!err2.to_string().is_empty());
    }

    #[test]
    fn framework_version_gate() {
        let m = PluginManifestV1::parse(&base_raw()).unwrap();
        assert!(m.ensure_framework_compatible("0.1.0").is_ok());
        assert!(m.ensure_framework_compatible("0.2.0").is_ok());
        let err = m.ensure_framework_compatible("0.0.9").unwrap_err();
        assert!(matches!(err, ManifestError::FrameworkTooOld { .. }));
    }

    #[test]
    fn version_parser() {
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("1.22.3"), Some((1, 22, 3)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.2.x"), None);
        // 字符串比较会错的地方：10 应当大于 9
        assert!(parse_version("1.10.0").unwrap() > parse_version("1.9.0").unwrap());
    }

    #[test]
    fn network_decl_rejects_wildcards() {
        let mut v: serde_json::Value = serde_json::from_slice(&base_raw()).unwrap();
        v["network"] = serde_json::json!({"allow_hosts": ["*.example.com"]});
        let err = PluginManifestV1::parse(&serde_json::to_vec(&v).unwrap()).unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)), "{err:?}");

        v["network"] = serde_json::json!({"allow_hosts": ["api.modrinth.com"]});
        assert!(PluginManifestV1::parse(&serde_json::to_vec(&v).unwrap()).is_ok());
    }

    #[test]
    fn converts_to_framework_manifest() {
        let m = PluginManifestV1::parse(&base_raw()).unwrap();
        let fm = m.to_framework_manifest();
        assert_eq!(fm.id, m.id);
        assert_eq!(fm.tools, m.tools);
        // 框架 manifest 不含 author/trust/review，这是刻意的最小化
        assert_eq!(fm.version, "0.1.0");
    }
}
