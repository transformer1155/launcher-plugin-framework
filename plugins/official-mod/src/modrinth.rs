//! Modrinth API 客户端（只读搜索）。
//!
//! # 为什么只用 `ctx.http()` 而不是直接引 HTTP 栈
//!
//! 规格禁止引入框架没有的依赖，且进程模型是"单进程、无 socket"。
//! `common::HttpPort` 是宿主注入的抽象端口，本插件通过它发请求 ——
//! 真正的 TCP 连接由宿主持有，插件这一层看不到 socket。
//!
//! # 白名单
//!
//! 本模块只拼 `https://api.modrinth.com/...` 的 URL。实际强制点在宿主的
//! `AllowListedHttp`（`common::context`）：它会在发请求前比对 `plugin.json` 的
//! `network.allow_hosts`，域名不在白名单则直接拒绝，**不会走到网络**。
//!
//! 也就是说这里是"守规矩"，宿主那边是"强制守规矩"。两层都在。
//!
//! # 响应解析
//!
//! Modrinth 搜索返回 `{ "hits": [ { "project_id", "title", "description", ... } ] }`。
//! 本模块只取需要的三个字段，其余忽略 —— 上游加字段不会让解析失败（`serde` 默认忽略未知字段）。

use launcher_plugin_common::{HttpPort, PluginContext};
use serde::Deserialize;

use crate::store::now_rfc3339;

const BASE: &str = "https://api.modrinth.com/v2/search";

/// 允许的加载器值（与 `official-instance` 的 `Loader::ALL` 保持一致）。
///
/// 这里刻意**不依赖** `official-instance`：两个插件应能独立加载/卸载。
/// 重复一个三元素枚举的代价远小于引入跨插件依赖。
pub const LOADER_VALUES: [&str; 3] = ["vanilla", "fabric", "forge"];

/// 搜索请求。
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub loader: String,
    pub game_version: String,
    pub limit: u32,
}

/// 一条搜索结果（本插件对外暴露的形态）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SearchHit {
    pub project_id: String,
    pub name: String,
    pub description: String,
}

#[derive(Deserialize)]
struct RawResponse {
    #[serde(default)]
    hits: Vec<RawHit>,
}

#[derive(Deserialize)]
struct RawHit {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
}

/// 上游返回错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamError {
    /// 宿主没有注入网络能力（`ctx.http` 不可用）。
    NoNetwork,
    /// 请求被白名单拒绝（域名不在 `network.allow_hosts`）。
    HostNotAllowed(String),
    /// 网络层失败（连接超时、DNS、TLS…）。
    Transport(String),
    /// 非 2xx 状态码。
    HttpStatus { status: u16 },
    /// 响应体不是合法 JSON。
    BadJson(String),
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoNetwork => f.write_str("当前宿主未向插件提供网络能力"),
            Self::HostNotAllowed(h) => write!(f, "域名 `{h}` 不在插件的网络白名单内"),
            Self::Transport(m) => write!(f, "网络请求失败：{m}"),
            Self::HttpStatus { status } => write!(f, "Modrinth 返回 HTTP {status}"),
            Self::BadJson(m) => write!(f, "Modrinth 响应格式异常：{m}"),
        }
    }
}

impl std::error::Error for UpstreamError {}

/// 把搜索结果转成 URL。
///
/// # 转义
///
/// `query` 来自用户输入，必须百分号编码。不转义的话，一个含 `&` 的查询就能
/// 注入额外参数（例如把 `limit` 改掉）。这里手写最小转义（RFC 3986 unreserved 之外全转），
/// 不引入 URL 库。
fn build_url(req: &SearchRequest) -> String {
    // 加载器 → Modrinth 的 facet 语法：`[["categories:fabric"]]`
    // vanilla 不对应任何 category（Modrinth 里没有 "vanilla" 分类）。
    //
    // ★ 关键：**整个 facet 结构必须百分号编码**。
    // 实测：未编码的 `facets=[["categories:fabric"]]` 会被 Modrinth 判为 400 Bad Request
    // （方括号与引号在 query 里是非法字符）。编码后返回 200。
    // 这个 bug 用 mock HttpPort 测不出来 —— 因为 mock 不解析 URL 就返回固定 JSON。
    let facets_raw = if req.loader == "vanilla" {
        // 只用版本 facet
        format!("[[\"versions:{}\"]]", req.game_version)
    } else {
        format!(
            "[[\"categories:{}\"],[\"versions:{}\"]]",
            req.loader, req.game_version
        )
    };
    format!(
        "{BASE}?query={}&limit={}&facets={}",
        percent_encode(&req.query),
        req.limit,
        percent_encode(&facets_raw)
    )
}

/// RFC 3986 最小转义：`A-Z a-z 0-9 - _ . ~` 之外全部 `%XX`。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 执行搜索。
///
/// `http` 由调用方从 `ctx.http()` 取得 —— 这样"有没有网络能力"的判断集中在调用方，
/// 本函数只负责协议细节。
pub async fn search(
    ctx: &PluginContext,
    http: std::sync::Arc<dyn HttpPort>,
    req: &SearchRequest,
) -> Result<Vec<SearchHit>, UpstreamError> {
    let url = build_url(req);
    ctx.log_info(format!("Modrinth 搜索：{}", redact_url(&url)));

    let reply = http.get(url).await.map_err(|e| {
        // 宿主的 AllowListedHttp 在这里返回"白名单"字样；透传它的原因便于排查。
        if e.contains("白名单") {
            UpstreamError::HostNotAllowed(req_loader_hint(&e))
        } else {
            UpstreamError::Transport(e)
        }
    })?;

    if !(200..300).contains(&reply.status) {
        return Err(UpstreamError::HttpStatus {
            status: reply.status,
        });
    }

    let parsed: RawResponse =
        serde_json::from_slice(&reply.body).map_err(|e| UpstreamError::BadJson(e.to_string()))?;

    Ok(parsed
        .hits
        .into_iter()
        .map(|h| SearchHit {
            // Modrinth 的 `project_id` 通常就返回了；`slug` 作为兜底。
            project_id: if h.project_id.is_empty() {
                h.slug
            } else {
                h.project_id
            },
            name: h.title,
            description: h.description,
        })
        .filter(|h| !h.project_id.is_empty())
        .collect())
}

