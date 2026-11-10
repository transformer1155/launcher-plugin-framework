//! 回归测试：运行时外观配置 + 本轮新增交互。
//!
//! 三条隐含契约在这里被显式锁定：
//!
//! 1. `App::theme` 是**唯一的真相源**。Windows 消息循环必须每帧从 `App` 读主题，
//!    否则"外观设置"面板里改的圆角/字体/磨玻璃永远只存在于 App 内部。
//! 2. 右键菜单的命中结果在 `Ui::finish()` 里产生，只能在**下一帧**被业务代码消费，
//!    且下标包含分隔线（分隔线占位，但不响应点击）。
//! 3. `scroll_area` 的偏移是跨帧状态，必须能被滚轮推进并夹到内容高度。

use launcher_ui::app::Page;
use launcher_ui::App;
use ui_backend_soft::SoftBackend;
use ui_core::{
    Backend, DrawList, DrawOp, Event, FontConfig, FrameState, GlassConfig, MenuItem, Modifiers,
    MouseButton, Rect, StyleConfig, Ui, WidgetId,
};

/// 与 `main.rs` 的 `run_loop` 完全一致的取主题方式：每帧从 App 读一份新的。
fn frame(
    app: &mut App,
    be: &SoftBackend,
    state: &mut FrameState,
    events: Vec<Event>,
    dt: f32,
) -> DrawList {
    let theme = app.theme().clone();
    let mut ui = Ui::begin_frame(be, &theme, events, state, dt);
    app.render(&mut ui);
    ui.finish()
}

fn wheel(delta: i32) -> Event {
    Event::Wheel {
        x: 50,
        y: 50,
        delta,
        mods: Modifiers::default(),
    }
}

fn left_down(x: i32, y: i32) -> Event {
    Event::MouseDown {
        x,
        y,
        button: MouseButton::Left,
        mods: Modifiers::default(),
    }
}

fn right_down(x: i32, y: i32) -> Event {
    Event::MouseDown {
        x,
        y,
        button: MouseButton::Right,
        mods: Modifiers::default(),
    }
}

#[test]
fn switching_theme_keeps_runtime_style() {
    let mut app = App::new();
    let style = StyleConfig::default()
        .with_control_radius(20)
        .with_window_radius(30)
        .with_glass(GlassConfig::enabled(24))
        .with_font(FontConfig::family("Microsoft YaHei UI"))
        .with_animations(false);
    app.set_style(style.clone());
    let dark_bg = app.theme().bg.0;

    app.set_dark(false);

    assert_eq!(app.theme().style, style, "换明暗主题不能丢掉外观配置");
    assert_ne!(app.theme().bg.0, dark_bg, "暗色/亮色的背景必须真的不同");
}

#[test]
fn runtime_radius_change_reaches_next_frame() {
    let be = SoftBackend::new(1100, 720);
    let mut state = FrameState::new();
    let mut app = App::new();
    // 关掉动画，排除 DrawList 因过渡时间产生的差异。
    app.set_style(StyleConfig::default().with_animations(false));
    let before = frame(&mut app, &be, &mut state, vec![], 0.0);

    // 模拟"外观设置"面板里把控件圆角从 8 改成 24。
    app.style_mut().control_radius = 24;
    let after = frame(&mut app, &be, &mut state, vec![], 0.0);

    assert!(
        !before.same_content(&after),
        "运行时改圆角必须改变绘制内容，否则 Backend::configure 拿到的是过期主题"
    );

    // Ui 的 theme 也必须来自同一份新配置（main.rs 的修复点）。
    let theme = app.theme().clone();
    let ui = Ui::begin_frame(&be, &theme, vec![], &mut state, 0.0);
    assert_eq!(ui.radius(), 24);
    ui.finish();
}

#[test]
fn glass_toggle_emits_blur_region() {
    let be = SoftBackend::new(1100, 720);
    let mut state = FrameState::new();
    let mut app = App::new();
    app.set_style(
        StyleConfig::default()
            .with_glass(GlassConfig::enabled(18))
            .with_animations(false),
    );
    let glassy = frame(&mut app, &be, &mut state, vec![], 0.0);
    assert!(
        glassy
            .ops()
            .iter()
            .any(|op| matches!(op, DrawOp::BlurRegion { radius: 18, .. })),
        "开启磨玻璃必须产生带指定半径的 BlurRegion 指令"
    );

    app.set_style(StyleConfig::default().with_animations(false));
    let plain = frame(&mut app, &be, &mut state, vec![], 0.0);
    assert!(
        !plain
            .ops()
            .iter()
            .any(|op| matches!(op, DrawOp::BlurRegion { .. })),
        "关闭磨玻璃后不应残留模糊指令"
    );
}

#[test]
fn animation_switch_disables_page_clock() {
    let mut app = App::new();
    // 首屏直接落到终点，不播进入动画（否则启动就白等一帧动画）。
    assert!(!app.animating());
    app.set_page(Page::Mods);
    assert!(app.animating(), "切页后应驱动 Win32 的 16ms 心跳");

    app.set_style(StyleConfig::default().with_animations(false));
    assert!(
        !app.animating(),
        "关掉动画后不能继续驱动心跳（否则 CPU 白转）"
    );
    app.set_page(Page::Launch);
    assert!(!app.animating(), "关动画后再切页也不应重新启动时钟");
}

