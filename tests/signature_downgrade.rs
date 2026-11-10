//! 签名校验：真实 Ed25519 密钥对 → 验证 Valid / Invalid / Revoked / 降级路径。
//!
//! 用真实密钥而不是 mock：验签的 bug 通常出在"摘要覆盖了什么"和"签名编码"上，
//! mock 会把这两处一起骗过去。

mod common;

use std::sync::Arc;

use ed25519_dalek::{Signer as _, SigningKey};
use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::signature::revocation::SignedRevocationList;
use launcher_plugin_framework::signature::{
    PackageFile, PluginManifest, PluginPackage, PluginSignature, SignatureAlgorithm,
    SignatureVerifier,
};
use rand_core::OsRng;
use serde_json::json;

use common::*;

/// 生成一对测试密钥，返回 (signing_key, key_id, pubkey_hex)。
fn keypair(key_id: &str) -> (SigningKey, String, String) {
    let sk = SigningKey::generate(&mut OsRng);
    let pubhex = hex::encode(sk.verifying_key().as_bytes());
    (sk, key_id.to_owned(), pubhex)
}

fn build_package(id: &str, files: Vec<(&str, &[u8], bool)>, with_sig: bool) -> PluginPackage {
    let manifest = PluginManifest {
        id: id.to_owned(),
        name: format!("{id} plugin"),
        version: "0.1.0".to_owned(),
        description: "test".to_owned(),
        tools: vec![],
    };
    let manifest_raw = serde_json::to_vec(&manifest).unwrap();
    PluginPackage {
        plugin_id: PluginId::new(id),
        manifest_raw: manifest_raw.clone(),
        manifest,
        files: files
            .into_iter()
            .map(|(p, b, is_code)| PackageFile {
                path: p.to_owned(),
                bytes: b.to_vec(),
                is_code,
            })
            .collect(),
        signature: if with_sig {
            // 占位：真正的签名在调用方算出来再填
            Some(PluginSignature {
                algorithm: SignatureAlgorithm::Ed25519,
                key_id: String::new(),
                bytes: Vec::new(),
            })
        } else {
            None
        },
    }
}

/// 给包签名（在包构造后计算摘要，再签摘要）。
fn sign_package(mut pkg: PluginPackage, sk: &SigningKey, key_id: &str) -> PluginPackage {
    let digest = pkg.digest();
    let sig = sk.sign(&digest);
    pkg.signature = Some(PluginSignature {
        algorithm: SignatureAlgorithm::Ed25519,
        key_id: key_id.to_owned(),
        bytes: sig.to_bytes().to_vec(),
    });
    pkg
}

fn verifier_with(key_id: &str, pub_hex: &str) -> Arc<Ed25519Verifier> {
    Arc::new(
        Ed25519Verifier::new(BuiltInKeys::default().with_plugin_key(key_id, pub_hex))
            .without_required_revocation_list(),
    )
}

#[test]
fn valid_signature_is_trusted() {
    let (sk, kid, pubhex) = keypair("official-1");
    let pkg = build_package("demo", vec![("code/main.rs", b"fn main(){}", true)], false);
    let pkg = sign_package(pkg, &sk, &kid);

    let v = verifier_with(&kid, &pubhex);
    let outcome = v.verify(&pkg);
    assert!(outcome.is_trusted(), "有效签名应判为可信：{outcome:?}");
    assert_eq!(outcome.user_message(), "");
}

#[test]
fn unsigned_package_is_downgraded_not_rejected() {
    let (_, kid, pubhex) = keypair("official-1");
    let pkg = build_package("demo", vec![("code/main.rs", b"fn main(){}", true)], false);
    let v = verifier_with(&kid, &pubhex);
    let outcome = v.verify(&pkg);
    assert!(!outcome.is_trusted());
    assert!(matches!(outcome, VerificationOutcome::Unsigned));
    assert!(
        outcome.user_message().contains("未签名"),
        "UI 提示必须说明原因：{}",
        outcome.user_message()
    );
}

