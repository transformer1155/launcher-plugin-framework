//! 全局外壳：顶栏、左侧导航、Toast、危险操作确认框、更新提示与公共展示组件。
//!
//! 这一层不持有业务状态，所有数据都从 [`LauncherModel`] 读，所有动作都写回它。

use crate::domain::{ConfirmRequest, LauncherModel, ToastKind};
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Color, Rect, ShortcutKey, TextAlign, Theme, Ui, WindowAction};

/// 页面路由。
///
/// `Instances` / `Mods` / `Launch` 是旧版三页的兼容入口，统一 `route()` 到新页面，
/// 这样既保留既有测试与命令行参数，又不产生两套界面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    Versions,
    Accounts,
    Downloads,
    Settings,
    Logs,
    Plugins,
    Instances,
    Mods,
    Launch,
    /// 插件工坊：AI / 用户粘贴 JSON 创建页面插件。
    Workshop,
    /// 声明式页面插件（热加载自 plugins/pages/），下标对应 model.page_plugins。
    Custom(usize),
}

impl Page {
    pub const NAV: [Page; 7] = [
        Page::Home,
        Page::Versions,
        Page::Accounts,
        Page::Downloads,
        Page::Settings,
        Page::Logs,
        Page::Plugins,
    ];

    pub fn route(self) -> Page {
        match self {
            Page::Instances => Page::Versions,
            Page::Mods => Page::Downloads,
            Page::Launch => Page::Home,
            other => other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Page::Home | Page::Launch => "首页",
            Page::Versions | Page::Instances => "版本管理",
            Page::Accounts => "账户管理",
            Page::Downloads | Page::Mods => "下载中心",
            Page::Settings => "设置",
            Page::Logs => "运行日志",
            Page::Plugins => "插件",
            Page::Workshop => "插件工坊",
            Page::Custom(_) => "页面插件",
        }
    }

    pub fn arg_name(self) -> &'static str {
        match self {
            Page::Home | Page::Launch => "home",
            Page::Versions | Page::Instances => "versions",
            Page::Accounts => "accounts",
            Page::Downloads | Page::Mods => "downloads",
            Page::Settings => "settings",
            Page::Logs => "logs",
            Page::Plugins => "plugins",
            Page::Workshop => "workshop",
            Page::Custom(_) => "custom",
        }
    }

    pub fn from_arg(s: &str) -> Page {
        match s {
            "versions" | "instances" => Page::Versions,
            "accounts" => Page::Accounts,
            "downloads" | "mods" => Page::Downloads,
            "settings" => Page::Settings,
            "logs" => Page::Logs,
            "plugins" => Page::Plugins,
            "workshop" => Page::Workshop,
            "launch" => Page::Home,
            _ => Page::Home,
        }
    }
}