/// 从错误文本里尽量提取域名（用于给用户更准确的提示）。
fn req_loader_hint(err: &str) -> String {
    // 错误形如 "... 域名 `xxx` 不在 ...白名单..."，取反引号内内容。
    err.split('`').nth(1).unwrap_or("unknown").to_owned()
}

/// 日志里隐藏查询串细节（不记录用户搜什么，保护隐私）。
fn redact_url(url: &str) -> String {
    match url.split_once("query=") {
        Some((head, tail)) => {
            let after = tail.split('&').next().unwrap_or("");
            format!(
                "{head}query=<{} 字节>&{}",
                after.len(),
                tail.split('&').skip(1).collect::<Vec<_>>().join("&")
            )
        }
        None => url.to_owned(),
    }
}

/// 时间戳辅助（供调用方记录"何时搜索过"）。
pub fn search_timestamp() -> String {
    now_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(query: &str, loader: &str, ver: &str, limit: u32) -> SearchRequest {
        SearchRequest {
            query: query.to_owned(),
            loader: loader.to_owned(),
            game_version: ver.to_owned(),
            limit,
        }
    }

    #[test]
    fn url_escapes_query() {
        let u = build_url(&req("cobble & mon", "fabric", "1.20.1", 20));
        assert!(u.contains("cobble%20%26%20mon"), "特殊字符必须转义：{u}");
        assert!(!u.contains("cobble "), "原始空格不能出现");
        // 注入防护：`&` 被转义后不会多出参数
        assert_eq!(u.matches("&limit=").count(), 1);
    }

    #[test]
    fn url_facets_for_loader() {
        let u = build_url(&req("jei", "fabric", "1.20.1", 20));
        // facet 的方括号与引号必须被百分号编码，否则 Modrinth 返回 400。
        assert!(
            u.contains("categories%3Afabric"),
            "loader 应被编码进 facets：{u}"
        );
        assert!(u.contains("%5B%5B"), "方括号必须编码为 %5B：{u}");
        assert!(u.contains("%22"), "引号必须编码为 %22：{u}");
        assert!(!u.contains("[[\""), "不能出现裸的 facet 结构：{u}");
        assert!(u.contains("versions"), "{u}");
    }

    #[test]
    fn vanilla_has_no_loader_category() {
        let u = build_url(&req("jei", "vanilla", "1.20.1", 20));
        assert!(
            !u.contains("categories"),
            "vanilla 不应带 categories facet：{u}"
        );
        assert!(u.contains("versions"), "{u}");
        // 同样必须编码
        assert!(u.contains("%5B%5B"), "方括号必须编码：{u}");
    }

    #[test]
    fn facets_are_percent_encoded_end_to_end() {
        // 回归测试：真实 URL 里不得出现未编码的 `[` `]` `"`
        // （此前未编码导致 Modrinth 返回 400，mock 测试测不出来）。
        for loader in ["vanilla", "fabric", "forge"] {
            let u = build_url(&req("jei", loader, "1.20.1", 20));
            for bad in ['[', ']', '"', ' '] {
                assert!(
                    !u.contains(bad),
                    "URL 不应含未编码的 `{bad}`（loader={loader}）：{u}"
                );
            }
        }
    }

    #[test]
    fn percent_encode_roundtrip_safe() {
        assert_eq!(percent_encode("abc"), "abc");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("a&b"), "a%26b");
        assert_eq!(percent_encode("a/b"), "a%2Fb");
        // 非 ASCII 也要被编码（UTF-8 字节逐一转）
        assert!(percent_encode("中文").contains('%'));
    }

    #[test]
    fn parses_hits_and_falls_back_to_slug() {
        let body = br#"{"hits":[
            {"project_id":"P1","title":"JEI","description":"recipe viewer"},
            {"slug":"cobblemon","title":"Cobblemon","description":"pokemon"},
            {"project_id":"","slug":"","title":"broken","description":"x"}
        ]}"#;
        let parsed: RawResponse = serde_json::from_slice(body).unwrap();
        let hits: Vec<SearchHit> = parsed
            .hits
            .into_iter()
            .map(|h| SearchHit {
                project_id: if h.project_id.is_empty() {
                    h.slug
                } else {
                    h.project_id
                },
                name: h.title,
                description: h.description,
            })
            .filter(|h| !h.project_id.is_empty())
            .collect();
        assert_eq!(hits.len(), 2, "无 ID 的条目应被过滤");
        assert_eq!(hits[0].project_id, "P1");
        assert_eq!(hits[1].project_id, "cobblemon", "应回退到 slug");
    }

    #[test]
    fn tolerates_missing_hits_field() {
        let parsed: RawResponse = serde_json::from_slice(b"{}").unwrap();
        assert!(parsed.hits.is_empty());
    }

    #[test]
    fn redacts_query_in_logs() {
        let u = build_url(&req("secret stuff", "fabric", "1.20.1", 20));
        let r = redact_url(&u);
        assert!(!r.contains("secret"), "日志不应包含用户查询原文：{r}");
        assert!(r.contains("query=<"), "{r}");
    }
}
