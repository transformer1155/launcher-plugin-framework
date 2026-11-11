//! 声明式页面插件：每个标签页 = 磁盘上的一份 `page.json`。
//!
//! # 为什么是 JSON 而不是动态库
//!
//! 框架约束是"单进程单二进制、无动态加载"——编译型插件改一行代码就要重编译整个
//! 启动器。声明式页面插件把「页面长什么样、绑定了哪些动作」放进数据：
//!
//! - **热加载**：改文件 → 下一次目录扫描即生效，无需重启；
//! - **AI 可写**：AI（或用户）产出一份 JSON 写进 `plugins/pages/<id>/page.json`
//!   就是新标签页——这正是「允许用户通过 AI 直接写插件」的落点；
//! - **能力收敛**：JSON 能表达的是「展示 + 绑定既有工具动作」，无法执行任意代码，
//!   天然沙箱。
//!
//! # 页面描述格式（v1）
//!
//! ```json
//! {
//!   "id": "user.server-status",
//!   "name": "服务器状态",
//!   "icon": "◆",
//!   "widgets": [
//!     { "type": "heading", "text": "服务器状态" },
//!     { "type": "caption", "text": "说明文字" },
//!     { "type": "section", "text": "区块标题" },
//!     { "type": "text",    "text": "普通文本" },
//!     { "type": "keyvalue", "rows": [["版本", "Aether Launcher"]] },
//!     { "type": "list", "bind": "instances" },
//!     { "type": "progress", "bind": "asset" },
//!     { "type": "spacer", "h": 12 },
//!     { "type": "button", "text": "启动游戏", "kind": "primary",
//!       "action": { "kind": "launch" } },
//!     { "type": "button", "text": "问 MCP",
//!       "action": { "kind": "mcp", "server": "my-server", "tool": "query", "args": {} } }
//!   ]
//! }
//! ```
//!
//! 动作（`action.kind`）：`toast` / `nav` / `refresh` / `launch` / `mcp`。
//! 列表绑定（`list.bind`）：`instances` / `accounts` / `downloads` / `logs` /
//! `plugins` / `mcp_tools` / `catalog`。

use std::path::Path;

use serde::Deserialize;
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Rect, Theme, Ui, WidgetId};

use crate::domain::{LauncherModel, ToastKind};
use crate::shell::{section, Page};

// ---------------------------------------------------------------------------
// 声明模型
// ---------------------------------------------------------------------------