/// Ctrl+1..6 切页，Ctrl+L 日志，F5 刷新，Esc 关闭浮层。
pub fn shortcut_page(keys: &[ShortcutKey]) -> Option<Page> {
    for k in keys {
        if let ShortcutKey::Ctrl(c) = k {
            if ('1'..='7').contains(c) {
                return Some(Page::NAV[(*c as u8 - b'1') as usize]);
            }
            if *c == 'L' || *c == 'l' {
                return Some(Page::Logs);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 公共组件
// ---------------------------------------------------------------------------

/// 区块标题 + 细分割线。
pub fn section(ui: &mut Ui<'_, impl Backend + ?Sized>, rect: Rect, title: &str, theme: &Theme) {
    ui.text_vcenter(
        Rect::new(rect.x, rect.y, rect.w, 26),
        title,
        theme.text,
        theme.font_heading,
        TextAlign::Left,
        true,
    );
    ui.line(
        Rect::new(rect.x, rect.y + 27, rect.w, 1),
        theme.divider,
        true,
    );
}

/// 彩色徽标（状态标签）。
pub fn pill(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    x: i32,
    y: i32,
    text: &str,
    color: Color,
    theme: &Theme,
) -> i32 {
    let w = ui.measure(text, theme.font_small, false) + theme.pad_small * 2 + 8;
    let h = theme.control_h_small - 4;
    let r = Rect::new(x, y, w, h);
    ui.fill_round(r, color, h / 2);
    ui.text_vcenter(
        r,
        text,
        Color::rgb(0xFF, 0xFF, 0xFF),
        theme.font_small,
        TextAlign::Center,
        false,
    );
    w
}

/// 键值行（卡片里的信息条目）。
pub fn kv(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    rect: Rect,
    key: &str,
    value: &str,
    theme: &Theme,
) {
    ui.text_vcenter(
        Rect::new(rect.x, rect.y, 108, rect.h),
        key,
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    ui.text_vcenter(
        Rect::new(rect.x + 112, rect.y, rect.w - 112, rect.h),
        value,
        theme.text,
        theme.font_small,
        TextAlign::Left,
        true,
    );
}

/// 加载态。
pub fn loading_block(ui: &mut Ui<'_, impl Backend + ?Sized>, rect: Rect, msg: &str, theme: &Theme) {
    ui.fill_round(rect, theme.card, theme.radius);
    ui.text_vcenter(
        rect,
        format!("正在加载… {msg}"),
        theme.text_dim,
        theme.font_body,
        TextAlign::Center,
        false,
    );
}

/// 空态 + 可选的主操作按钮，返回是否点击。
pub fn empty_block(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    rect: Rect,
    msg: &str,
    action: Option<&str>,
    theme: &Theme,
) -> bool {
    ui.fill_round(rect, theme.card, theme.radius);
    ui.text_vcenter(
        Rect::new(rect.x, rect.y + 10, rect.w, 32),
        msg,
        theme.text_dim,
        theme.font_body,
        TextAlign::Center,
        false,
    );
    let Some(label) = action else {
        return false;
    };
    let bw = 132;
    let br = Rect::new(
        rect.x + (rect.w - bw) / 2,
        rect.y + 52,
        bw,
        theme.control_h_small,
    );
    ui.button(br, label, ButtonKind::Primary, true).clicked
}

/// 错误态 + 重试按钮。
pub fn error_block(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    rect: Rect,
    msg: &str,
    theme: &Theme,
) -> bool {
    ui.fill_round(rect, theme.card, theme.radius);
    ui.stroke_round(rect, theme.danger, theme.radius, 1);
    ui.text_vcenter(
        Rect::new(rect.x, rect.y + 8, rect.w, 30),
        msg,
        theme.danger,
        theme.font_body,
        TextAlign::Center,
        true,
    );
    let bw = 120;
    ui.button(
        Rect::new(
            rect.x + (rect.w - bw) / 2,
            rect.y + 46,
            bw,
            theme.control_h_small,
        ),
        "重试",
        ButtonKind::Normal,
        true,
    )
    .clicked
}

/// 启动流程四阶段进度条。
pub fn launch_stages(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    rect: Rect,
    m: &LauncherModel,
    theme: &Theme,
) {
    use crate::domain::LaunchStage;
    let n = LaunchStage::ORDER.len();
    let gap = theme.gap;
    let each = (rect.w - gap * (n - 1) as i32) / n as i32;
    let current = LaunchStage::ORDER
        .iter()
        .position(|s| *s == m.launch.stage)
        .unwrap_or(0);
    for (i, stage) in LaunchStage::ORDER.iter().enumerate() {
        let r = Rect::new(rect.x + i as i32 * (each + gap), rect.y, each, 46);
        let active = m.launch.pct > 0.0 && i <= current;
        let color = if active { theme.accent } else { theme.card };
        ui.fill_round(r, color, theme.radius);
        ui.text_vcenter(
            Rect::new(r.x, r.y + 4, r.w, 20),
            format!("{}. {}", i + 1, stage.label()),
            if active {
                Color::rgb(0xFF, 0xFF, 0xFF)
            } else {
                theme.text_dim
            },
            theme.font_small,
            TextAlign::Center,
            active,
        );
        let label: String = if i < current {
            "已完成".to_owned()
        } else if i == current {
            format!("{}%", (m.launch.pct * 100.0) as i32)
        } else {
            "等待中".to_owned()
        };
        ui.text_vcenter(
            Rect::new(r.x, r.y + 24, r.w, 18),
            label,
            if active {
                Color::rgb(0xFF, 0xFF, 0xFF)
            } else {
                theme.text_faint
            },
            theme.font_small,
            TextAlign::Center,
            false,
        );
    }
}

// ---------------------------------------------------------------------------
// 顶栏（自绘标题栏）
// ---------------------------------------------------------------------------

/// 返回需要跳转的页面（日志 / 设置 / 更新）。
pub fn render_topbar(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    theme: &Theme,
) -> Option<Page> {
    let w = ui.size().w;
    let bar = Rect::new(0, 0, w, theme.topbar_h);
    ui.fill(bar, theme.panel);
    ui.fill(Rect::new(0, bar.bottom() - 1, w, 1), theme.divider);

    let mut goto = None;
    let mut x = theme.pad;

    ui.text_vcenter(
        Rect::new(x, 0, 190, bar.h),
        "Aether Launcher",
        theme.text,
        theme.font_heading,
        TextAlign::Left,
        true,
    );
    x += 196;

    // 当前阶段徽章
    x += pill(
        ui,
        x,
        (bar.h - 22) / 2,
        m.phase.label(),
        theme.accent,
        theme,
    ) + theme.gap;

    // 更新提示
    if let Some(ver) = m.update_available.clone() {
        let label = format!("发现新版本 {ver}");
        let bw = ui.measure(&label, theme.font_small, false) + theme.pad_small * 2;
        if ui
            .button(
                Rect::new(
                    x,
                    (bar.h - theme.control_h_small) / 2,
                    bw,
                    theme.control_h_small,
                ),
                &label,
                ButtonKind::Ghost,
                true,
            )
            .clicked
        {
            m.log("[INFO] 用户查看更新提示");
            m.toast(
                format!("新版本 {ver} 已发布，可在设置中检查更新"),
                ToastKind::Info,
            );
            goto = Some(Page::Settings);
        }
        x += bw + theme.gap;
    }

    // 日志入口（Ctrl+L）
    {
        let bw = 84;
        if ui
            .button(
                Rect::new(
                    x,
                    (bar.h - theme.control_h_small) / 2,
                    bw,
                    theme.control_h_small,
                ),
                "运行日志",
                ButtonKind::Ghost,
                true,
            )
            .clicked
        {
            goto = Some(Page::Logs);
        }
        x += bw + theme.gap;
    }

    // 外观/设置入口
    {
        let bw = 92;
        if ui
            .button(
                Rect::new(
                    x,
                    (bar.h - theme.control_h_small) / 2,
                    bw,
                    theme.control_h_small,
                ),
                "外观设置",
                ButtonKind::Ghost,
                true,
            )
            .clicked
        {
            m.settings_tab = 2;
            goto = Some(Page::Settings);
        }
    }

    // 数据源徽标（保留旧版语义）
    let label = "本地数据";
    let bw = ui.measure(label, theme.font_small, false) + theme.pad_small * 2;
    pill(
        ui,
        w - 3 * 46 - bw - theme.pad,
        (bar.h - 22) / 2,
        label,
        theme.accent,
        theme,
    );

    // 三个 46px 窗口按钮：必须放在最后，且关闭按钮占 [w-46, w)。
    let btn_y = (bar.h - theme.control_h_small) / 2;
    let btn_h = theme.control_h_small;
    let base = w - 3 * 46;
    let minimize = Rect::new(base, btn_y, 46, btn_h);
    let maximize = Rect::new(base + 46, btn_y, 46, btn_h);
    let close = Rect::new(base + 92, btn_y, 46, btn_h);
    if ui.button(minimize, "—", ButtonKind::Ghost, true).clicked {
        // 最小化一律隐藏到系统托盘。
        m.tray_hidden = true;
        m.log("[INFO] 已最小化到系统托盘");
        m.touch();
        ui.request_window_action(WindowAction::MinimizeToTray);
    }
    if ui.button(maximize, "□", ButtonKind::Ghost, true).clicked {
        ui.request_window_action(WindowAction::ToggleMaximize);
    }
    if ui.button(close, "✕", ButtonKind::Ghost, true).clicked {
        if m.settings.close_to_tray {
            m.tray_hidden = true;
            m.log("[INFO] 关闭按钮：隐藏到系统托盘");
            m.touch();
            ui.request_window_action(WindowAction::MinimizeToTray);
        } else {
            ui.request_window_action(WindowAction::Close);
        }
    }

    // 顶栏空白区域按住可拖动窗口。
    ui.title_bar(bar);
    goto
}

// ---------------------------------------------------------------------------
// 左侧导航
// ---------------------------------------------------------------------------

pub fn render_sidebar(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &LauncherModel,
    page: Page,
    theme: &Theme,
) -> Option<Page> {
    let h = ui.size().h;
    let r = Rect::new(0, theme.topbar_h, theme.sidebar_w, h - theme.topbar_h);
    ui.fill(r, theme.panel);
    ui.fill(Rect::new(r.right() - 1, r.y, 1, r.h), theme.divider);

    let mut goto = None;
    let mut y = r.y + theme.pad;
    ui.caption(
        Rect::new(r.x + theme.pad, y, r.w - theme.pad * 2, 22),
        "导航",
    );
    y += 26;

    for (i, p) in Page::NAV.iter().enumerate() {
        let item = Rect::new(
            r.x + theme.pad_small,
            y,
            r.w - theme.pad_small * 2,
            theme.control_h,
        );
        if ui
            .nav_item(
                item,
                &format!("{}  {}", i + 1, p.label()),
                p.route() == page.route(),
            )
            .clicked
        {
            goto = Some(*p);
        }
        y += theme.control_h + 4;
    }

    // ---- 页面插件区（热加载 + 工坊）----
    y += theme.gap;
    ui.line(
        Rect::new(r.x + theme.pad, y, r.w - theme.pad * 2, 1),
        theme.divider,
        true,
    );
    y += theme.gap;
    ui.caption(
        Rect::new(r.x + theme.pad, y, r.w - theme.pad * 2, 22),
        "页面插件（热加载）",
    );
    y += 24;

    let ws_item = Rect::new(
        r.x + theme.pad_small,
        y,
        r.w - theme.pad_small * 2,
        theme.control_h,
    );
    if ui
        .nav_item(ws_item, "⚒  插件工坊", page == Page::Workshop)
        .clicked
    {
        goto = Some(Page::Workshop);
    }
    y += theme.control_h + 4;

    for (i, decl) in m.page_plugins.iter().enumerate() {
        let item = Rect::new(
            r.x + theme.pad_small,
            y,
            r.w - theme.pad_small * 2,
            theme.control_h,
        );
        if ui
            .nav_item(item, &format!("{}  {}", decl.icon, decl.name), page == Page::Custom(i))
            .clicked
        {
            goto = Some(Page::Custom(i));
        }
        y += theme.control_h + 4;
    }

    y += theme.gap;
    ui.line(
        Rect::new(r.x + theme.pad, y, r.w - theme.pad * 2, 1),
        theme.divider,
        true,
    );
    y += theme.gap;
    ui.caption(
        Rect::new(r.x + theme.pad, y, r.w - theme.pad * 2, 22),
        "当前选择",
    );
    y += 24;

    let version = m
        .current_version()
        .map(|v| v.name.clone())
        .unwrap_or_else(|| "未选择版本".to_owned());
    let account = m
        .current_account()
        .map(|a| a.name.clone())
        .unwrap_or_else(|| "未登录账户".to_owned());
    for (k, v) in [("版本", version), ("账户", account)] {
        ui.text_vcenter(
            Rect::new(r.x + theme.pad, y, 44, 20),
            k,
            theme.text_dim,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        ui.text_vcenter(
            Rect::new(r.x + theme.pad + 46, y, r.w - theme.pad * 2 - 46, 20),
            v,
            theme.text,
            theme.font_small,
            TextAlign::Left,
            true,
        );
        y += 22;
    }

    y += theme.gap;
    ui.line(
        Rect::new(r.x + theme.pad, y, r.w - theme.pad * 2, 1),
        theme.divider,
        true,
    );
    y += theme.gap;
    ui.caption(
        Rect::new(r.x + theme.pad, y, r.w - theme.pad * 2, 20),
        format!(
            "Java · {}",
            m.selected_java()
                .map(|j| j.version.as_str())
                .unwrap_or("未配置")
        ),
    );
    y += 22;
    ui.caption(
        Rect::new(r.x + theme.pad, y, r.w - theme.pad * 2, 20),
        format!(
            "磨玻璃 {} · 圆角 {}px",
            if m.settings.glass { "开" } else { "关" },
            m.settings.radius
        ),
    );
    y += 22;
    ui.caption(
        Rect::new(r.x + theme.pad, y, r.w - theme.pad * 2, 20),
        format!("字体 {}", m.settings.font_family),
    );

    // 底部：状态机提示
    ui.caption(
        Rect::new(r.x + theme.pad, r.bottom() - 30, r.w - theme.pad * 2, 22),
        "Ctrl+1..6 切页 · F5 刷新 · Esc 关闭",
    );
    goto
}

// ---------------------------------------------------------------------------
// 全局反馈
// ---------------------------------------------------------------------------

/// Toast 队列。危险操作用警示色，淡入淡出由 `Toast::alpha()` 驱动。
pub fn render_toasts(ui: &mut Ui<'_, impl Backend + ?Sized>, m: &LauncherModel, theme: &Theme) {
    let w = ui.size().w;
    let mut y = theme.topbar_h + theme.pad;
    for t in m.toasts.iter().rev().take(4) {
        let alpha = t.alpha();
        if alpha <= 0.01 {
            continue;
        }
        let color = match t.kind {
            ToastKind::Info => theme.accent,
            ToastKind::Success => Color::rgb(0x3E, 0xA5, 0x6A),
            ToastKind::Warn => Color::rgb(0xD1, 0x96, 0x3C),
            ToastKind::Danger => theme.danger,
        };
        let bg = theme.card.lerp(color, 0.22 * alpha);
        let tw = 330;
        let r = Rect::new(w - tw - theme.pad, y, tw, 46);
        ui.fill_round(r, bg, theme.radius);
        ui.stroke_round(r, color, theme.radius, 1);
        ui.fill_round(Rect::new(r.x, r.y + 8, 4, r.h - 16), color, 2);
        ui.text_vcenter(
            Rect::new(r.x + 14, r.y, r.w - 24, r.h),
            t.text.as_str(),
            theme.text,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        y += 54;
    }
}

/// 危险操作确认框。删除版本 / 删除账户必须走这里。
pub fn render_confirm(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    theme: &Theme,
) {
    let Some(req) = m.confirm.clone() else {
        return;
    };
    let size = ui.size();
    // 半透明遮罩（用底色混合模拟，GDI 后端不支持真 alpha）
    let mask = Rect::new(0, 0, size.w, size.h);
    ui.fill(mask, theme.bg.lerp(Color::rgb(0x00, 0x00, 0x00), 0.55));

    let bw = 460;
    let bh = 226;
    let box_r = Rect::new((size.w - bw) / 2, (size.h - bh) / 2, bw, bh);
    ui.fill_round(box_r, theme.panel, theme.radius.max(10));
    ui.stroke_round(
        box_r,
        if req.danger {
            theme.danger
        } else {
            theme.divider
        },
        theme.radius.max(10),
        2,
    );

    ui.text_vcenter(
        Rect::new(
            box_r.x + theme.pad,
            box_r.y + 16,
            box_r.w - theme.pad * 2,
            32,
        ),
        req.title.as_str(),
        if req.danger { theme.danger } else { theme.text },
        theme.font_heading,
        TextAlign::Left,
        true,
    );
    if req.danger {
        pill(
            ui,
            box_r.x + theme.pad,
            box_r.y + 54,
            "不可恢复",
            theme.danger,
            theme,
        );
    }
    // 手动折行（内核没有自动换行）：按显示宽度切成两行。
    let body = &req.body;
    let cut = body.chars().count().min(34);
    let (l1, l2) = body.split_at(
        body.char_indices()
            .nth(cut)
            .map(|(i, _)| i)
            .unwrap_or(body.len()),
    );
    ui.text_vcenter(
        Rect::new(
            box_r.x + theme.pad,
            box_r.y + 82,
            box_r.w - theme.pad * 2,
            26,
        ),
        l1,
        theme.text_dim,
        theme.font_body,
        TextAlign::Left,
        false,
    );
    if !l2.is_empty() {
        ui.text_vcenter(
            Rect::new(
                box_r.x + theme.pad,
                box_r.y + 108,
                box_r.w - theme.pad * 2,
                26,
            ),
            l2,
            theme.text_dim,
            theme.font_body,
            TextAlign::Left,
            false,
        );
    }

    let by = box_r.bottom() - theme.control_h - theme.pad;
    let cancel = Rect::new(box_r.right() - 240, by, 108, theme.control_h);
    let ok = Rect::new(box_r.right() - 120, by, 104, theme.control_h);
    if ui.button(cancel, "取消", ButtonKind::Normal, true).clicked {
        m.cancel_confirm();
    }
    if ui
        .button(
            ok,
            &req.confirm_label,
            if req.danger {
                ButtonKind::Danger
            } else {
                ButtonKind::Primary
            },
            true,
        )
        .clicked
    {
        m.confirm_action();
    }
}

/// 顶部更新提示条。返回是否点击了「稍后」。
pub fn render_update_banner(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    content: Rect,
    theme: &Theme,
) {
    let Some(ver) = m.update_available.clone() else {
        return;
    };
    if m.update_dismissed {
        return;
    }
    let r = Rect::new(content.x, content.y, content.w, 34);
    ui.fill_round(r, theme.accent.lerp(theme.panel, 0.6), theme.radius);
    ui.text_vcenter(
        Rect::new(r.x + theme.pad, r.y, r.w - 220, r.h),
        format!("启动器有新版本 {ver} 可用，建议更新以获得更好的 Java 兼容性"),
        theme.text,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    if ui
        .button(
            Rect::new(r.right() - 168, r.y + 4, 78, 26),
            "查看更新",
            ButtonKind::Primary,
            true,
        )
        .clicked
    {
        m.toast("已打开更新说明（离线演示）", ToastKind::Info);
    }
    if ui
        .button(
            Rect::new(r.right() - 84, r.y + 4, 72, 26),
            "稍后",
            ButtonKind::Ghost,
            true,
        )
        .clicked
    {
        m.update_dismissed = true;
        m.touch();
    }
}

/// 构造一个危险操作确认请求（统一文案口径）。
pub fn danger_confirm(
    title: &str,
    body: &str,
    action: crate::domain::PendingAction,
) -> ConfirmRequest {
    ConfirmRequest {
        title: title.to_owned(),
        body: body.to_owned(),
        confirm_label: "确认删除".to_owned(),
        danger: true,
        action,
    }
}

/// 内容区（去掉顶栏、侧栏、更新条）。
pub fn content_rect(ui: &Ui<'_, impl Backend + ?Sized>, theme: &Theme, banner: bool) -> Rect {
    let size = ui.size();
    let top = theme.topbar_h + if banner { 34 + theme.gap } else { 0 };
    Rect::new(
        theme.sidebar_w + theme.pad,
        top + theme.pad,
        size.w - theme.sidebar_w - theme.pad * 2,
        size.h - top - theme.pad * 2,
    )
}