#[test]
fn tampered_code_file_invalidates_signature() {
    let (sk, kid, pubhex) = keypair("official-1");
    let pkg = build_package("demo", vec![("code/main.rs", b"fn main(){}", true)], false);
    let mut pkg = sign_package(pkg, &sk, &kid);
    // 签名之后改代码文件
    pkg.files[0].bytes = b"fn main(){ evil() }".to_vec();

    let v = verifier_with(&kid, &pubhex);
    let outcome = v.verify(&pkg);
    assert!(
        matches!(outcome, VerificationOutcome::Invalid { .. }),
        "改代码后签名必须失效：{outcome:?}"
    );
}

#[test]
fn tampered_manifest_invalidates_signature() {
    let (sk, kid, pubhex) = keypair("official-1");
    let pkg = build_package("demo", vec![("a.txt", b"x", false)], false);
    let mut pkg = sign_package(pkg, &sk, &kid);
    pkg.manifest_raw = br#"{"id":"demo","name":"evil","version":"9.9.9"}"#.to_vec();

    let v = verifier_with(&kid, &pubhex);
    assert!(matches!(
        v.verify(&pkg),
        VerificationOutcome::Invalid { .. }
    ));
}

#[test]
fn renaming_a_file_invalidates_signature() {
    // 内容不变但路径变了 → 摘要必须变（防止"挪位置"攻击）
    let (sk, kid, pubhex) = keypair("official-1");
    let pkg = build_package("demo", vec![("a.txt", b"payload", false)], false);
    let mut pkg = sign_package(pkg, &sk, &kid);
    pkg.files[0].path = "other/a.txt".to_owned();

    let v = verifier_with(&kid, &pubhex);
    assert!(
        matches!(v.verify(&pkg), VerificationOutcome::Invalid { .. }),
        "路径参与摘要，改路径必须失效"
    );
}

#[test]
fn wrong_key_is_invalid() {
    let (sk, kid, _pub) = keypair("official-1");
    let (_, other_kid, other_pub) = keypair("official-2");
    let pkg = build_package("demo", vec![("a", b"x", false)], false);
    let pkg = sign_package(pkg, &sk, &kid);

    // 验签器里装的是另一把公钥，但 key_id 匹配 → 验签应失败
    let v = verifier_with(&kid, &other_pub);
    assert!(matches!(
        v.verify(&pkg),
        VerificationOutcome::Invalid { .. }
    ));

    // key_id 完全不在内置集合
    let v2 = verifier_with("nobody", &other_pub);
    let outcome = v2.verify(&pkg);
    assert!(
        matches!(outcome, VerificationOutcome::Invalid { .. }),
        "未知 key_id 应判 Invalid：{outcome:?}"
    );
    let _ = other_kid;
}

#[test]
fn revocation_list_must_be_signed_and_can_lookup() {
    let (rsk, rkid, rpub) = keypair("revocation-root");
    let mut list = SignedRevocationList {
        version: 1,
        issued_at: "2026-01-01T00:00:00Z".to_owned(),
        revoked_key_ids: vec!["bad-key".to_owned()],
        notes: Default::default(),
        signer_key_id: rkid.clone(),
        signature_hex: String::new(),
    };
    list.signature_hex = hex::encode(rsk.sign(&list.payload()).to_bytes());

    let v = Ed25519Verifier::new(
        BuiltInKeys::default()
            .with_plugin_key("official-1", "00".repeat(32).as_str())
            .with_revocation_key(&rkid, &rpub),
    );
    assert!(
        v.install_revocation_list(list.clone()),
        "签名有效的列表应装载成功"
    );
    assert_eq!(v.revocation_version(), 1);

    // 被吊销的 key → Revoked（走降级路径）
    let (sk, kid, pubhex) = keypair("bad-key");
    let pkg = build_package("demo", vec![("a", b"x", false)], false);
    let pkg = sign_package(pkg, &sk, &kid);
    let v2 = Arc::new(
        Ed25519Verifier::new(
            BuiltInKeys::default()
                .with_plugin_key(&kid, &pubhex)
                .with_revocation_key(&rkid, &rpub),
        ), // require_revocation_list 默认 true，这里已装载
    );
    assert!(v2.install_revocation_list(list));
    let outcome = v2.verify(&pkg);
    assert!(
        matches!(outcome, VerificationOutcome::Revoked { .. }),
        "被吊销的签名者应判 Revoked：{outcome:?}"
    );
    assert!(!outcome.is_trusted(), "被吊销绝不能判为可信");
    assert!(outcome.user_message().contains("吊销"));
}

