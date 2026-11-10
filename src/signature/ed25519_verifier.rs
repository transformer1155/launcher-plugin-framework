//! Ed25519 验签实现。
//!
//! # 密钥布局（两把锁，互不通用）
//!
//! - **插件签名公钥**（`BuiltInKeys::plugin_keys`）：内置在启动器二进制。
//! - **吊销列表签名公钥**（`BuiltInKeys::revocation_keys`）：同样内置，但**另一把**。
//!
//! 分离的意义：泄露插件私钥的攻击者不能伪造吊销列表来"洗白"自己的插件；
//! 泄露吊销私钥的攻击者也不能伪造插件签名。两者同时泄露才失守。
//!
//! # 签名覆盖什么
//!
//! 签名是对 **`PluginPackage::digest()`**（32 字节）的 Ed25519 签名。
//! 该摘要已经覆盖「清单原文 + 每个文件的路径 + 每个文件内容的 sha256」，
//! 因此满足规格的"插件包整体签名，覆盖清单 + 所有代码文件"。
//!
//! 为什么签"摘要"而不是"原始文件流"：摘要计算与 JSON 序列化、文件遍历顺序、
//! 压缩方式全部无关（见 [`crate::signature::PluginPackage::digest`] 的确定性构造）。
//! 若直接签原始流，任何打包工具的顺序差异都会导致验签失败 —— 那是不可维护的。

use std::collections::BTreeMap;
use std::sync::RwLock;

use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};

use super::revocation::{RevocationList, RevocationStatus, SignedRevocationList};
use super::{PluginPackage, SignatureAlgorithm, SignatureVerifier, VerificationOutcome};

/// 内置的公钥集合。**启动器二进制内硬编码**（见 [`BuiltInKeys::from_hex`]）。
#[derive(Debug, Clone, Default)]
pub struct BuiltInKeys {
    /// key_id → 公钥 hex（48/64 位 base16 的 Ed25519 公钥，32 字节）。
    plugin_keys: BTreeMap<String, String>,
    /// key_id → 公钥 hex，用于吊销列表。
    revocation_keys: BTreeMap<String, String>,
}

impl BuiltInKeys {
    pub fn from_hex(
        plugin_keys: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
        revocation_keys: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        Self {
            plugin_keys: plugin_keys
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
            revocation_keys: revocation_keys
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    pub fn with_plugin_key(mut self, key_id: impl Into<String>, hex: impl Into<String>) -> Self {
        self.plugin_keys.insert(key_id.into(), hex.into());
        self
    }

    pub fn with_revocation_key(
        mut self,
        key_id: impl Into<String>,
        hex: impl Into<String>,
    ) -> Self {
        self.revocation_keys.insert(key_id.into(), hex.into());
        self
    }

    fn plugin_key(&self, key_id: &str) -> Option<&str> {
        self.plugin_keys.get(key_id).map(|s| s.as_str())
    }

    fn revocation_key(&self, key_id: &str) -> Option<&str> {
        self.revocation_keys.get(key_id).map(|s| s.as_str())
    }

    /// 有多少把内置插件公钥（诊断用，不暴露具体值）。
    pub fn plugin_key_count(&self) -> usize {
        self.plugin_keys.len()
    }
}

fn parse_key(hex_str: &str) -> Option<VerifyingKey> {
    let bytes = hex::decode(hex_str).ok()?;
    let arr: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&arr).ok()
}

/// 校验 Ed25519 签名是否覆盖 `message`。
fn verify_sig(hex_key: &str, signature_hex: &str, message: &[u8]) -> Result<(), String> {
    let key =
        parse_key(hex_key).ok_or_else(|| "内置公钥格式非法（应由启动器构建保证）".to_owned())?;
    let sig_bytes = hex::decode(signature_hex).map_err(|e| format!("签名不是合法 hex：{e}"))?;
    let arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("签名长度必须是 64 字节，实际 {}", sig_bytes.len()))?;
    let sig = Signature::from_bytes(&arr);
    key.verify(message, &sig)
        .map_err(|e| format!("验签不通过：{e}"))
}

/// 可信实现。
pub struct Ed25519Verifier {
    keys: BuiltInKeys,
    /// 当前吊销状态。
    status: RwLock<RevocationStatus>,
    /// 当前吊销列表（`NotLoaded` 时为空表）。
    list: RwLock<RevocationList>,
    /// 是否强制要求吊销列表已装载。
    ///
    /// 默认 **true**：规格说"吊销列表随启动器更新"，那它应当总是存在。
    /// 缺失时按 fail-closed 处理（全部降级），而不是默默认为"没人被吊销"。
    /// 如果产品后期确实需要"无吊销库"的发行版，可以显式关掉（见 open-questions）。
    require_revocation_list: bool,
}

impl Ed25519Verifier {
    pub fn new(keys: BuiltInKeys) -> Self {
        Self {
            keys,
            status: RwLock::new(RevocationStatus::NotLoaded),
            list: RwLock::new(RevocationList::empty()),
            require_revocation_list: true,
        }
    }

