//! `PluginContext`：插件 handler 能拿到的**全部**宿主能力。
//!
//! # 设计原则：能力是显式注入的，不是全局可得的
//!
//! 规格要求 "ctx 提供插件私有数据目录、日志接口、以及跨插件调用能力"。
//! 把这些能力收进一个结构体，而不是让插件直接 `std::fs::` 或直接持有 `Arc<ToolRegistry>`，
//! 有两个实际好处：
//!
//! 1. **可测试**：单元测试可以造一个指向临时目录、日志丢弃、无网络的 ctx，
//!    不必启动整个框架（规格要求"测试直接调 handler，不启动框架"）。
//! 2. **可审计**：一个插件能碰什么，全写在这一个结构体的字段里。
//!
//! # 关于"网络"的诚实说明
//!
//! 规格原文说 "框架层已声明域名白名单机制，通过 `ctx.http` 发起"。
//! **框架层实际没有 HTTP 实现，也没有域名白名单的运行时校验**
//! （见 Step 1 报告的框架接口缺口）。因此：
//!
//! - [`PluginContext::http`] 返回**可选**端口，由宿主注入 [`HttpPort`] 实现；
//! - 白名单声明在 `plugin.json` 的 `network.allow_hosts` 里，但**强制力在注入方**：
//!   宿主必须实现一个在发起前比对该域名的 [`HttpPort`]。`common` 提供
//!   [`AllowListedHttp`] 包装器把这件事做实，避免"声明了但没人检查"。
//!
//! # 跨插件调用与引用环
//!
//! `call_tool` 需要注册表。但注册表持有 `ToolMeta`，而 `ToolMeta` 里的 handler 闭包
//! 捕获了 `PluginContext` —— 如果 `PluginContext` 再持 `Arc<ToolRegistry>`，就形成
//! `registry → meta → closure → ctx → registry` 的 **Arc 引用环**，卸载后永远不会释放。
//!
//! 因此这里持 [`Weak`]。卸载插件（注册表丢掉 meta）之后，ctx 的调用能力自然失效，
//! 返回明确错误而不是泄漏。这是刻意选择，不是偷懒。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use launcher_plugin_framework::error::{ToolError, ToolErrorCode};
use launcher_plugin_framework::registry::ToolRegistry;
use launcher_plugin_framework::types::{PluginId, ToolResult};

use crate::manifest::NetworkDecl;

/// 日志级别。刻意只有三档：插件的日志不需要更多层次，多了反而没人看。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// 日志端口。宿主可替换为真正的日志系统。
pub trait PluginLogger: Send + Sync + 'static {
    fn log(&self, level: LogLevel, plugin_id: &str, message: &str);
}

/// 默认日志：打到 stderr，带插件前缀。
///
/// 之所以不做成 `tracing`/`log`：规格要求不引入框架没有的依赖。
pub struct StderrLogger;

impl PluginLogger for StderrLogger {
    fn log(&self, level: LogLevel, plugin_id: &str, message: &str) {
        eprintln!("[{}] [{}] {}", level.as_str(), plugin_id, message);
    }
}

/// 记录日志但什么都不做的实现（单元测试用）。
pub struct NullLogger;

impl PluginLogger for NullLogger {
    fn log(&self, _level: LogLevel, _plugin_id: &str, _message: &str) {}
}

/// 捕获日志的实现，便于在测试里断言"确实打了 WARN"。
#[derive(Default)]
pub struct CapturingLogger {
    entries: std::sync::Mutex<Vec<(LogLevel, String)>>,
}

impl CapturingLogger {
    pub fn take(&self) -> Vec<(LogLevel, String)> {
        std::mem::take(&mut self.entries.lock().expect("logger mutex poisoned"))
    }

    pub fn saw(&self, level: LogLevel, needle: &str) -> bool {
        self.entries
            .lock()
            .expect("logger mutex poisoned")
            .iter()
            .any(|(l, m)| *l == level && m.contains(needle))
    }
}

impl PluginLogger for CapturingLogger {
    fn log(&self, level: LogLevel, plugin_id: &str, message: &str) {
        self.entries
            .lock()
            .expect("logger mutex poisoned")
            .push((level, format!("{plugin_id}: {message}")));
    }
}