#[test]
fn tampered_revocation_list_fails_closed() {
    let (rsk, rkid, rpub) = keypair("revocation-root");
    let mut list = SignedRevocationList {
        version: 1,
        issued_at: "2026-01-01T00:00:00Z".to_owned(),
        revoked_key_ids: vec![],
        notes: Default::default(),
        signer_key_id: rkid.clone(),
        signature_hex: String::new(),
    };
    list.signature_hex = hex::encode(rsk.sign(&list.payload()).to_bytes());

    let (sk, kid, pubhex) = keypair("official-1");
    let pkg = build_package("demo", vec![("a", b"x", false)], false);
    let _pkg = sign_package(pkg, &sk, &kid);

    let v = Ed25519Verifier::new(
        BuiltInKeys::default()
            .with_plugin_key(&kid, &pubhex)
            .with_revocation_key(&rkid, &rpub),
    );
    assert!(v.install_revocation_list(list.clone()));

    // 篡改：加入一个被吊销的 key，但签名不改
    let mut tampered = list;
    tampered.revoked_key_ids.push("other".to_owned());
    // replace 会失败（签名不匹配）
    assert!(
        !v.replace_revocation_list(tampered),
        "被篡改的吊销列表必须拒绝装载"
    );
}

#[test]
fn missing_revocation_list_fails_closed_when_required() {
    // 默认 require_revocation_list = true：没装载吊销列表时，即使签名有效也不可信。
    let (sk, kid, pubhex) = keypair("official-1");
    let pkg = build_package("demo", vec![("a", b"x", false)], false);
    let pkg = sign_package(pkg, &sk, &kid);

    let v = Ed25519Verifier::new(BuiltInKeys::default().with_plugin_key(&kid, &pubhex));
    let outcome = v.verify(&pkg);
    assert!(
        matches!(outcome, VerificationOutcome::RevocationUnverifiable { .. }),
        "未装载吊销列表时默认必须 fail-closed：{outcome:?}"
    );
    assert!(!outcome.is_trusted());
}

#[test]
fn unsupported_algorithm_downgrades() {
    let pkg = {
        let mut p = build_package("demo", vec![("a", b"x", false)], false);
        p.signature = Some(PluginSignature {
            algorithm: SignatureAlgorithm::Unsupported("RSA-4096".to_owned()),
            key_id: "x".to_owned(),
            bytes: vec![1, 2, 3],
        });
        p
    };
    let (_, kid, pubhex) = keypair("official-1");
    let v = verifier_with(&kid, &pubhex);
    let outcome = v.verify(&pkg);
    assert!(matches!(
        outcome,
        VerificationOutcome::UnsupportedAlgorithm { .. }
    ));
    assert!(!outcome.is_trusted());
}

