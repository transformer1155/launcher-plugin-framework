//! Mod 管理页：已安装列表 + 搜索 + 安装/移除。

use ui_core::backend::Backend;
use ui_core::draw::TextAlign;
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
    ui.heading(area.take_top(header_h), "Mod 管理");
    ui.fill(
        Rect::new(area.x, area.y + header_h, area.w, 1),
        theme.divider,
    );

    // ---- 搜索行 ----
    let y = area.y + header_h + gap;
    let search_row = Rect::new(area.x, y, area.w, theme.control_h);
    let input_w = (search_row.w - 3 * gap - 90 - 90).max(120);
    let input_rect = Rect::new(search_row.x, search_row.y, input_w, search_row.h);

    // 搜索框状态由 App 持有（这里用 toast 代替实际搜索，因为无网络宿主接入）
    let mut query = app_search_text(app);
    ui.text_input(
        input_rect,
        "mod_search",
        &mut query,
        "搜索 Mod（例如 jei、sodium）",
    );
    set_search_text(app, query);

    let mut x = input_rect.right() + gap;
    let search_btn = Rect::new(x, search_row.y, 90, search_row.h);
    if ui
        .button(search_btn, "搜索", ButtonKind::Primary, true)
        .clicked
    {
        app.toast("搜索需要接入 host-http + official-mod.search_mods", false);
    }
    x += 90 + gap;

    let installed_count = snap.mods.len();
    let install_btn = Rect::new(x, search_row.y, 90, search_row.h);
    if ui
        .button(install_btn, "安装", ButtonKind::Normal, false)
        .clicked
    {
        let _ = installed_count;
    }

    // ---- 已安装列表 ----
    let list_top = y + theme.control_h + gap;
    let list_area = Rect::new(area.x, list_top, area.w, (area.bottom() - list_top).max(0));

    if snap.mods.is_empty() {
        ui.empty_state(list_area, "该实例还没有安装 Mod");
        return;
    }

    let head_h = 28;
    let head = list_area.take_top(head_h);
    ui.fill(head, theme.card);
    let cols = columns3(head, theme.pad);
    ui.text_vcenter(
        cols[0],
        "文件名",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        true,
    );
    ui.text_vcenter(
        cols[1],
        "名称",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        true,
    );
    ui.text_vcenter(
        cols[2],
        "操作",
        theme.text_dim,
        theme.font_small,
        TextAlign::Right,
        true,
    );

    let row_h = theme.control_h;
    let mut ry = list_area.y + head_h + 1;
    let mut to_remove: Option<String> = None;

    for m in &snap.mods {
        if ry + row_h > area.bottom() {
            break;
        }
        let row = Rect::new(area.x, ry, area.w, row_h);
        let c = columns3(row, theme.pad);

        ui.fill(Rect::new(row.x, row.bottom(), row.w, 1), theme.divider);
        ui.text_vcenter(
            c[0],
            m.filename.clone(),
            theme.text,
            theme.font_body,
            TextAlign::Left,
            false,
        );
        ui.text_vcenter(
            c[1],
            format!("{}  {}", m.name, m.version),
            theme.text_dim,
            theme.font_body,
            TextAlign::Left,
            false,
        );

        let btn_w = 72;
        let btn = Rect::new(
            c[2].right() - btn_w,
            row.y + (row_h - theme.control_h_small) / 2,
            btn_w,
            theme.control_h_small,
        );
        if ui.button(btn, "移除", ButtonKind::Ghost, true).clicked {
            to_remove = Some(m.filename.clone());
        }
        ry += row_h + 1;
    }

    if let Some(f) = to_remove {
        // 真正移除需要调用 official-mod.remove_mod
        app.toast(format!("将调用 remove_mod(filename='{f}')"), false);
    }
}

/// 读取搜索框文本。
fn app_search_text(app: &App) -> String {
    app.mod_search.clone()
}

fn set_search_text(app: &mut App, v: String) {
    app.mod_search = v;
}

fn columns3(rect: Rect, pad: i32) -> [Rect; 3] {
    let inner = rect.shrink(EdgeInsets::symmetric(pad, 0));
    let w0 = inner.w * 42 / 100;
    let w1 = inner.w * 35 / 100;
    let w2 = inner.w - w0 - w1;
    [
        Rect::new(inner.x, inner.y, w0, inner.h),
        Rect::new(inner.x + w0, inner.y, w1, inner.h),
        Rect::new(inner.x + w0 + w1, inner.y, w2, inner.h),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns3_sum_to_inner() {
        let r = Rect::new(0, 0, 900, 30);
        let c = columns3(r, 10);
        let inner = r.shrink(EdgeInsets::symmetric(10, 0));
        assert_eq!(c[0].w + c[1].w + c[2].w, inner.w);
        assert_eq!(c[2].right(), inner.right());
    }

    #[test]
    fn search_text_thread_local_roundtrip() {
        let mut app = App::new();
        set_search_text(&mut app, "jei".to_owned());
        assert_eq!(app_search_text(&app), "jei");
        set_search_text(&mut app, String::new());
        assert_eq!(app_search_text(&app), "");
    }
}
