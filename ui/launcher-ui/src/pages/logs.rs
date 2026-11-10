//! 运行日志：入口（Ctrl+L）、筛选、异常退出高亮、清空。

use crate::domain::{ConfirmRequest, LauncherModel, PendingAction};
use crate::shell::{empty_block, section, Page};
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Color, Rect, TextAlign, Theme, Ui, WidgetId};

const FILTERS: [&str; 4] = ["全部", "信息", "警告", "错误"];

pub fn render(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    ui.heading(Rect::new(r.x, r.y, 260, 32), "运行日志");
    ui.caption(
        Rect::new(r.x + 266, r.y, r.w - 266, 32),
        "游戏异常退出时会自动提示并定位到这里（Ctrl+L）",
    );

    let bar_y = r.y + 38;
    let mut bx = r.x;
    for (i, name) in FILTERS.iter().enumerate() {
        if ui
            .button(
                Rect::new(bx, bar_y, 88, theme.control_h_small),
                name,
                if m.log_filter == i {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Normal
                },
                true,
            )
            .clicked
        {
            m.log_filter = i;
        }
        bx += 92;
    }
    if ui
        .button(
            Rect::new(r.right() - 216, bar_y, 100, theme.control_h_small),
            "刷新",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.refresh();
    }
    if ui
        .button(
            Rect::new(r.right() - 108, bar_y, 100, theme.control_h_small),
            "清空日志",
            ButtonKind::Danger,
            true,
        )
        .clicked
    {
        m.request_confirm(ConfirmRequest {
            title: "清空运行日志".to_owned(),
            body: "所有日志记录会被永久删除，此操作不可恢复。".to_owned(),
            confirm_label: "确认清空".to_owned(),
            danger: true,
            action: PendingAction::ClearLogs,
        });
    }

    // 异常退出横幅
    let mut list_y = bar_y + theme.control_h_small + theme.gap;
    if m.error.is_some() {
        let err = m.error.clone().unwrap_or_default();
        let banner = Rect::new(r.x, list_y, r.w, 38);
        ui.fill_round(banner, theme.card, theme.radius);
        ui.stroke_round(banner, theme.danger, theme.radius, 2);
        ui.text_vcenter(
            Rect::new(
                banner.x + theme.pad,
                banner.y,
                banner.w - theme.pad * 2,
                banner.h,
            ),
            format!("{err} · 请检查 ERROR 行与 JVM 参数、Java 版本是否匹配"),
            theme.danger,
            theme.font_small,
            TextAlign::Left,
            true,
        );
        list_y += 46;
    }

    section(ui, Rect::new(r.x, list_y, r.w, 28), "日志输出", theme);
    let list = Rect::new(r.x, list_y + 32, r.w, (r.bottom() - list_y - 32).max(60));
    let lines: Vec<String> = m
        .logs
        .iter()
        .rev()
        .filter(|l| match m.log_filter {
            1 => l.contains("[INFO]"),
            2 => l.contains("[WARN]"),
            3 => l.contains("[ERROR]"),
            _ => true,
        })
        .cloned()
        .collect();
    if lines.is_empty() {
        empty_block(
            ui,
            list,
            "暂无匹配日志，切换筛选或运行一次启动流程",
            None,
            theme,
        );
        return None;
    }
    let row_h = 26;
    let id = WidgetId::derive(WidgetId::NONE, 0, "logs-list");
    let off = ui.scroll_area(id, list, lines.len() as i32 * row_h);
    ui.push_clip(list);
    for (i, line) in lines.iter().enumerate() {
        let row = Rect::new(list.x, list.y + i as i32 * row_h - off, list.w, row_h - 2);
        if i % 2 == 0 {
            ui.fill_round(row, theme.card, theme.radius.min(4));
        }
        let color = if line.contains("[ERROR]") {
            theme.danger
        } else if line.contains("[WARN]") {
            Color::rgb(0xD1, 0x96, 0x3C)
        } else if line.contains("[INFO]") {
            theme.text_dim
        } else {
            theme.text_faint
        };
        ui.text_vcenter(
            Rect::new(row.x + theme.pad, row.y, row.w - theme.pad * 2, row.h),
            line.as_str(),
            color,
            theme.font_small,
            TextAlign::Left,
            false,
        );
    }
    ui.pop_clip();
    None
}
