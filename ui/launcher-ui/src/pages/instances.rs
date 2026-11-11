//! 实例管理页：列表 + 操作按钮。

use ui_core::backend::Backend;
use ui_core::draw::TextAlign;
use ui_core::layout::{EdgeInsets, Rect};
use ui_core::widget::ButtonKind;
use ui_core::{Theme, Ui};

use crate::app::App;
use crate::data::Snapshot;

pub fn render(app: &mut App, ui: &mut Ui<'_, impl Backend + ?Sized>, area: Rect, snap: &Snapshot) {
    let theme = app.theme().clone();
    let row_h = theme.control_h + 16;
    let gap = theme.gap;

    // ---- 标题行 ----
    let header_h = theme.font_heading + 8;
    let header = area.take_top(header_h);
    ui.heading(header, "实例列表");
    ui.fill(
        Rect::new(area.x, area.y + header_h, area.w, 1),
        theme.divider,
    );

    // ---- 操作行 ----
    let actions_y = area.y + header_h + theme.gap;
    let actions = Rect::new(area.x, actions_y, area.w, theme.control_h);

    let btn_w = 96;
    let mut x = actions.x;
    let new_btn = Rect::new(x, actions.y, btn_w, actions.h);
    if ui
        .button(new_btn, "新建实例", ButtonKind::Primary, true)
        .clicked
    {
        app.toast(
            "新建实例：需要接入 official-instance.create_instance",
            false,
        );
    }
    x += btn_w + gap;

    let del_btn = Rect::new(x, actions.y, btn_w, actions.h);
    let has_selection = app.selected_instance.is_some();
    let danger_resp = ui.button(del_btn, "删除", ButtonKind::Danger, has_selection);
    if danger_resp.clicked {
        // 二次确认：UI 侧用"必须再点一次"的简化交互。
        app.toast(
            "删除需要二次确认：将调用 delete_instance(confirm=true)",
            true,
        );
    }

    // 统计信息（右对齐）
    let stats = format!(
        "{} 个实例 · {} 个已安装 Mod",
        snap.instances.len(),
        snap.mods.len()
    );
    let tw = ui.measure(&stats, theme.font_small, false);
    ui.text_vcenter(
        Rect::new(actions.right() - tw, actions.y, tw, actions.h),
        stats,
        theme.text_dim,
        theme.font_small,
        TextAlign::Right,
        false,
    );

    // ---- 列表 ----
    let list_top = actions_y + theme.control_h + theme.gap;
    let list_area = Rect::new(area.x, list_top, area.w, (area.bottom() - list_top).max(0));

    if snap.instances.is_empty() {
        ui.empty_state(list_area, "还没有实例 —— 点击「新建实例」开始");
        return;
    }

    // 表头
    let head_h = 28;
    let head = list_area.take_top(head_h);
    ui.fill(head, theme.card);
    let cols = column_rects(head, theme.pad);
    ui.text_vcenter(
        cols[0],
        "名称",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        true,
    );
    ui.text_vcenter(
        cols[1],
        "版本",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        true,
    );
    ui.text_vcenter(
        cols[2],
        "加载器",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        true,
    );
    ui.text_vcenter(
        cols[3],
        "上次游玩",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        true,
    );

    let mut y = list_area.y + head_h + 1;
    let max_y = area.bottom();

    for inst in &snap.instances {
        if y + row_h > max_y {
            // 超出一屏：显示"还有更多"提示，而不是画出界
            let more = format!(
                "还有 {} 个实例未显示（可滚动，当前版本暂未启用滚动）",
                snap.instances.iter().filter(|i| i.id != inst.id).count()
            );
            ui.caption(Rect::new(area.x, y, area.w, 24), more);
            break;
        }

        let row = Rect::new(area.x, y, area.w, row_h);
        let selected = app.selected_instance.as_deref() == Some(inst.id.as_str());
        let resp = draw_row(ui, &theme, row, inst, selected);
        if resp {
            app.selected_instance = Some(inst.id.clone());
            app.toast(format!("已选中「{}」", inst.name), false);
        }
        y += row_h + 1;
    }
}

fn column_rects(rect: Rect, pad: i32) -> [Rect; 4] {
    let inner = rect.shrink(EdgeInsets::symmetric(pad, 0));
    // 名称占 40%，其余各 20%
    let w_name = inner.w * 40 / 100;
    let rest = inner.w - w_name;
    let w_rest = rest / 3;
    [
        Rect::new(inner.x, inner.y, w_name, inner.h),
        Rect::new(inner.x + w_name, inner.y, w_rest, inner.h),
        Rect::new(inner.x + w_name + w_rest, inner.y, w_rest, inner.h),
        Rect::new(
            inner.x + w_name + w_rest * 2,
            inner.y,
            rest - w_rest * 2,
            inner.h,
        ),
    ]
}

/// 画一行，返回是否被点击。
fn draw_row(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    theme: &Theme,
    row: Rect,
    inst: &crate::data::InstanceView,
    selected: bool,
) -> bool {
    let id = ui_core::WidgetId::derive(ui_core::WidgetId::NONE, 999, &inst.id);
    // 用 nav_item 的视觉风格但不复用其 ID 逻辑：这里直接做一个可点击区域
    let resp = ui.nav_item(row, "", selected);
    // 覆盖文字（nav_item 画了空标签，这里补列内容）
    let cols = column_rects(row, theme.pad);
    let fg = theme.text;
    ui.text_vcenter(
        cols[0],
        inst.name.clone(),
        fg,
        theme.font_body,
        TextAlign::Left,
        selected,
    );
    ui.text_vcenter(
        cols[1],
        inst.game_version.clone(),
        theme.text_dim,
        theme.font_body,
        TextAlign::Left,
        false,
    );
    ui.text_vcenter(
        cols[2],
        inst.loader.clone(),
        theme.text_dim,
        theme.font_body,
        TextAlign::Left,
        false,
    );
    let played = inst
        .last_played
        .as_deref()
        .map(short_time)
        .unwrap_or_else(|| "从未".to_owned());
    ui.text_vcenter(
        cols[3],
        played,
        theme.text_faint,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    let _ = id;
    resp.clicked
}

/// `2026-11-08T10:00:00Z` → `11-08 10:00`。
fn short_time(s: &str) -> String {
    if s.len() >= 16 {
        let d = &s[5..10];
        let t = &s[11..16];
        format!("{d} {t}")
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_time_formats_iso() {
        assert_eq!(short_time("2026-11-08T10:00:00Z"), "11-08 10:00");
        assert_eq!(short_time("bad"), "bad");
    }

    #[test]
    fn columns_sum_to_inner_width() {
        let r = Rect::new(0, 0, 1000, 30);
        let cols = column_rects(r, 12);
        let inner = r.shrink(EdgeInsets::symmetric(12, 0));
        assert_eq!(cols[0].w + cols[1].w + cols[2].w + cols[3].w, inner.w);
        assert_eq!(cols[0].x, inner.x);
        assert_eq!(cols[3].right(), inner.right());
    }
}
