//! 签名校验：决定插件跑在宿主上下文还是沙箱里。
//!
//! # 规格要点
//!
//! - 公钥内置启动器，私钥官方持有。
//! - 插件包**整体**签名，覆盖清单 + 所有代码文件。
//! - 校验时机：加载时 + 每次沙箱单元重启时 + 热重载时。
//! - 吊销列表随启动器更新，**吊销列表本身必须签名**。
//! - 校验失败 → **降级到沙箱**，不拒绝加载，UI 提示"未通过签名校验，运行在受限沙箱"。
//!
//! # "降级 vs 拒绝"的边界（必须区分清楚）
//!
//! | 情况 | 结果 | 理由 |
//! |---|---|---|
//! | 无签名 / 签名无效 / 签名者被吊销 | **降级到沙箱** | 规格明文要求 |
//! | 吊销列表自身签名无效 | **全部降级到沙箱**（fail-closed） | 无法信任"谁被吊销"，则谁都不能被信任 |
//! | 包内文件摘要与签名清单不符 | **拒绝加载** (`LoadError::PackageTampered`) | 这不是"没签名"，是签名之后被改过：签名清单是权威，摘要不符说明包被篡改 |
//! | 清单缺失/格式错 | **拒绝加载** | 没有清单就不知道 plugin_id，无法安全地降级 |
//!
//! 上表第 3 条容易被误实现成"降级"。它不是：签名清单里明确写了"文件 A 的 sha256 是 X"，
//! 而实际是 Y —— 这意味着攻击者改了代码但没改签名。降级到沙箱会让攻击者的代码跑起来
//! （尽管受限），而正确做法是拒绝。见 docs/open-questions.md 第 2 条。

pub mod ed25519_verifier;
pub mod revocation;

pub use ed25519_verifier::{BuiltInKeys, Ed25519Verifier};
pub use revocation::{RevocationList, RevocationStatus, SignedRevocationList};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::types::PluginId;

/// 插件包中的一个文件。签名覆盖其内容摘要。
#[derive(Debug, Clone)]
pub struct PackageFile {
    /// 包内相对路径，使用 `/` 分隔，**区分大小写**。
    pub path: String,
    pub bytes: Vec<u8>,
    /// 该文件是否是可执行代码（.rs/.dll/.so 等）。
    /// 仅用于诊断与 UI 展示；签名覆盖**全部文件**，不区分类型。
    pub is_code: bool,
}

/// 插件清单。字段刻意保持最小 —— 业务字段（Mod 管理、版本等）不属于本框架。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// 全局唯一。同时是工具名前缀。
    pub id: String,
    pub name: String,
    pub version: String,
    /// 作者可读信息，不参与任何信任决策。
    #[serde(default)]
    pub description: String,
    /// 插件声明的工具名（仅用于加载后交叉校验；权威仍是 register_tool）。
    #[serde(default)]
    pub tools: Vec<String>,
}

/// 签名算法的抽象。新增算法（例如未来换 P-256）不需要改校验流程。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignatureAlgorithm {
    Ed25519,
    /// 保留：本框架只实现 Ed25519 与以下"无签名"。
    Unsupported(String),
}

/// 一个插件签名的原始材料。
#[derive(Debug, Clone)]
pub struct PluginSignature {
    pub algorithm: SignatureAlgorithm,
    /// 签名者公钥 ID（用于吊销列表查找）。不是公钥本身。
    pub key_id: String,
    /// 签名字节。Ed25519 为 64 字节。
    pub bytes: Vec<u8>,
}

/// 插件包：清单原文 + 全部文件 + 可选签名。
#[derive(Debug, Clone)]
pub struct PluginPackage {
    pub plugin_id: PluginId,
    /// 清单**原始字节**。签名直接覆盖它，不做重新序列化 ——
    /// 否则 JSON 键序/空白差异会让"重新序列化后验签"变成不可靠的。
    pub manifest_raw: Vec<u8>,
    pub manifest: PluginManifest,
    /// 除清单外的文件。按 path 排序后参与摘要。
    pub files: Vec<PackageFile>,
    pub signature: Option<PluginSignature>,
}

