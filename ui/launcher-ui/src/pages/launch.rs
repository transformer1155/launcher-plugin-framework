//! 启动设置页：可编辑表单 + 可点击内存条 + 真实日志。

use ui_core::backend::Backend;
use ui_core::layout::{EdgeInsets, Rect};
use ui_core::widget::ButtonKind;
use ui_core::{Ui, WidgetId};

use crate::app::App;
use crate::data::Snapshot;

fn log_scroll_id() -> WidgetId {
    WidgetId::derive(WidgetId::NONE, 7103, "launch-log")
}

pub fn render(app: &mut App, ui: &mut Ui<'_, impl Backend + ?Sized>, area: Rect, snap: &Snapshot) {
    let theme = app.theme().clone();
    let gap = theme.gap;
    let radius = theme.style.radius();

    // ---- 标题 ----
    let header_h = theme.font_heading + 8;
    ui.heading(area.take_top(header_h), "启动设置");
    ui.fill(
        Rect::new(area.x, area.y + header_h, area.w, 1),
        theme.divider,
    );

    let inst_name = app
        .selected_view()
        .map(|i| i.name)
        .unwrap_or_else(|| "（未选择实例）".to_owned());
    let y = area.y + header_h + gap;
    ui.caption(
        Rect::new(area.x, y, area.w, 20),
        format!("当前实例：{inst_name}"),
    );

    let mut form_y = y + 26;
    let label_w = 120;
    let field_x = area.x + label_w;
    let field_w = (area.w - label_w - 280).max(140);

    // ---- 内存（可编辑 + 可点击比例条）----
    let row = Rect::new(area.x, form_y, area.w, theme.control_h);
    ui.label(
        Rect::new(row.x, row.y, label_w, row.h),
        "内存上限(MB)",
        false,
    );
    ui.text_input(
        Rect::new(field_x, row.y, 160, row.h),
        "launch_memory",
        &mut app.form_memory,
        "512..32768",
    );
    let bar = Rect::new(
        field_x + 172,
        row.y + row.h / 2 - 5,
        (field_w - 180).max(80),
        10,
    );
    let memory = app.form_memory.trim().parse::<u32>().unwrap_or(4096);
    let frac = memory_fraction(memory);
    // 先命中测试，再画进度条（否则 hover 底纹会盖住进度条）。
    let hit = ui.nav_item(bar, "", false);
    if hit.clicked {
        if let Some((mx, _)) = ui.mouse_pos() {
            let f = ((mx - bar.x) as f32 / bar.w.max(1) as f32).clamp(0.0, 1.0);
            let mb = ((f * 32768.0) as u32).clamp(512, 32768);
            app.form_memory = mb.to_string();
        }
    }
    ui.progress(bar, frac);
    form_y += theme.control_h + gap;

    // ---- JVM 参数（可编辑）----
    let row = Rect::new(area.x, form_y, area.w, theme.control_h);
    ui.label(Rect::new(row.x, row.y, label_w, row.h), "JVM 参数", false);
    ui.text_input(
        Rect::new(field_x, row.y, (row.w - label_w).max(120), row.h),
        "launch_jvm",
        &mut app.form_jvm,
        "例如 -XX:+UseG1GC -Xmx6G",
    );
    form_y += theme.control_h + gap;

    // ---- 游戏参数（可编辑）----
    let row = Rect::new(area.x, form_y, area.w, theme.control_h);
    ui.label(Rect::new(row.x, row.y, label_w, row.h), "游戏参数", false);
    ui.text_input(
        Rect::new(field_x, row.y, (row.w - label_w).max(120), row.h),
        "launch_game_args",
        &mut app.form_game,
        "例如 --fullscreen",
    );
    form_y += theme.control_h + gap + gap;

    // ---- 按钮行 ----
    let btn_row = Rect::new(area.x, form_y, area.w, theme.control_h);
    let buttons = [
        ("保存配置", ButtonKind::Normal, 110),
        ("启动游戏", ButtonKind::Primary, 110),
        ("查看日志", ButtonKind::Normal, 110),
    ];
    let mut x = btn_row.x;
    let mut action: Option<&str> = None;
    for (label, kind, w) in buttons {
        if ui
            .button(Rect::new(x, btn_row.y, w, btn_row.h), label, kind, true)
            .clicked
        {
            action = Some(label);
        }
        x += w + gap;
    }
    match action {
        Some("保存配置") => match app.save_launch_config() {
            Ok(()) => app.toast("启动配置已保存", false),
            Err(e) => app.toast(e, true),
        },
        Some("启动游戏") => {
            // 模拟启动：不创建任何 OS 进程（框架约束），但日志真的会变。
            let name = app
                .selected_view()
                .map(|i| i.name)
                .unwrap_or_else(|| "未选择实例".to_owned());
            app.store()
                .push_log(format!("[INFO] 启动实例「{name}」（simulated）"));
            app.store().push_log("[INFO] pid=null：未创建任何 OS 进程");
            app.toast("模拟启动完成：未创建进程，已写入日志", false);
        }
        Some("查看日志") => {
            app.log_expanded = !app.log_expanded;
        }
        _ => {}
    }

    // ---- 日志区 ----
    let log_top = form_y + theme.control_h + gap;
    let log_area = Rect::new(area.x, log_top, area.w, (area.bottom() - log_top).max(0));
    if log_area.h > 60 {
        ui.fill_round(log_area, theme.input_bg, radius);
        ui.stroke_round(log_area, theme.input_border, radius, 1);
        let inner = log_area.shrink(EdgeInsets::all(theme.pad_small));
        ui.caption(
            inner.take_top(18),
            format!(
                "最近日志（{} 行，滚轮可滚动）{}",
                snap.log_lines.len(),
                if app.log_expanded { " · 全部" } else { "" }
            ),
        );
        let lines: Vec<&String> = if app.log_expanded {
            snap.log_lines.iter().collect()
        } else {
            snap.log_lines.iter().rev().take(6).collect()
        };
        let line_h = 16;
        let body = Rect::new(inner.x, inner.y + 20, inner.w, (inner.h - 20).max(0));
        let content_h = lines.len() as i32 * line_h;
        let offset = ui.scroll_area(log_scroll_id(), body, content_h);
        ui.push_clip(body);
        let mut ly = body.y - offset;
        for line in lines {
            if ly + line_h > body.y && ly < body.bottom() {
                ui.text(
                    Rect::new(body.x, ly, body.w, line_h),
                    line.clone(),
                    theme.text_faint,
                    theme.font_small,
                );
            }
            ly += line_h;
        }
        ui.pop_clip();
    }
}

/// 供测试：验证内存比例计算不失真。
pub fn memory_fraction(memory_mb: u32) -> f32 {
    (memory_mb as f32 / 32768.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_fraction_clamped() {
        assert_eq!(memory_fraction(0), 0.0);
        assert_eq!(memory_fraction(32768), 1.0);
        assert_eq!(memory_fraction(65536), 1.0, "超上限应被夹到 1.0");
        let f = memory_fraction(6144);
        assert!(f > 0.15 && f < 0.25, "6144/32768 ≈ 0.19，实际 {f}");
    }
}