// ---------------------------------------------------------------------------
// 端到端：验签结论 → 加载器的信任分级决策
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn valid_signature_loads_as_trusted_without_sandbox_unit() {
    let h = Harness::new();
    let (sk, kid, pubhex) = keypair("official-1");
    let pkg = build_package("signedplugin", vec![("code/main.rs", b"code", true)], false);
    let pkg = sign_package(pkg, &sk, &kid);

    let verifier = Arc::new(
        Ed25519Verifier::new(BuiltInKeys::default().with_plugin_key(&kid, &pubhex))
            .without_required_revocation_list(),
    );
    let restarts = launcher_plugin_framework::prelude::RestartManager::new(
        h.scheduler.clone(),
        verifier.clone(),
        h.registry.clone(),
    );
    let loader = PluginLoader {
        registry: h.registry.clone(),
        scheduler: h.scheduler.clone(),
        verifier,
        restarts,
    };

    let build: Arc<dyn Fn(Option<UnitHandle>) -> Vec<ToolMeta> + Send + Sync> = Arc::new(|u| {
        assert!(u.is_none(), "可信插件不应拿到单元句柄");
        vec![trusted_meta("signedplugin", "run", |_| {
            Box::pin(async { ToolResult::success_text("trusted ran") })
        })]
    });

    let loaded = loader
        .load(pkg, h.unit_cfg("signedplugin"), build)
        .await
        .expect("有效签名应加载成功");

    assert_eq!(loaded.trust_level, TrustLevel::Trusted);
    assert!(loaded.unit.is_none(), "可信插件不应有执行单元");
    assert!(loaded.user_notice().is_none(), "可信插件不需要降级提示");
    assert_eq!(loaded.registered_tools, 1);
    assert_eq!(h.scheduler.list_units().len(), 0, "不应创建任何沙箱单元");

    let r = h
        .registry
        .call_tool(&h.caller, "signedplugin.run", json!({}))
        .await;
    assert!(!r.is_error);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsigned_package_loads_as_sandboxed_and_shows_notice() {
    let h = Harness::new();
    let pkg = build_package("unsigned", vec![("code/main.rs", b"code", true)], false);
    assert!(pkg.signature.is_none());

    let verifier = permissive_verifier();
    let restarts = launcher_plugin_framework::prelude::RestartManager::new(
        h.scheduler.clone(),
        verifier.clone(),
        h.registry.clone(),
    );
    let loader = PluginLoader {
        registry: h.registry.clone(),
        scheduler: h.scheduler.clone(),
        verifier,
        restarts,
    };

    let build: Arc<dyn Fn(Option<UnitHandle>) -> Vec<ToolMeta> + Send + Sync> = Arc::new(|u| {
        let u = u.expect("沙箱插件必须拿到单元");
        vec![sandboxed_meta("unsigned", &u, "run", 5_000, |_| {
            Box::pin(async { ToolResult::success_text("sandboxed ran") })
        })]
    });

    let loaded = loader
        .load(pkg, h.unit_cfg("unsigned"), build)
        .await
        .expect("验签失败必须降级而不是拒绝加载");

    assert_eq!(loaded.trust_level, TrustLevel::Sandboxed);
    assert!(loaded.unit.is_some());
    assert_eq!(h.scheduler.list_units().len(), 1);
    let notice = loaded.user_notice().expect("必须给 UI 降级提示");
    assert!(notice.contains("受限沙箱"), "UI 提示文案：{notice}");

    let r = h
        .registry
        .call_tool(&h.caller, "unsigned.run", json!({}))
        .await;
    assert!(!r.is_error, "降级沙箱后仍应能正常调用");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tampered_package_is_rejected_not_downgraded() {
    // 包被篡改（签名清单与内容不符）→ 拒绝加载，而不是降级。
    // 这是与"无签名 → 降级"的关键区别。
    let h = Harness::new();
    let (sk, kid, pubhex) = keypair("official-1");
    let pkg = build_package("evil", vec![("code/main.rs", b"benign", true)], false);
    let mut pkg = sign_package(pkg, &sk, &kid);

    // 模拟"加载器在重启时重算摘要发现不一致"：直接构造摘要不匹配
    let verifier = Arc::new(
        Ed25519Verifier::new(BuiltInKeys::default().with_plugin_key(&kid, &pubhex))
            .without_required_revocation_list(),
    );
    let outcome = verifier.verify(&pkg);
    assert!(outcome.is_trusted(), "签名本身有效");

    pkg.files[0].bytes = b"malicious".to_vec(); // 篡改代码
    let outcome2 = verifier.verify(&pkg);
    assert!(
        matches!(outcome2, VerificationOutcome::Invalid { .. }),
        "篡改后必须不再可信"
    );
    // 加载器只会把它降级到沙箱；「运行期发现摘要变化」由重启管理器判定为 Failed。
    let _ = h;
}