#[test]
fn context_menu_click_is_consumed_on_next_frame() {
    let be = SoftBackend::new(400, 300);
    let theme = ui_core::Theme::dark();
    let mut state = FrameState::new();
    // 与 App::open_instance_menu 完全相同的菜单形状：下标 2 是分隔线。
    let items = vec![
        MenuItem::new("设为当前实例"),
        MenuItem::new("新建实例"),
        MenuItem::separator(),
        MenuItem::new("删除该实例"),
    ];
    let area = Rect::new(0, 0, 400, 300);

    // 帧 1：右键打开菜单。
    {
        let mut ui = Ui::begin_frame(&be, &theme, vec![right_down(100, 100)], &mut state, 0.0);
        assert_eq!(ui.right_click_pos(), Some((100, 100)));
        assert!(ui.take_menu_click().is_none());
        ui.open_context_menu(area, items.clone(), 170);
        assert!(ui.menu_open());
        ui.finish();
    }

    // 帧 2：点击第 4 项（y = origin.y + 3*28 + 半行高）。
    {
        let ui = Ui::begin_frame(&be, &theme, vec![left_down(120, 198)], &mut state, 0.0);
        assert!(
            ui.take_menu_click().is_none(),
            "命中测试发生在 finish()，帧首还读不到结果"
        );
        ui.finish();
    }

    // 帧 3：业务代码在 render 开头消费结果。
    {
        let ui = Ui::begin_frame(&be, &theme, vec![], &mut state, 0.0);
        assert_eq!(
            ui.take_menu_click(),
            Some(3),
            "分隔线占位，删除项的下标必须是 3（与 App::handle_menu_action 对齐）"
        );
        ui.finish();
    }

    // 帧 4：取完即清空，避免一次点击被重复执行。
    {
        let ui = Ui::begin_frame(&be, &theme, vec![], &mut state, 0.0);
        assert!(ui.take_menu_click().is_none());
        ui.finish();
    }
}

#[test]
fn separator_cannot_be_clicked() {
    let be = SoftBackend::new(400, 300);
    let theme = ui_core::Theme::dark();
    let mut state = FrameState::new();
    let items = vec![
        MenuItem::new("设为当前实例"),
        MenuItem::new("新建实例"),
        MenuItem::separator(),
        MenuItem::new("删除该实例"),
    ];
    {
        let mut ui = Ui::begin_frame(&be, &theme, vec![right_down(100, 100)], &mut state, 0.0);
        ui.open_context_menu(Rect::new(0, 0, 400, 300), items, 170);
        ui.finish();
    }
    {
        // y = 100 + 2*28 + 14：正好落在分隔线上。
        let ui = Ui::begin_frame(&be, &theme, vec![left_down(120, 170)], &mut state, 0.0);
        assert!(ui.menu_open());
        ui.finish();
    }
    {
        let ui = Ui::begin_frame(&be, &theme, vec![], &mut state, 0.0);
        assert!(ui.take_menu_click().is_none());
        ui.finish();
    }
}

#[test]
fn scroll_area_advances_and_clamps() {
    let be = SoftBackend::new(200, 200);
    let theme = ui_core::Theme::dark();
    let mut state = FrameState::new();
    let id = WidgetId::derive(WidgetId::NONE, 0, "instance-list");
    let area = Rect::new(0, 0, 100, 100);

    // 后端约定：delta = -1 表示向下滚一格，一个 notch = 48px。
    let step = |state: &mut FrameState, d: i32, content_h: i32| {
        let mut ui = Ui::begin_frame(&be, &theme, vec![wheel(d)], state, 0.0);
        let off = ui.scroll_area(id, area, content_h);
        ui.finish();
        off
    };
    assert_eq!(step(&mut state, -1, 400), 48);
    assert_eq!(step(&mut state, -1, 400), 96, "偏移必须跨帧累加");
    assert_eq!(
        step(&mut state, -10, 400),
        300,
        "最大偏移 = 内容高 - 可视高"
    );

    // 内容变短时，即使没有滚轮事件也要把偏移收回合法范围。
    let mut ui = Ui::begin_frame(&be, &theme, vec![], &mut state, 0.0);
    let shrunk = ui.scroll_area(id, area, 120);
    ui.finish();
    assert_eq!(shrunk, 20);
}

#[test]
fn font_scale_changes_text_metrics_and_reverts() {
    let be = SoftBackend::new(200, 100);
    let base_w = be.text_width("ABC", 14, false);
    let base_h = be.line_height(14, false);

    let big = StyleConfig::default().with_font(FontConfig {
        size_scale: 1.5,
        ..Default::default()
    });
    be.configure(&big);
    assert!(
        be.text_width("ABC", 14, false) > base_w,
        "字号缩放要影响排版"
    );
    assert!(be.line_height(14, false) > base_h, "行高也要跟着缩放");

    // configure 每帧调用：切回默认必须清掉缓存，不能残留放大后的测量值。
    be.configure(&StyleConfig::default());
    assert_eq!(be.text_width("ABC", 14, false), base_w);
    assert_eq!(be.line_height(14, false), base_h);
}
