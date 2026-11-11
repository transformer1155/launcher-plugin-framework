//! 输入框帧级交互回归：点击聚焦 → 键入文本 → Ctrl+C / Ctrl+V 剪贴板。
//!
//! 这些测试用 SoftBackend 跨多帧驱动 `Ui`，复现真实窗口的事件时序
//! （MouseDown 与 MouseUp 在不同帧、文本事件在后续帧）。

use ui_backend_soft::SoftBackend;
use ui_core::{Backend, Event, FrameState, Modifiers, MouseButton, Rect, Theme, Ui};

const R: Rect = Rect::new(100, 100, 300, 32);

fn click_events(x: i32, y: i32) -> Vec<Event> {
    vec![
        Event::MouseMove { x, y },
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            mods: Modifiers::default(),
        },
        Event::MouseUp {
            x,
            y,
            button: MouseButton::Left,
            mods: Modifiers::default(),
        },
    ]
}

/// 跑一帧输入框，返回（新的文本，本帧是否被点击）。
fn frame(be: &SoftBackend, state: &mut FrameState, events: Vec<Event>, text: &mut String) -> bool {
    let theme = Theme::dark();
    let mut ui = Ui::begin_frame(be, &theme, events, state, 0.0);
    let resp = ui.text_input(R, "测试输入框", text, "占位");
    let clicked = resp.clicked;
    ui.finish();
    clicked
}

#[test]
fn click_focuses_then_typing_inserts() {
    let be = SoftBackend::new(640, 480).with_backdrop(0xFF0E_0E12);
    let mut state = FrameState::new();
    let mut text = String::new();

    // 帧 1：点击输入框 → 应获得焦点。
    let clicked = frame(&be, &mut state, click_events(200, 116), &mut text);
    assert!(clicked, "点击输入框应产生 clicked");

    // 帧 2：键入字符（无鼠标事件）→ 应插入。
    frame(&be, &mut state, vec![Event::Text { text: "a".into() }], &mut text);
    frame(&be, &mut state, vec![Event::Text { text: "b".into() }], &mut text);
    frame(
        &be,
        &mut state,
        vec![Event::Text { text: "c".into() }],
        &mut text,
    );
    assert_eq!(text, "abc", "聚焦后键入应插入文本");
}

#[test]
fn typing_without_focus_does_nothing() {
    let be = SoftBackend::new(640, 480).with_backdrop(0xFF0E_0E12);
    let mut state = FrameState::new();
    let mut text = String::new();
    // 没有点击过 → 没有焦点 → 键入不应生效。
    frame(&be, &mut state, vec![Event::Text { text: "x".into() }], &mut text);
    assert!(text.is_empty(), "未聚焦时键入应被忽略");
}

#[test]
fn click_elsewhere_clears_focus() {
    let be = SoftBackend::new(640, 480).with_backdrop(0xFF0E_0E12);
    let mut state = FrameState::new();
    let mut text = String::new();
    frame(&be, &mut state, click_events(200, 116), &mut text);
    frame(&be, &mut state, vec![Event::Text { text: "a".into() }], &mut text);
    assert_eq!(text, "a");
    // 点击空白处 → 失焦 → 键入失效。
    frame(&be, &mut state, click_events(600, 450), &mut text);
    frame(&be, &mut state, vec![Event::Text { text: "b".into() }], &mut text);
    assert_eq!(text, "a", "失焦后键入应被忽略");
}

#[test]
fn ctrl_c_copies_and_ctrl_v_pastes() {
    use ui_core::ShortcutKey;
    let be = SoftBackend::new(640, 480).with_backdrop(0xFF0E_0E12);
    let mut state = FrameState::new();
    let mut text = String::new();


    // 点击聚焦 → 输入 hello
    frame(&be, &mut state, click_events(200, 116), &mut text);
    for c in "hello".chars() {
        frame(&be, &mut state, vec![Event::Text { text: c.to_string() }], &mut text);
    }
    assert_eq!(text, "hello");

    // Ctrl+C → 剪贴板应有 hello
    frame(
        &be,
        &mut state,
        vec![Event::Shortcut(ShortcutKey::Ctrl('C'))],
        &mut text,
    );
    assert_eq!(be.clipboard_text(), "hello", "Ctrl+C 应复制整段文本");

    // 清空 + 失焦再聚焦（保证粘贴在干净状态）
    text.clear();
    frame(
        &be,
        &mut state,
        vec![Event::Shortcut(ShortcutKey::Ctrl('V'))],
        &mut text,
    );
    assert_eq!(text, "hello", "Ctrl+V 应把剪贴板内容粘进输入框");
}

#[test]
fn ctrl_v_pastes_external_clipboard() {
    use ui_core::ShortcutKey;
    let be = SoftBackend::new(640, 480).with_backdrop(0xFF0E_0E12);
    be.set_clipboard_text("外部文本");
    let mut state = FrameState::new();
    let mut text = String::new();
    frame(&be, &mut state, click_events(200, 116), &mut text);
    frame(
        &be,
        &mut state,
        vec![Event::Shortcut(ShortcutKey::Ctrl('V'))],
        &mut text,
    );
    assert_eq!(text, "外部文本", "Ctrl+V 应粘贴外部剪贴板内容");
}

#[test]
fn id_is_stable_across_widget_count_changes() {
    // 模拟条件控件：帧 A 在输入框前多画一个按钮，帧 B 不画。
    // 输入框 ID 必须不变，否则点击后焦点会丢。
    let be = SoftBackend::new(640, 480).with_backdrop(0xFF0E_0E12);
    let mut state = FrameState::new();
    let mut text = String::new();
    let theme = Theme::dark();

    let run = |be: &SoftBackend,
               state: &mut FrameState,
               events: Vec<Event>,
               text: &mut String,
               extra_button: bool| {
        let mut ui = Ui::begin_frame(be, &theme, events, state, 0.0);
        if extra_button {
            ui.button(Rect::new(100, 50, 80, 26), "X", ui_core::widget::ButtonKind::Normal, true);
        }
        let resp = ui.text_input(R, "测试输入框", text, "占位");
        ui.finish();
        resp.clicked
    };

    run(&be, &mut state, click_events(200, 116), &mut text, true);
    run(&be, &mut state, vec![Event::Text { text: "a".into() }], &mut text, false);
    assert_eq!(text, "a", "控件数量变化后输入框焦点应保持（稳定 ID）");
}
