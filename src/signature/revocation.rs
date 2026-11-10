//! 吊销列表：**自身必须签名**。
//!
//! # 为什么吊销列表要签名
//!
//! 如果吊销列表可被本地篡改，攻击者只需把它清空就能让所有被吊销的插件恢复可信。
//! 因此列表用启动器内置的**另一把**公钥（与插件签名公钥分离）验签 ——
//! 密钥分离的意义：泄露插件私钥不能同时伪造吊销列表，反之亦然。
//!
//! # fail-closed 语义
//!
//! 列表验签失败时**不是**"忽略列表继续用旧的"，而是：所有插件都不可信（降级到沙箱）。
//! 理由：无法判断"谁被吊销"时，最安全的假设是"可能有人被吊销了"。
//! 但**不拒绝加载** —— 规格明确要求降级而非拒绝。

use serde::{Deserialize, Serialize};

/// 吊销列表的序列化形式（带签名）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedRevocationList {
    /// 列表版本。单调递增；启动器用内置版本判断"本地是否比内置旧"。
    pub version: u64,
    /// 签发时间（ISO 8601）。仅供展示，不参与信任决策。
    pub issued_at: String,
    /// 被吊销的公钥 ID。
    pub revoked_key_ids: Vec<String>,
    /// 豁免/说明文字，例如某 key 因事故吊销。
    #[serde(default)]
    pub notes: std::collections::BTreeMap<String, String>,
    /// 签发者公钥 ID（必须是内置吊销公钥之一）。
    pub signer_key_id: String,
    /// 对 `payload()` 的签名，hex 编码（Ed25519 64 字节）。
    pub signature_hex: String,
}

impl SignedRevocationList {
    /// 参与签名的载荷 = 去掉 `signature_hex` 与 `signer_key_id` 之外的**规范化 JSON**。
    ///
    /// 用"显式字段拼接"而不是 `serde_json::to_string(self)`，是因为后者会把
    /// `signature_hex` 自己也算进去（自指）。这里逐字段拼，顺序固定，与 JSON 键序无关。
    pub fn payload(&self) -> Vec<u8> {
        // 关键：`revoked_key_ids` 与 `notes` 必须确定性排序，否则同一份内容在
        // 不同机器上会得到不同载荷 → 验签失败。
        let mut keys = self.revoked_key_ids.clone();
        keys.sort();
        keys.dedup();

        let mut buf = Vec::new();
        buf.extend_from_slice(b"dsh-revocation-list-v1\0");
        buf.extend_from_slice(self.version.to_string().as_bytes());
        buf.push(0);
        buf.extend_from_slice(self.issued_at.as_bytes());
        buf.push(0);
        for k in &keys {
            buf.extend_from_slice(k.as_bytes());
            buf.push(0);
        }
        buf.push(b'\x01');
        for (k, v) in &self.notes {
            buf.extend_from_slice(k.as_bytes());
            buf.push(b'\x02');
            buf.extend_from_slice(v.as_bytes());
            buf.push(0);
        }
        buf
    }

    pub fn into_list(self) -> RevocationList {
        RevocationList {
            version: self.version,
            revoked_key_ids: self.revoked_key_ids,
        }
    }
}

/// 投影到信任判定只需要的那部分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationList {
    pub version: u64,
    pub revoked_key_ids: Vec<String>,
}

impl RevocationList {
    pub fn empty() -> Self {
        Self {
            version: 0,
            revoked_key_ids: Vec::new(),
        }
    }

    pub fn is_revoked(&self, key_id: &str) -> bool {
        self.revoked_key_ids.iter().any(|k| k == key_id)
    }
}

/// 校验吊销列表的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevocationStatus {
    /// 已装载且验签通过。
    Trusted { version: u64, revoked: usize },
    /// 未装载（启动器没带吊销列表）。**不算失败**，但也不提供吊销信息。
    NotLoaded,
    /// 已装载但自身验签失败 → fail-closed。
    Unverifiable { reason: String },
}

impl RevocationStatus {
    /// 是否可以安全地据此判定"未吊销"。
    pub fn can_attest_not_revoked(&self) -> bool {
        matches!(self, Self::Trusted { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_deterministic_and_order_independent() {
        let a = SignedRevocationList {
            version: 7,
            issued_at: "2026-01-01T00:00:00Z".into(),
            revoked_key_ids: vec!["k1".into(), "k2".into()],
            notes: Default::default(),
            signer_key_id: "r1".into(),
            signature_hex: String::new(),
        };
        let mut b = a.clone();
        b.revoked_key_ids = vec!["k2".into(), "k1".into()];
        assert_eq!(a.payload(), b.payload(), "载荷必须与列表顺序无关");
    }

    #[test]
    fn payload_ignores_signature_field() {
        let a = SignedRevocationList {
            version: 1,
            issued_at: "t".into(),
            revoked_key_ids: vec!["k".into()],
            notes: Default::default(),
            signer_key_id: "s".into(),
            signature_hex: "aaaa".into(),
        };
        let mut b = a.clone();
        b.signature_hex = "bbbb".into();
        assert_eq!(a.payload(), b.payload());
    }

    #[test]
    fn payload_includes_notes() {
        let mut a = SignedRevocationList {
            version: 1,
            issued_at: "t".into(),
            revoked_key_ids: vec![],
            notes: Default::default(),
            signer_key_id: "s".into(),
            signature_hex: String::new(),
        };
        let p1 = a.payload();
        a.notes.insert("k".into(), "compromised".into());
        assert_ne!(p1, a.payload(), "notes 篡改必须改变载荷");
    }

    #[test]
    fn is_revoked_lookup() {
        let l = RevocationList {
            version: 3,
            revoked_key_ids: vec!["old-key".into()],
        };
        assert!(l.is_revoked("old-key"));
        assert!(!l.is_revoked("good-key"));
    }
}
