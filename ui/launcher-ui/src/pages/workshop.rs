//! 插件工坊：让 AI（或用户）直接"写插件"并热重载进启动器。
//!
//! # 工作流
//!
//! 1. 把下面的 page.json 格式说明发给任意 AI，让它生成一份描述；
//! 2. 复制 AI 的 JSON → 点「从剪贴板导入」→ 新标签页立即出现在侧栏；
//! 3. 不满意 → 删掉重写，热重载即时生效。
//!
//! 声明式插件只能「展示数据 + 绑定既有动作」，无法执行任意代码，
//! 因此 AI 产出的 JSON 天然安全（最坏情况是渲染失败被跳过）。
//!
//! # page.json 格式速查（可直接复制给 AI 当提示词）
//!
//! ```text
//! {"id":"user.xxx","name":"页面名","icon":"◆","widgets":[
//!   {"type":"heading","text":"标题"},
//!   {"type":"caption","text":"说明"},
//!   {"type":"section","text":"区块"},
//!   {"type":"text","text":"正文"},
//!   {"type":"keyvalue","rows":[["键","值"]]},
//!   {"type":"list","bind":"instances|accounts|downloads|logs|plugins|mcp_tools|catalog"},
//!   {"type":"progress","bind":"asset"},
//!   {"type":"spacer","h":8},
//!   {"type":"button","text":"按钮","kind":"primary|normal|danger",
//!    "action":{"kind":"toast","message":"..."} |
//!             {"kind":"nav","page":"home|versions|accounts|downloads|settings|logs|plugins|workshop"} |
//!             {"kind":"refresh"} | {"kind":"launch"} |
//!             {"kind":"mcp","server":"服务器名","tool":"工具名","args":{}} ] }
//! ```

use crate::domain::ToastKind;
use crate::shell::{section, Page};
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Rect, TextAlign, Theme, Ui, WidgetId};

/// 示例模板（「复制模板到剪贴板」用）。
pub const TEMPLATE: &str = r#"{
  "id": "user.my-page",
  "name": "我的页面",
  "icon": "★",
  "widgets": [
    { "type": "heading", "text": "我的页面" },
    { "type": "caption", "text": "由 AI 生成、热加载进启动器的页面插件。" },
    { "type": "section", "text": "游戏实例" },
    { "type": "list", "bind": "instances" },
    { "type": "progress", "bind": "asset" },
    { "type": "button", "text": "启动游戏", "kind": "primary", "action": { "kind": "launch" } },
    { "type": "button", "text": "问 MCP 工具", "action": { "kind": "mcp", "server": "示例·本机 MCP", "tool": "query", "args": {} } }
  ]
}"#;

/// 完整的 AI 提示词（「复制 AI 提示词」按钮用）。
/// 用户复制后发给任意 AI，AI 的回复（只要含一份 page.json）即可直接导入。
pub const AI_PROMPT: &str = r#"你是 Aether Launcher（Minecraft 启动器）的页面插件生成器。
请严格按下面的 JSON Schema 生成一份"页面插件"描述（page.json），只输出 JSON，不要解释。

字段说明：
- id：以 "user." 开头的唯一标识，只能用字母/数字/./-/_，例如 "user.server-panel"
- name：侧栏标签页名称（2-6 个字最佳）
- icon：单个字符的图标（如 ◆ ★ ⇄ ✦）
- widgets：控件数组，type 取值：
  heading（大标题）/ caption（说明文字）/ section（区块标题）/ text（正文）
  keyvalue：{"type":"keyvalue","rows":[["键","值"],...]}
  list：{"type":"list","bind":"instances|accounts|downloads|logs|plugins|mcp_tools|catalog"}
        （实时绑定：游戏实例/账户/下载任务/运行日志/已装插件/MCP 工具/可安装版本）
  progress：{"type":"progress","bind":"asset"}（资源补齐进度条）
  spacer：{"type":"spacer","h":12}
  button：{"type":"button","text":"按钮文字","kind":"primary|normal|danger",
           "action": 动作}
