//! 外接 MCP（Model Context Protocol）客户端。
//!
//! 通过 `plugins/mcp.json` 配置外部 MCP 服务器（HTTP / Streamable-HTTP 形态）：
//!
//! ```json
//! { "servers": [ { "name": "my-server", "url": "http://127.0.0.1:8814/mcp" } ] }
//! ```
//!
//! 通信是标准 JSON-RPC 2.0 over HTTP POST：
//! 1. `initialize` → 2. `notifications/initialized`（通知，忽略响应）→ 3. `tools/list` / `tools/call`。
//!
//! 传输层用 [`host_http::post_json`]：https 任意主机，http 仅限本机回环。
//! 调用永远跑在后台线程（`LauncherModel::spawn_bg`），UI 线程零阻塞。

use std::path::Path;

/// 一个外接 MCP 服务器配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub url: String,
}

/// 从 `plugins_dir/mcp.json` 读取服务器配置。文件不存在或非法 → 空列表（不报错）。
pub fn load_servers(plugins_dir: &Path) -> Vec<McpServer> {
    let Ok(text) = std::fs::read_to_string(plugins_dir.join("mcp.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    v.get("servers")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    Some(McpServer {
                        name: s.get("name")?.as_str()?.to_owned(),
                        url: s.get("url")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 写出 mcp.json 模板（首次启动铺底，用户或 AI 往里填服务器）。
pub fn ensure_config_template(plugins_dir: &Path) {
    let p = plugins_dir.join("mcp.json");
    if p.exists() {
        return;
    }
    let _ = std::fs::write(
        &p,
        r#"{
  "servers": [
    { "name": "示例·本机 MCP", "url": "http://127.0.0.1:8814/mcp" }
  ]
}
"#,
    );
}

/// JSON-RPC 请求 → 解析响应。返回 `result` 字段；`error` 字段转成 Err。
fn rpc(url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let raw = serde_json::to_string(&body).map_err(|e| e.to_string())?;
    let (status, text) = host_http::post_json(
        url,
        &raw,
        std::time::Duration::from_secs(20),
    )?;
    if status >= 400 {
        return Err(format!("HTTP {status}：{}", truncate(&text, 200)));
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("响应不是合法 JSON：{e}"))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("未知错误");
        return Err(format!("MCP 错误：{msg}"));
    }
    Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null))
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// 取结果第一行（Toast 单行显示用）。
pub fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_owned()
}

/// initialize + tools/list，返回 `(工具名, 描述)`。
pub fn list_tools(server: &McpServer) -> Result<Vec<(String, String)>, String> {
    rpc(
        &server.url,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "Aether Launcher", "version": "1.0.0" }
        }),
    )?;
    // 通知不要求响应，但带 id 的 POST 也常被单端点服务器接受；失败不致命。
    let _ = rpc(&server.url, "notifications/initialized", serde_json::json!({}));

    let result = rpc(&server.url, "tools/list", serde_json::json!({}))?;
    let mut out = Vec::new();
    if let Some(tools) = result.get("tools").and_then(|t| t.as_array()) {
        for t in tools {
            let name = t
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_owned();
            let desc = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_owned();
            if !name.is_empty() {
                out.push((name, desc));
            }
        }
    }
    Ok(out)
}

/// tools/call，把结果压成一段可读文本（content 数组里的 text 拼接）。
pub fn call_tool(
    server: &McpServer,
    tool: &str,
    args: serde_json::Value,
) -> Result<String, String> {
    let result = rpc(
        &server.url,
        "tools/call",
        serde_json::json!({ "name": tool, "arguments": args }),
    )?;
    let mut parts = Vec::new();
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        for c in content {
            if let Some(text) = c.get("text").and_then(|t| t.as_str()) {
                parts.push(text.to_owned());
            }
        }
    }
    if parts.is_empty() {
        if result.get("isError").and_then(|b| b.as_bool()) == Some(true) {
            return Err("工具返回了错误（无文本内容）".to_owned());
        }
        let s = result.to_string();
        return Ok(first_n(&s, 400));
    }
    let joined = parts.join("\n");
    Ok(first_n(&joined, 800))
}

/// 截取前 n 个字符（拥有所有权的版本，Toast / 日志用）。
fn first_n(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => s[..i].to_owned(),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mcp_config() {
        let dir = std::env::temp_dir().join(format!("aether-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mcp.json"),
            r#"{"servers":[{"name":"a","url":"http://127.0.0.1:1"},{"name":"b","url":"https://x.dev/mcp"}]}"#,
        )
        .unwrap();
        let s = load_servers(&dir);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "a");
        assert_eq!(s[1].url, "https://x.dev/mcp");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_remote_plaintext() {
        let err = host_http::post_json(
            "http://evil.example.com/rpc",
            "{}",
            std::time::Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(err.contains("localhost"), "应拒绝远程明文：{err}");
    }
}