impl PluginPackage {
    /// **包整体摘要**：覆盖清单 + 全部代码/资源文件。
    ///
    /// 计算方式（确定性、与 JSON 序列化无关）：
    /// ```text
    /// H = sha256(
    ///   "dsh-plugin-package-v1\0"
    ///   + "manifest\0" + manifest_raw + "\0" + sha256(manifest_raw) + "\0"
    ///   + 对每个文件按 path 字典序：
    ///       path + "\0" + sha256(bytes) + "\0"
    /// )
    /// ```
    ///
    /// 用 `\0` 分隔并逐项把"路径 + 内容摘要"都纳进来，避免"把文件挪个位置但内容不变"
    /// 这种攻击（路径参与摘要，所以挪位置会改变摘要）。
    pub fn digest(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut h = Sha256::new();
        h.update(b"dsh-plugin-package-v1\0");

        h.update(b"manifest\0");
        h.update(&self.manifest_raw);
        h.update(b"\0");
        let mut mh = Sha256::new();
        mh.update(&self.manifest_raw);
        h.update(mh.finalize());
        h.update(b"\0");

        let mut sorted: Vec<&PackageFile> = self.files.iter().collect();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));
        for f in sorted {
            h.update(f.path.as_bytes());
            h.update(b"\0");
            let mut fh = Sha256::new();
            fh.update(&f.bytes);
            h.update(fh.finalize());
            h.update(b"\0");
        }

        let out = h.finalize();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&out);
        arr
    }

    /// 包内声明"某个代码文件的摘要"，用于检测"签名后被篡改"。
    ///
    /// 返回 `(path, sha256_hex)` 的有序列表，与 [`Self::digest`] 覆盖同一集合。
    pub fn file_digests(&self) -> BTreeMap<String, [u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut out = BTreeMap::new();
        for f in &self.files {
            let mut h = Sha256::new();
            h.update(&f.bytes);
            let d = h.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&d);
            out.insert(f.path.clone(), arr);
        }
        out
    }

    /// 从任意字节构造一份包（加载器用）。
    ///
    /// 清单解析失败返回 `None`，调用方转成 `LoadError::BadManifest`。
    pub fn from_raw(
        plugin_id_hint: impl Into<PluginId>,
        manifest_raw: Vec<u8>,
        files: Vec<PackageFile>,
        signature: Option<PluginSignature>,
    ) -> Result<Self, serde_json::Error> {
        let manifest: PluginManifest = serde_json::from_slice(&manifest_raw)?;
        let plugin_id = PluginId::new(manifest.id.clone());
        let _ = plugin_id_hint.into(); // 保持签名兼容；权威 ID 是清单里的
        Ok(Self {
            plugin_id,
            manifest_raw,
            manifest,
            files,
            signature,
        })
    }
}

/// 校验结论。**不是** `Result`，因为"无效签名"是正常结果（→ 降级），不是错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum VerificationOutcome {
    /// 签名有效且未被吊销 → 可信插件。
    Valid {
        key_id: String,
        /// 校验时用的吊销列表版本。为 0 表示未装载吊销列表（见下）。
        revocation_version: u64,
    },
    /// 无签名。
    Unsigned,
    /// 签名存在但无效（摘要不符 / 密钥不匹配 / 签名格式错）。
    Invalid { reason: String },
    /// 签名有效但签名者已被吊销。**与 Invalid 走同一条降级路径。**
    Revoked { key_id: String },
    /// 吊销列表自身无法校验（fail-closed：全部视为不可信）。
    RevocationUnverifiable { reason: String },
    /// 算法不被支持（例如插件用 RSA 签名）。
    UnsupportedAlgorithm { algorithm: String },
}

impl VerificationOutcome {
    /// 是否可授予 Trusted。
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }

    /// 面向用户的说明。规格要求 UI 提示"未通过签名校验，运行在受限沙箱"。
    pub fn user_message(&self) -> &'static str {
        match self {
            Self::Valid { .. } => "",
            Self::Unsigned => "插件未签名，运行在受限沙箱",
            Self::Invalid { .. } => "插件签名无效，运行在受限沙箱",
            Self::Revoked { .. } => "插件签名已被吊销，运行在受限沙箱",
            Self::RevocationUnverifiable { .. } => "吊销列表无法校验，所有插件降级为受限沙箱",
            Self::UnsupportedAlgorithm { .. } => "插件签名算法不受支持，运行在受限沙箱",
        }
    }

    /// 诊断用的详细原因（进日志，不进 UI）。
    pub fn detail(&self) -> String {
        match self {
            Self::Valid { key_id, .. } => format!("signature valid, key_id={key_id}"),
            Self::Unsigned => "no signature".to_owned(),
            Self::Invalid { reason } => format!("invalid signature: {reason}"),
            Self::Revoked { key_id } => format!("signer {key_id} is revoked"),
            Self::RevocationUnverifiable { reason } => {
                format!("revocation list unverifiable: {reason}")
            }
            Self::UnsupportedAlgorithm { algorithm } => {
                format!("unsupported algorithm {algorithm}")
            }
        }
    }
}

/// 签名校验接口。
///
/// **实现必须是无状态的**：要求"每次沙箱单元重启时都重新校验"，如果把结果缓存起来
/// （尤其按 plugin_id 缓存），就会在热重载/吊销更新后给出过期结论。
pub trait SignatureVerifier: Send + Sync + 'static {
    /// 校验一个包。返回结论（永不返回 Err —— 失败是一种结论）。
    ///
    /// 实现内部需要：
    /// 1. 先校验吊销列表（若装载）；列表不可信 → `RevocationUnverifiable`（fail-closed）。
    /// 2. 无签名 → `Unsigned`。
    /// 3. 算法不支持 → `UnsupportedAlgorithm`。
    /// 4. 摘要 + 验签失败 → `Invalid`。
    /// 5. 签名者被吊销 → `Revoked`。
    /// 6. 全部通过 → `Valid`。
    fn verify(&self, package: &PluginPackage) -> VerificationOutcome;

    /// 当前吊销列表版本，供 UI 展示"吊销库是否已更新"。
    fn revocation_version(&self) -> u64;

    /// 热重载时替换吊销列表。返回 `false` 表示新列表自身验签失败，**保留旧列表**
    /// （不因为一次坏更新就丢掉整份吊销信息）。
    fn replace_revocation_list(&self, list: SignedRevocationList) -> bool;
}
