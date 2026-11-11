//! 启动设置页：JVM 参数 / 游戏参数 / 内存 / 启动按钮 / 日志。

use ui_core::backend::Backend;
use ui_core::layout::{EdgeInsets, Rect};
use ui_core::widget::ButtonKind;
use ui_core::Ui;

use crate::app::App;
use crate::data::Snapshot;

pub fn render(app: &mut App, ui: &mut Ui<'_, impl Backend + ?Sized>, area: Rect, snap: &Snapshot) {
    let theme = app.theme().clone();
    let gap = theme.gap;

    // ---- 标题 ----
    let header_h = theme.font_heading + 8;
    ui.heading(area.take_top(header_h), "启动设置");
    ui.fill(
        Rect::new(area.x, area.y + header_h, area.w, 1),
        theme.divider,
    );

    // 当前实例名
    let inst_name = app
        .selected_view()
        .map(|i| i.name)
        .unwrap_or_else(|| "（未选择实例）".to_owned());
    let y = area.y + header_h + gap;
    ui.caption(
        Rect::new(area.x, y, area.w, 20),
        format!("当前实例：{inst_name}"),
    );

    let cfg = snap.launch_config.clone();
    let mut form_y = y + 26;

    // ---- 内存 ----
    let row = Rect::new(area.x, form_y, area.w, theme.control_h);
    ui.label(Rect::new(row.x, row.y, 120, row.h), "内存上限", false);
    let mem_text = cfg
        .as_ref()
        .map(|c| format!("{} MB", c.memory_mb))
        .unwrap_or_else(|| "—".to_owned());
    let field = Rect::new(row.x + 120, row.y, 160, row.h);
    ui.fill(field, theme.input_bg);
    ui.stroke(field, theme.input_border, theme.border);
    ui.text_vcenter(
        field.shrink(EdgeInsets::symmetric(theme.pad_small, 0)),
        mem_text,
        theme.text,
        theme.font_body,
        TextAlign::Left,
        false,
    );
    // 内存条（可视化比例：假定上限 32768）
    let bar = Rect::new(field.right() + gap, row.y + row.h / 2 - 4, 260, 8);
    let frac = cfg
        .as_ref()
        .map(|c| (c.memory_mb as f32) / 32768.0)
        .unwrap_or(0.0);
    ui.progress(bar, frac);
    form_y += theme.control_h + gap;

    // ---- JVM 参数 ----
    let row = Rect::new(area.x, form_y, area.w, theme.control_h);
    ui.label(Rect::new(row.x, row.y, 120, row.h), "JVM 参数", false);
    let jvm = cfg
        .as_ref()
        .map(|c| c.jvm_args.join("  "))
        .unwrap_or_else(|| "—".to_owned());
    let field = Rect::new(row.x + 120, row.y, (row.w - 130).max(0), row.h);
    ui.fill(field, theme.input_bg);
    ui.stroke(field, theme.input_border, theme.border);
    ui.text_vcenter(
        field.shrink(EdgeInsets::symmetric(theme.pad_small, 0)),
        jvm,
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    form_y += theme.control_h + gap;

    // ---- 游戏参数 ----
    let row = Rect::new(area.x, form_y, area.w, theme.control_h);
    ui.label(Rect::new(row.x, row.y, 120, row.h), "游戏参数", false);
    let game_args = cfg
        .as_ref()
        .map(|c| {
            if c.game_args.is_empty() {
                "（无）".to_owned()
            } else {
                c.game_args.join("  ")
            }
        })
        .unwrap_or_else(|| "—".to_owned());
    let field = Rect::new(row.x + 120, row.y, (row.w - 130).max(0), row.h);
    ui.fill(field, theme.input_bg);
    ui.stroke(field, theme.input_border, theme.border);
    ui.text_vcenter(
        field.shrink(EdgeInsets::symmetric(theme.pad_small, 0)),
        game_args,
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    form_y += theme.control_h + gap + gap;

    // ---- 按钮行 ----
    let btn_row = Rect::new(area.x, form_y, area.w, theme.control_h);
    let lots = [
        ("保存配置", ButtonKind::Normal, 110),
        ("启动游戏", ButtonKind::Primary, 110),
        ("查看日志", ButtonKind::Normal, 110),
    ];
    let mut x = btn_row.x;
    for (label, kind, w) in lots {
        let r = Rect::new(x, btn_row.y, w, btn_row.h);
        let resp = ui.button(r, label, kind, cfg.is_some());
        if resp.clicked {
            match label {
                "保存配置" => app.toast("将调用 set_launch_config", false),
                "启动游戏" => {
                    app.toast("launch_game 返回 pid=null（模拟启动，未创建进程）", false)
                }
                _ => app.toast("将调用 get_last_log", false),
            }
        }
        x += w + gap;
    }

    // ---- 日志区 ----
    let log_top = form_y + theme.control_h + gap * 2;
    let log_area = Rect::new(area.x, log_top, area.w, (area.bottom() - log_top).max(0));
    if log_area.h > 60 {
        ui.fill(log_area, theme.input_bg);
        ui.stroke(log_area, theme.input_border, theme.border);
        let inner = log_area.shrink(EdgeInsets::all(theme.pad_small));
        ui.caption(inner.take_top(18), "最近日志（模拟）");
        let lines = [
            "[10:00:00] [INFO] 加载实例配置",
            "[10:00:01] [INFO] 解析 JVM 参数 2 项",
            "[10:00:02] [WARN] memory_mb=6144 超过推荐值 4096",
            "[10:00:03] [INFO] 启动（simulated）：未创建任何 OS 进程",
        ];
        let mut ly = inner.y + 20;
        for line in lines {
            if ly + 16 > inner.bottom() {
                break;
            }
            ui.text(
                Rect::new(inner.x, ly, inner.w, 16),
                line,
                theme.text_faint,
                theme.font_small,
            );
            ly += 16;
        }
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