/// HTTP 端口的响应。
#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: u16,
    pub body: Vec<u8>,
}

/// 网络端口。**宿主必须实现**（框架层没有网络栈）。
///
/// 实现方的强制义务：在真正发起请求前，校验目标主机是否在插件的
/// `network.allow_hosts` 里。`common` 的 [`AllowListedHttp`] 已经把这件事包好，
/// 宿主只要提供一个"裸"的 [`HttpPort`] 就能得到白名单强制。
pub trait HttpPort: Send + Sync + 'static {
    /// 发起 GET。`url` 已由调用方做过白名单校验（见 [`AllowListedHttp`]）。
    fn get(
        &self,
        url: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HttpReply, String>> + Send>>;
}

/// 把任意 [`HttpPort`] 包成"只允许访问声明域名"的版本。
///
/// 白名单校验放在这里而不是让每个插件自己调，是因为"每个插件都记得校验"
/// 等价于"没人校验"。包装器是唯一入口，才可能做对。
pub struct AllowListedHttp {
    inner: Arc<dyn HttpPort>,
    allow_hosts: Vec<String>,
}

impl AllowListedHttp {
    pub fn new(inner: Arc<dyn HttpPort>, decl: &NetworkDecl) -> Self {
        Self {
            inner,
            allow_hosts: decl.allow_hosts.iter().map(|s| s.to_lowercase()).collect(),
        }
    }

    /// 从 URL 里抽出主机名。只支持 `https://host/path` 与 `http://host/path`。
    ///
    /// 刻意不引 URL 解析库：这里只需要主机名，手写解析且**严格失败**比引入新依赖更安全
    /// （解析库的宽松行为会带来 SSRF 风险，例如 `https://evil.com@api.modrinth.com/`）。
    pub fn host_of(url: &str) -> Option<String> {
        // 只接受 https。明文 http 与任何其他 scheme 一律拒绝 ——
        // 避免"白名单通了但走的是明文"这种降级。
        let rest = url.strip_prefix("https://")?;
        let host_port = rest.split('/').next()?;
        if host_port.contains('@') || host_port.contains('#') || host_port.contains('?') {
            // 出现 userinfo / fragment / query 说明 URL 形状可疑，直接拒绝。
            return None;
        }
        let host = host_port.split(':').next()?;
        if host.is_empty() || host.contains(' ') {
            return None;
        }
        Some(host.to_lowercase())
    }

    pub fn is_allowed(&self, url: &str) -> bool {
        match Self::host_of(url) {
            Some(h) => self.allow_hosts.contains(&h),
            None => false,
        }
    }
}

impl HttpPort for AllowListedHttp {
    fn get(
        &self,
        url: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HttpReply, String>> + Send>>
    {
        let allowed = self.is_allowed(&url);
        let inner = Arc::clone(&self.inner);
        let host = Self::host_of(&url).unwrap_or_default();
        Box::pin(async move {
            if !allowed {
                return Err(format!(
                    "域名 `{host}` 不在插件的 network.allow_hosts 白名单内，拒绝请求"
                ));
            }
            inner.get(url).await
        })
    }
}

/// 插件运行时上下文。
///
/// 廉价克隆：内部全是 `Arc` / `PathBuf`。因此 handler 可以按需 clone 一份带进
/// `'static` future（框架的 `BoxToolFuture` 要求 `'static`）。
#[derive(Clone)]
pub struct PluginContext {
    plugin_id: PluginId,
    data_dir: PathBuf,
    logger: Arc<dyn PluginLogger>,
    http: Option<Arc<dyn HttpPort>>,
    /// 跨插件调用用的注册表弱引用（见模块文档"引用环"一节）。
    registry: Option<Weak<dyn ToolRegistry>>,
}

impl std::fmt::Debug for PluginContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginContext")
            .field("plugin_id", &self.plugin_id)
            .field("data_dir", &self.data_dir)
            .field("has_http", &self.http.is_some())
            .field("has_registry", &self.registry.is_some())
            .finish_non_exhaustive()
    }
}