- action 动作 kind 取值：
  {"kind":"toast","message":"提示文字"}
  {"kind":"nav","page":"home|versions|accounts|downloads|settings|logs|plugins|workshop"}
  {"kind":"refresh"}
  {"kind":"launch"}   （启动当前选中的版本）
  {"kind":"mcp","server":"MCP 服务器名","tool":"工具名","args":{...}}

模板示例：
{"id":"user.my-page","name":"我的页面","icon":"★","widgets":[
  {"type":"heading","text":"我的页面"},
  {"type":"caption","text":"说明文字"},
  {"type":"section","text":"游戏实例"},
  {"type":"list","bind":"instances"},
  {"type":"progress","bind":"asset"},
  {"type":"button","text":"启动游戏","kind":"primary","action":{"kind":"launch"}}
]}

现在请根据我的需求生成：{在这里描述你想要的页面}"#;

pub fn render(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut crate::domain::LauncherModel,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    let mut y = r.y;
    ui.heading(Rect::new(r.x, y, 300, 32), "插件工坊");
    ui.caption(
        Rect::new(r.x + 306, y, r.w - 306, 32),
        "把格式说明发给 AI → 复制它生成的 page.json → 一键导入，新标签页立即热重载进侧栏",
    );
    y += 42;

    // ---- 创建页面插件 ----
    section(ui, Rect::new(r.x, y, r.w, 28), "创建页面插件", theme);
    y += 36;
    ui.fill_round(Rect::new(r.x, y, r.w, 106), theme.card, theme.radius);
    ui.text_vcenter(
        Rect::new(r.x + 12, y + 8, 110, 28),
        "插件名称",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    let before = m.form_ws_name.clone();
    ui.text_input(
        Rect::new(r.x + 130, y + 6, 260, 30),
        "ws-name",
        &mut m.form_ws_name,
        "如 my-server-panel",
    );
    if m.form_ws_name != before {
        m.touch();
    }

    if ui
        .button(
            Rect::new(r.x + 12, y + 44, 190, 30),
            "从剪贴板导入 (Ctrl+C 后点我)",
            ButtonKind::Primary,
            true,
        )
        .clicked
    {
        match ui.clipboard_text() {
            Some(text) if !text.trim().is_empty() => {
                let name = m.form_ws_name.clone();
                match m.create_page_plugin_from_text(&name, &text) {
                    Ok(n) => m.toast(format!("已创建页面插件「{n}」，已热重载进侧栏"), ToastKind::Success),
                    Err(e) => m.toast(e, ToastKind::Warn),
                }
            }
            _ => m.toast("剪贴板是空的：先复制 page.json 内容", ToastKind::Warn),
        }
        m.touch();
    }
    if ui
        .button(
            Rect::new(r.x + 212, y + 44, 178, 30),
            "复制 AI 提示词",
            ButtonKind::Primary,
            true,
        )
        .clicked
    {
        ui.set_clipboard_text(AI_PROMPT);
        m.toast("AI 提示词已复制：发给任意 AI，把它生成的 JSON 复制回来点「从剪贴板导入」", ToastKind::Info);
    }
    if ui
        .button(
            Rect::new(r.x + 400, y + 44, 150, 30),
            "复制模板 JSON",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        ui.set_clipboard_text(TEMPLATE);
        m.toast("模板已复制到剪贴板，可直接粘贴修改或发给 AI", ToastKind::Info);
    }
    if ui
        .button(
            Rect::new(r.x + 560, y + 44, 150, 30),
            "打开插件目录",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        if let Some(dir) = m.page_plugins_dir() {
            let _ = std::process::Command::new("explorer").arg(dir).spawn();
        }
    }
    ui.caption(
        Rect::new(r.x + 12, y + 78, r.w - 24, 22),
        "也可以手动把 <目录名>/page.json 放进 plugins/pages/，约半秒内自动热重载",
    );
    y += 116;

    // ---- 外接 MCP ----
    section(ui, Rect::new(r.x, y, r.w, 28), "外接 MCP 服务器", theme);
    y += 36;
    ui.fill_round(Rect::new(r.x, y, r.w, 66), theme.card, theme.radius);
    ui.text_vcenter(
        Rect::new(r.x + 12, y + 6, r.w - 200, 26),
        format!(
            "已配置 {} 个服务器（plugins/mcp.json）",
            m.mcp_servers.len()
        ),
        theme.text,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    ui.caption(
        Rect::new(r.x + 12, y + 30, r.w - 200, 26),
        m.mcp_status
            .clone()
            .unwrap_or_else(|| "配置格式：{\"servers\":[{\"name\":\"名\",\"url\":\"http(s)://…\"}]}；http 仅允许本机回环".to_owned()),
    );
    let busy = m.mcp_busy;
    if ui
        .button(
            Rect::new(r.x + r.w - 190, y + 18, 178, 30),
            if busy { "刷新中…" } else { "刷新工具列表" },
            ButtonKind::Primary,
            !busy,
        )
        .clicked
    {
        m.refresh_mcp_tools();
    }
    y += 76;

    // MCP 工具列表
    let tools_rect = Rect::new(r.x, y, r.w, 120.min(r.bottom() - y - 8).max(40));
    let rows: Vec<String> = m
        .mcp_tools
        .iter()
        .map(|(s, t, d)| {
            if d.is_empty() {
                format!("[{s}] {t}")
            } else {
                format!("[{s}] {t} — {d}")
            }
        })
        .collect();
    let content_h = (rows.len() as i32 * 26).max(26);
    let id = WidgetId::derive(WidgetId::NONE, 0, "mcp-tools-list");
    let off = ui.scroll_area(id, tools_rect, content_h);
    ui.push_clip(tools_rect);
    if rows.is_empty() {
        ui.empty_state(tools_rect, "尚未拉取到 MCP 工具（配置服务器后点「刷新工具列表」）");
    } else {
        for (i, row) in rows.iter().enumerate() {
            let ry = tools_rect.y - off + i as i32 * 26;
            if ry + 24 > tools_rect.bottom() || ry < tools_rect.y - 24 {
                continue;
            }
            ui.text_vcenter(
                Rect::new(tools_rect.x + 8, ry, tools_rect.w - 16, 24),
                row.clone(),
                theme.text_dim,
                theme.font_small,
                TextAlign::Left,
                false,
            );
        }
    }
    ui.pop_clip();
    y += tools_rect.h + 10;

    // ---- 已装页面插件 ----
    section(
        ui,
        Rect::new(r.x, y, r.w, 28),
        &format!("已装页面插件（{}）", m.page_plugins.len()),
        theme,
    );
    y += 36;
    let list_rect = Rect::new(r.x, y, r.w, r.bottom() - y - 8);
    let row_h = 44;
    let content_h = (m.page_plugins.len() as i32 * row_h).max(row_h);
    let id = WidgetId::derive(WidgetId::NONE, 0, "page-plugins-list");
    let off = ui.scroll_area(id, list_rect, content_h);
    ui.push_clip(list_rect);
    let items: Vec<(String, String, usize)> = m
        .page_plugins
        .iter()
        .map(|p| (p.id.clone(), p.name.clone(), p.widgets.len()))
        .collect();
    for (i, (pid, name, nwidgets)) in items.iter().enumerate() {
        let cy = list_rect.y - off + i as i32 * row_h;
        if cy + row_h < list_rect.y || cy > list_rect.bottom() {
            continue;
        }
        let card = Rect::new(list_rect.x, cy, list_rect.w, row_h - 6);
        ui.fill_round(card, theme.card, theme.radius);
        ui.text_vcenter(
            Rect::new(card.x + 12, cy + 4, card.w - 120, 32),
            format!("{name}   ({pid}) · {nwidgets} 个控件"),
            theme.text,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        if ui
            .button(
                Rect::new(card.x + card.w - 90, cy + 7, 78, 26),
                "删除",
                ButtonKind::Danger,
                true,
            )
            .clicked
        {
            match m.remove_page_plugin(pid) {
                Ok(()) => m.toast(format!("已删除 {pid}，正在热重载"), ToastKind::Info),
                Err(e) => m.toast(e, ToastKind::Warn),
            }
            m.touch();
        }
    }
    ui.pop_clip();
    None
}