/// 一个声明式页面插件（来自磁盘的 page.json）。
#[derive(Debug, Clone, PartialEq)]
pub struct PagePluginDecl {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub widgets: Vec<WidgetDecl>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WidgetDecl {
    Heading { text: String },
    Caption { text: String },
    Section { text: String },
    Text { text: String },
    Keyvalue { rows: Vec<(String, String)> },
    List { bind: String },
    Progress { bind: String },
    Spacer { #[serde(default)] h: i32 },
    Button {
        text: String,
        #[serde(default)]
        kind: String,
        action: ActionDecl,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionDecl {
    Toast { message: String },
    Nav { page: String },
    Refresh,
    Launch,
    Mcp {
        server: String,
        tool: String,
        #[serde(default)]
        args: serde_json::Value,
    },
}

/// 解析一份 page.json。失败返回可读错误（给工坊页 toast 用）。
pub fn parse(bytes: &[u8]) -> Result<PagePluginDecl, String> {
    #[derive(Deserialize)]
    struct Raw {
        id: String,
        name: String,
        #[serde(default = "default_icon")]
        icon: String,
        #[serde(default)]
        widgets: Vec<WidgetDecl>,
    }
    fn default_icon() -> String {
        "◆".to_owned()
    }
    let raw: Raw = serde_json::from_slice(bytes).map_err(|e| format!("page.json 解析失败：{e}"))?;
    let id = raw.id.trim().to_owned();
    if id.is_empty() {
        return Err("page.json 缺少 id".to_owned());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err(format!("page.json id 只允许字母/数字/./-/_：`{id}`"));
    }
    let name = raw.name.trim().to_owned();
    if name.is_empty() {
        return Err("page.json 缺少 name".to_owned());
    }
    Ok(PagePluginDecl {
        id,
        name,
        icon: raw.icon,
        widgets: raw.widgets,
    })
}

/// 目录指纹：文件数 + 最近修改秒。变了就重扫（热加载检测，每 tick 一次 read_dir，
/// 只有几个 JSON 文件时开销可忽略）。
pub fn dir_stamp(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut files = 0u64;
    let mut newest = 0u64;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            files += 1;
            if let Ok(meta) = e.metadata() {
                if let Ok(m) = meta.modified() {
                    if let Ok(secs) = m.duration_since(std::time::UNIX_EPOCH) {
                        newest = newest.max(secs.as_secs());
                    }
                }
            }
        }
    }
    files * 1_000_000_000 + newest
}

/// 扫描目录里每个子文件夹的 page.json。解析失败的目录跳过（并在日志页可见，
/// 由调用方负责记录）。
pub fn load_pages(dir: &Path) -> (Vec<PagePluginDecl>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (out, errors);
    };
    for e in entries.flatten() {
        let sub = e.path();
        if !sub.is_dir() {
            continue;
        }
        let f = sub.join("page.json");
        match std::fs::read(&f) {
            Ok(bytes) => match parse(&bytes) {
                Ok(mut decl) => {
                    // 目录名兜底：id 与目录名保持一致，删除/更新按目录定位。
                    if let Some(dirname) = sub.file_name().and_then(|s| s.to_str()) {
                        if decl.id != dirname {
                            decl.id = dirname.to_owned();
                        }
                    }
                    out.push(decl);
                }
                Err(e) => errors.push(format!("{}：{e}", sub.display())),
            },
            Err(_) => errors.push(format!("{}：缺少 page.json", sub.display())),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    (out, errors)
}

/// 首次启动铺两个示例页面插件 + mcp.json 模板。
pub fn seed_examples(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let hello = dir.join("user.hello");
    if !hello.exists() {
        let _ = std::fs::create_dir_all(&hello);
        let _ = std::fs::write(
            hello.join("page.json"),
            r#"{
  "id": "user.hello",
  "name": "示例·总览",
  "icon": "✦",
  "widgets": [
    { "type": "heading", "text": "示例页面插件" },
    { "type": "caption", "text": "这是一个由 page.json 声明的标签页：改文件即热重载，AI 可以直接生成它。" },
    { "type": "spacer", "h": 8 },
    { "type": "section", "text": "实时数据（list 绑定）" },
    { "type": "list", "bind": "instances" },
    { "type": "spacer", "h": 8 },
    { "type": "section", "text": "动作按钮" },
    { "type": "button", "text": "启动当前版本", "kind": "primary", "action": { "kind": "launch" } },
    { "type": "spacer", "h": 4 },
    { "type": "button", "text": "去下载中心", "action": { "kind": "nav", "page": "downloads" } },
    { "type": "spacer", "h": 4 },
    { "type": "button", "text": "打个招呼", "action": { "kind": "toast", "message": "你好，这是页面插件的 Toast！" } }
  ]
}
"#,
        );
    }
    let tools = dir.join("user.mcp-tools");
    if !tools.exists() {
        let _ = std::fs::create_dir_all(&tools);
        let _ = std::fs::write(
            tools.join("page.json"),
            r#"{
  "id": "user.mcp-tools",
  "name": "MCP 工具台",
  "icon": "⇄",
  "widgets": [
    { "type": "heading", "text": "外接 MCP 工具" },
    { "type": "caption", "text": "在插件目录的 mcp.json 里配置服务器后，点「刷新工具列表」；下表实时来自 MCP 服务器的 tools/list。" },
    { "type": "list", "bind": "mcp_tools" }
  ]
}
"#,
        );
    }
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

/// 渲染一个声明式页面。返回需要跳转的内置页面（nav 动作）。
pub fn render(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    decl: &PagePluginDecl,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    let mut y = r.y;
    ui.heading(Rect::new(r.x, y, 280, 32), format!("{} {}", decl.icon, decl.name));
    y += 40;

    // 先展开 widgets 算内容高度（滚动需要）。
    let content_h = layout_height(m, decl, r.w).max(r.h);
    let list_id = WidgetId::derive(WidgetId::NONE, 0, &format!("page-plugin:{}", decl.id));
    let off = ui.scroll_area(list_id, Rect::new(r.x, y, r.w, r.bottom() - y), content_h);
    ui.push_clip(Rect::new(r.x, y, r.w, r.bottom() - y));
    let mut cy = y - off;
    let mut nav: Option<Page> = None;

    for w in &decl.widgets {
        cy = render_widget(ui, m, w, r, theme, cy, &mut nav);
    }
    ui.pop_clip();
    nav
}

/// 每个控件的估算高度（与 render_widget 保持一致）。
fn widget_height(m: &LauncherModel, w: &WidgetDecl, width: i32) -> i32 {
    match w {
        WidgetDecl::Heading { .. } => 36,
        WidgetDecl::Caption { text } => {
            let lines = wrap_lines(text, width, 14);
            20 * lines + 4
        }
        WidgetDecl::Section { .. } => 34,
        WidgetDecl::Text { text } => 20 * wrap_lines(text, width - 8, 14) + 2,
        WidgetDecl::Keyvalue { rows } => rows.len() as i32 * 26 + 6,
        WidgetDecl::List { bind } => {
            let n = bind_len(m, bind).max(1) as i32;
            (n * 30).min(30 * 200)
        }
        WidgetDecl::Progress { .. } => 26,
        WidgetDecl::Spacer { h } => (*h).max(0),
        WidgetDecl::Button { .. } => 38,
    }
}

fn layout_height(m: &LauncherModel, decl: &PagePluginDecl, width: i32) -> i32 {
    decl.widgets
        .iter()
        .map(|w| widget_height(m, w, width))
        .sum()
}

fn wrap_lines(text: &str, width: i32, font: i32) -> i32 {
    let w = width.max(60) as f32;
    // 粗略估算：中文按全宽、ASCII 按半宽。
    let units: f32 = text
        .chars()
        .map(|c| if c.is_ascii() { 0.55 } else { 1.0 })
        .sum();
    ((units * font as f32) / w).ceil().max(1.0) as i32
}

fn bind_len(m: &LauncherModel, bind: &str) -> usize {
    match bind {
        "instances" => m.versions.len(),
        "accounts" => m.accounts.len(),
        "downloads" => m.downloads.len(),
        "logs" => m.logs.len().min(200),
        "plugins" => m.plugins.len(),
        "mcp_tools" => m.mcp_tools.len(),
        "catalog" => m.mc_catalog.len().min(100),
        _ => 0,
    }
}

/// 渲染单个控件，返回新的 y。
fn render_widget(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    w: &WidgetDecl,
    r: Rect,
    theme: &Theme,
    mut cy: i32,
    nav: &mut Option<Page>,
) -> i32 {
    let width = r.w;
    match w {
        WidgetDecl::Heading { text } => {
            ui.heading(Rect::new(r.x, cy, width, 32), text.clone());
            cy + 36
        }
        WidgetDecl::Caption { text } => {
            let lines = wrap_lines(text, width, 14);
            ui.caption(Rect::new(r.x, cy, width, 20 * lines), text.clone());
            cy + 20 * lines + 4
        }
        WidgetDecl::Section { text } => {
            section(ui, Rect::new(r.x, cy, width, 28), text, theme);
            cy + 34
        }
        WidgetDecl::Text { text } => {
            let lines = wrap_lines(text, width - 8, 14);
            ui.text_vcenter(
                Rect::new(r.x + 4, cy, width - 8, 20 * lines),
                text.clone(),
                theme.text_dim,
                theme.font_small,
                TextAlign::Left,
                false,
            );
            cy + 20 * lines + 2
        }
        WidgetDecl::Keyvalue { rows } => {
            ui.fill_round(Rect::new(r.x, cy, width, rows.len() as i32 * 26 + 6), theme.card, theme.radius);
            let mut ry = cy + 3;
            for (k, v) in rows {
                ui.text_vcenter(
                    Rect::new(r.x + 12, ry, 160, 26),
                    k.clone(),
                    theme.text_dim,
                    theme.font_small,
                    TextAlign::Left,
                    false,
                );
                ui.text_vcenter(
                    Rect::new(r.x + 180, ry, width - 192, 26),
                    v.clone(),
                    theme.text,
                    theme.font_small,
                    TextAlign::Left,
                    false,
                );
                ry += 26;
            }
            cy + rows.len() as i32 * 26 + 6
        }
        WidgetDecl::List { bind } => {
            let n = bind_len(m, bind).max(1) as i32;
            let h = (n * 30).min(30 * 200);
            let inner = Rect::new(r.x, cy, width - 8, h);
            let rows = bind_rows(m, bind);
            if rows.is_empty() {
                ui.empty_state(inner, "（暂无数据）");
            } else {
                for (i, row) in rows.iter().enumerate().take(200) {
                    let ry = inner.y + i as i32 * 30;
                    if ry + 28 > inner.bottom() {
                        break;
                    }
                    if i % 2 == 0 {
                        ui.fill_round(Rect::new(inner.x, ry, inner.w, 28), theme.card, theme.radius);
                    }
                    ui.text_vcenter(
                        Rect::new(inner.x + 10, ry, inner.w - 20, 28),
                        row.clone(),
                        theme.text,
                        theme.font_small,
                        TextAlign::Left,
                        false,
                    );
                }
            }
            cy + h + 4
        }
        WidgetDecl::Progress { bind } => {
            let (label, frac) = match bind.as_str() {
                "asset" => m
                    .downloads
                    .iter()
                    .find(|t| t.id == "asset-repair")
                    .map(|t| {
                        (
                            format!(
                                "{}  {}/{} MB",
                                t.name, t.done_mb, t.total_mb
                            ),
                            if t.total_mb > 0 {
                                t.done_mb as f32 / t.total_mb as f32
                            } else {
                                0.0
                            },
                        )
                    })
                    .unwrap_or(("资源补齐：空闲".to_owned(), 0.0)),
                _ => ("未知进度".to_owned(), 0.0),
            };
            ui.caption(Rect::new(r.x, cy, width, 18), label);
            ui.progress(Rect::new(r.x, cy + 18, width, 6), frac);
            cy + 26
        }
        WidgetDecl::Spacer { h } => cy + (*h).max(0),
        WidgetDecl::Button { text, kind, action } => {
            let bk = match kind.as_str() {
                "primary" => ButtonKind::Primary,
                "danger" => ButtonKind::Danger,
                _ => ButtonKind::Normal,
            };
            let bw = (ui.measure(text, theme.font_small, false) + 32).max(96);
            if ui.button(Rect::new(r.x, cy, bw, 32), text, bk, true).clicked {
                *nav = run_action(m, action);
            }
            cy + 38
        }
    }
}

use ui_core::TextAlign;

fn bind_rows(m: &LauncherModel, bind: &str) -> Vec<String> {
    match bind {
        "instances" => m
            .versions
            .iter()
            .map(|v| {
                format!(
                    "{}  —  {}（{}）",
                    v.name,
                    v.mc_version,
                    v.loader.label()
                )
            })
            .collect(),
        "accounts" => m
            .accounts
            .iter()
            .map(|a| format!("{}（{}）", a.name, a.kind.label()))
            .collect(),
        "downloads" => m
            .downloads
            .iter()
            .map(|t| {
                format!(
                    "{}  {}/{} MB  {}",
                    t.name,
                    t.done_mb,
                    t.total_mb,
                    if t.state == crate::domain::DownloadState::Downloading {
                        "下载中"
                    } else if t.error.is_some() {
                        "失败"
                    } else {
                        "已完成"
                    }
                )
            })
            .collect(),
        "logs" => m.logs.iter().rev().take(200).cloned().collect(),
        "plugins" => m
            .plugins
            .iter()
            .map(|p| format!("{}  v{}  ·  {}", p.name, p.version, p.status()))
            .collect(),
        "mcp_tools" => m
            .mcp_tools
            .iter()
            .map(|(server, tool, desc)| {
                if desc.is_empty() {
                    format!("[{server}] {tool}")
                } else {
                    format!("[{server}] {tool}  —  {desc}")
                }
            })
            .collect(),
        "catalog" => m
            .mc_catalog
            .iter()
            .take(100)
            .map(|(id, name, _)| format!("{name}  ({id})"))
            .collect(),
        _ => Vec::new(),
    }
}

/// 执行声明动作。返回需要跳转的内置页面。
pub fn run_action(m: &mut LauncherModel, action: &ActionDecl) -> Option<Page> {
    match action {
        ActionDecl::Toast { message } => {
            m.toast(message.clone(), ToastKind::Info);
            None
        }
        ActionDecl::Nav { page } => Some(match page.as_str() {
            "home" => Page::Home,
            "versions" | "instances" => Page::Versions,
            "accounts" => Page::Accounts,
            "downloads" => Page::Downloads,
            "settings" => Page::Settings,
            "logs" => Page::Logs,
            "plugins" => Page::Plugins,
            "workshop" => Page::Workshop,
            _ => Page::Home,
        }),
        ActionDecl::Refresh => {
            m.refresh();
            None
        }
        ActionDecl::Launch => {
            // 与首页启动按钮同一条路径；失败会以 Toast/日志反馈。
            let _ = m.start_launch();
            None
        }
        ActionDecl::Mcp { server, tool, args } => {
            let srv = m
                .mcp_servers
                .iter()
                .find(|s| s.name == *server)
                .cloned();
            match srv {
                None => m.toast(
                    format!("未找到 MCP 服务器「{server}」，请在 plugins/mcp.json 配置"),
                    ToastKind::Warn,
                ),
                Some(srv) => {
                    let tool = tool.clone();
                    let args = args.clone();
                    m.spawn_bg(move || {
                        Box::new(move |m: &mut LauncherModel| {
                            match crate::mcp::call_tool(&srv, &tool, args) {
                                Ok(text) => {
                                    m.log(format!("[MCP] {tool} → {text}"));
                                    m.toast(
                                        format!("{tool}：{}", crate::mcp::first_line(&text)),
                                        ToastKind::Success,
                                    );
                                }
                                Err(e) => m.toast(format!("{tool} 失败：{e}"), ToastKind::Warn),
                            }
                        })
                    });
                }
            }
            None
        }
    }
}