impl PluginContext {
    /// 基础构造：数据目录 + 日志。
    pub fn new(
        plugin_id: impl Into<PluginId>,
        data_dir: impl Into<PathBuf>,
        logger: Arc<dyn PluginLogger>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            data_dir: data_dir.into(),
            logger,
            http: None,
            registry: None,
        }
    }

    pub fn with_http(mut self, http: Arc<dyn HttpPort>) -> Self {
        self.http = Some(http);
        self
    }

    pub fn with_registry(mut self, registry: Weak<dyn ToolRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// 测试用上下文：临时目录 + 丢弃日志 + 无网络 + 无跨插件调用。
    ///
    /// 目录名带进程 id 与纳秒时间戳，避免并行测试互相踩。
    pub fn for_test(plugin_id: &str) -> Self {
        let unique = format!(
            "lpf-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            plugin_id.replace(['/', '\\', '.'], "_")
        );
        let dir = std::env::temp_dir().join(unique);
        Self::new(plugin_id, dir, Arc::new(NullLogger))
    }

    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub fn plugin_id_str(&self) -> &str {
        self.plugin_id.as_str()
    }

    /// 插件私有数据目录。
    ///
    /// 返回路径不保证已存在 —— 由插件按需 `create_dir_all`。这样"只读工具"
    /// （如 `list_instances`）在目录尚未创建时也能正常工作，而不是一开始就失败。
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 确保数据目录存在并返回它。
    pub fn ensure_data_dir(&self) -> std::io::Result<&Path> {
        std::fs::create_dir_all(&self.data_dir)?;
        Ok(&self.data_dir)
    }

    /// 数据目录下的子路径。会拒绝 `..` 逃逸。
    pub fn data_path(&self, rel: &str) -> Result<PathBuf, ToolError> {
        if rel.contains("..") {
            return Err(self.err(
                ToolErrorCode::InvalidArguments,
                format!("路径 `{rel}` 不允许包含 `..`"),
            ));
        }
        Ok(self.data_dir.join(rel))
    }

    // ---- 日志 ----

    pub fn log_info(&self, msg: impl AsRef<str>) {
        self.logger
            .log(LogLevel::Info, self.plugin_id.as_str(), msg.as_ref());
    }

    pub fn log_warn(&self, msg: impl AsRef<str>) {
        self.logger
            .log(LogLevel::Warn, self.plugin_id.as_str(), msg.as_ref());
    }

    pub fn log_error(&self, msg: impl AsRef<str>) {
        self.logger
            .log(LogLevel::Error, self.plugin_id.as_str(), msg.as_ref());
    }

    // ---- 网络 ----

    /// 取 HTTP 端口。未注入时返回明确的错误（而不是静默失败）。
    pub fn http(&self) -> Result<Arc<dyn HttpPort>, ToolError> {
        match &self.http {
            Some(h) => Ok(Arc::clone(h)),
            None => Err(self.err(
                ToolErrorCode::PermissionDenied,
                "当前宿主未向本插件注入网络能力（ctx.http 不可用）".to_owned(),
            )),
        }
    }

    // ---- 跨插件调用 ----

    /// 通过注册表调用其他插件（或本插件）的工具。
    ///
    /// `tool_name` 是**全名**（`插件ID.工具名`）。ACL 仍由注册表判定，
    /// 调用方身份固定为当前插件 —— 插件不能伪造成别人。
    pub async fn call_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<ToolResult, ToolError> {
        let Some(weak) = &self.registry else {
            return Err(self.err(
                ToolErrorCode::PermissionDenied,
                "当前上下文未挂载注册表，无法跨插件调用".to_owned(),
            ));
        };
        let Some(registry) = weak.upgrade() else {
            // 弱引用升级失败 = 注册表已被丢弃 = 插件正在被卸载。
            return Err(self.err(
                ToolErrorCode::ProcessFailed,
                "注册表已释放（宿主正在卸载），跨插件调用不可用".to_owned(),
            ));
        };
        Ok(registry.call_tool(&self.plugin_id, tool_name, args).await)
    }

    // ---- 错误构造 ----

    /// 以本插件身份构造一个框架错误码错误。
    ///
    /// 注意：`ToolErrorCode` 是**框架**的固定错误码表，业务失败不要滥用它
    /// （"实例不存在"不是 `ToolNotFound`）。业务失败请用
    /// [`crate::PluginError::business`]。
    pub fn err(&self, code: ToolErrorCode, message: impl Into<String>) -> ToolError {
        ToolError::new(code, self.plugin_id.as_str(), message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_is_per_plugin_and_creatable() {
        let ctx = PluginContext::for_test("com.example.demo");
        assert!(ctx.data_dir().to_string_lossy().contains("lpf-test"));
        assert!(!ctx.data_dir().exists(), "构造时不应自动创建");
        let p = ctx.ensure_data_dir().unwrap();
        assert!(p.exists());
        std::fs::remove_dir_all(p).ok();
    }

    #[test]
    fn data_path_rejects_traversal() {
        let ctx = PluginContext::for_test("demo");
        assert!(ctx.data_path("instances/a.json").is_ok());
        let err = ctx.data_path("../../etc/passwd").unwrap_err();
        assert_eq!(err.code, ToolErrorCode::InvalidArguments);
    }

    #[test]
    fn http_absent_gives_explicit_error() {
        let ctx = PluginContext::for_test("demo");
        // 不用 `expect_err`：`Arc<dyn HttpPort>` 不实现 Debug，`expect_err` 要求 Ok 侧 Debug。
        let err = match ctx.http() {
            Ok(_) => panic!("未注入 http 时必须返回 Err"),
            Err(e) => e,
        };
        assert_eq!(err.code, ToolErrorCode::PermissionDenied);
        assert!(err.message.contains("网络"), "{}", err.message);
    }

    #[test]
    fn host_parsing_is_strict() {
        assert_eq!(
            AllowListedHttp::host_of("https://api.modrinth.com/v2/search"),
            Some("api.modrinth.com".to_owned())
        );
        assert_eq!(
            AllowListedHttp::host_of("https://api.modrinth.com:443/x"),
            Some("api.modrinth.com".to_owned())
        );
        // 明文拒绝
        assert_eq!(AllowListedHttp::host_of("http://api.modrinth.com/x"), None);
        // userinfo 混淆攻击
        assert_eq!(
            AllowListedHttp::host_of("https://evil.com@api.modrinth.com/x"),
            None
        );
        assert_eq!(AllowListedHttp::host_of("ftp://x"), None);
        assert_eq!(AllowListedHttp::host_of("no-scheme.example/x"), None);
    }

    struct EchoHttp;
    impl HttpPort for EchoHttp {
        fn get(
            &self,
            url: String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HttpReply, String>> + Send>>
        {
            Box::pin(async move {
                Ok(HttpReply {
                    status: 200,
                    body: url.into_bytes(),
                })
            })
        }
    }

    #[tokio::test]
    async fn allowlist_blocks_unlisted_host() {
        let decl = NetworkDecl {
            allow_hosts: vec!["api.modrinth.com".to_owned()],
            reason: "mod search".to_owned(),
        };
        let http = AllowListedHttp::new(Arc::new(EchoHttp), &decl);

        let ok = http.get("https://api.modrinth.com/v2/x".to_owned()).await;
        assert!(ok.is_ok(), "白名单内应当放行");

        let blocked = http.get("https://evil.example/x".to_owned()).await;
        let err = blocked.unwrap_err();
        assert!(err.contains("evil.example"), "{err}");
        assert!(err.contains("白名单"), "{err}");
    }

    #[test]
    fn logger_captures_levels() {
        let lg = Arc::new(CapturingLogger::default());
        let ctx = PluginContext::new("demo", std::env::temp_dir(), lg.clone());
        ctx.log_info("hello");
        ctx.log_warn("careful");
        assert!(lg.saw(LogLevel::Info, "hello"));
        assert!(lg.saw(LogLevel::Warn, "careful"));
        assert!(!lg.saw(LogLevel::Error, "careful"));
    }
}