    /// 允许无吊销列表的发行版（谨慎使用）。
    pub fn without_required_revocation_list(mut self) -> Self {
        self.require_revocation_list = false;
        self
    }

    /// 装载吊销列表（启动时调用一次）。验签失败则保持 fail-closed。
    pub fn install_revocation_list(&self, signed: SignedRevocationList) -> bool {
        let signer = signed.signer_key_id.clone();
        let Some(hex_key) = self.keys.revocation_key(&signer) else {
            *self.status.write().expect("status rwlock poisoned") =
                RevocationStatus::Unverifiable {
                    reason: format!("签发者 {signer} 不是内置吊销公钥"),
                };
            return false;
        };
        let payload = signed.payload();
        if let Err(reason) = verify_sig(hex_key, &signed.signature_hex, &payload) {
            *self.status.write().expect("status rwlock poisoned") =
                RevocationStatus::Unverifiable { reason };
            return false;
        }
        let version = signed.version;
        let revoked = signed.revoked_key_ids.len();
        *self.list.write().expect("list rwlock poisoned") = signed.into_list();
        *self.status.write().expect("status rwlock poisoned") =
            RevocationStatus::Trusted { version, revoked };
        true
    }

    fn revocation_status(&self) -> RevocationStatus {
        self.status.read().expect("status rwlock poisoned").clone()
    }
}

impl SignatureVerifier for Ed25519Verifier {
    fn verify(&self, package: &PluginPackage) -> VerificationOutcome {
        // ---- 第 1 步：吊销列表自身是否可信（fail-closed 的入口） ----
        let status = self.revocation_status();
        match &status {
            RevocationStatus::Unverifiable { reason } => {
                // 无法判断谁被吊销 → 谁也不能被信任。
                return VerificationOutcome::RevocationUnverifiable {
                    reason: reason.clone(),
                };
            }
            RevocationStatus::NotLoaded => {
                if self.require_revocation_list {
                    return VerificationOutcome::RevocationUnverifiable {
                        reason: "启动器未装载吊销列表，无法判断签名者是否被吊销".to_owned(),
                    };
                }
                // 显式允许无吊销库的发行版：继续，但 revocation_version 记为 0。
            }
            RevocationStatus::Trusted { .. } => {}
        }
        let revocation_version = match &status {
            RevocationStatus::Trusted { version, .. } => *version,
            _ => 0,
        };

        // ---- 第 2 步：有没有签名 ----
        let Some(sig) = &package.signature else {
            return VerificationOutcome::Unsigned;
        };

        // ---- 第 3 步：算法是否受支持 ----
        if let SignatureAlgorithm::Unsupported(a) = &sig.algorithm {
            return VerificationOutcome::UnsupportedAlgorithm {
                algorithm: a.clone(),
            };
        }

        // ---- 第 4 步：验签（签的是包整体摘要） ----
        let digest = package.digest();
        let Some(key_hex) = self.keys.plugin_key(&sig.key_id) else {
            // key_id 不在内置集合：可能来自更新版本的启动器，也可能是伪造的。
            return VerificationOutcome::Invalid {
                reason: format!(
                    "签名者 {} 不在内置公钥集合中（可能来自更新版本的启动器）",
                    sig.key_id
                ),
            };
        };
        if let Err(reason) = verify_sig(key_hex, &hex::encode(&sig.bytes), &digest) {
            return VerificationOutcome::Invalid { reason };
        }

        // ---- 第 5 步：签名者对用户有效，但他被吊销了吗 ----
        // 注意顺序：先验签再查吊销。反过来的话，攻击者可以用被吊销的 key_id
        // 配上无效签名，得到一个信息量更小的结论（"被吊销"而非"签名无效"）。
        let revoked = self
            .list
            .read()
            .expect("list rwlock poisoned")
            .is_revoked(&sig.key_id);
        if revoked {
            return VerificationOutcome::Revoked {
                key_id: sig.key_id.clone(),
            };
        }

        VerificationOutcome::Valid {
            key_id: sig.key_id.clone(),
            revocation_version,
        }
    }

    fn revocation_version(&self) -> u64 {
        match self.revocation_status() {
            RevocationStatus::Trusted { version, .. } => version,
            _ => 0,
        }
    }

    fn replace_revocation_list(&self, list: SignedRevocationList) -> bool {
        // 热重载：新列表验签失败时**保留旧列表**（不清空状态），避免一次坏更新
        // 把已有吊销信息洗掉。注意与 `install_revocation_list` 的差别：
        // 初始化失败要 fail-closed（因为没有旧信息），热重载失败要保持旧结论。
        let signer = list.signer_key_id.clone();
        let Some(hex_key) = self.keys.revocation_key(&signer) else {
            return false;
        };
        if verify_sig(hex_key, &list.signature_hex, &list.payload()).is_err() {
            return false;
        }
        let version = list.version;
        let revoked = list.revoked_key_ids.len();
        *self.list.write().expect("list rwlock poisoned") = list.into_list();
        *self.status.write().expect("status rwlock poisoned") =
            RevocationStatus::Trusted { version, revoked };
        true
    }
}
