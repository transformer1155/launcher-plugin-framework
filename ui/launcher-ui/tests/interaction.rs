//! 交互逻辑测试：对齐 PCL 的交互范式。
//!
//! # 参考
//!
//! PCL 的 `MyButton.xaml.vb`：
//!
//! ```vb
//! Private IsMouseDown As Boolean = False
//! Private Sub Button_MouseDown(...) Handles Me.MouseLeftButtonDown
//!     IsMouseDown = True
//! End Sub
//! Private Sub Button_MouseUp(...) Handles Me.MouseLeftButtonUp
//!     If Not IsMouseDown Then Return
//!     RaiseEvent Click(sender, e)
//! End Sub
//! Private Sub Button_MouseLeave(...) Handles Me.MouseLeave
//!     If Not IsMouseDown Then Return
//!     IsMouseDown = False
//! End Sub
//! ```
//!
//! 本文件把那些行为变成可断言的测试。

use ui_core::widget::ButtonKind;
use ui_core::{Event, Modifiers, MouseButton, Rect, Theme, Ui};

const BTN: Rect = Rect {
    x: 10,
    y: 10,
    w: 80,
    h: 30,
};

fn mouse_down(x: i32, y: i32) -> Event {
    Event::MouseDown {
        x,
        y,
        button: MouseButton::Left,
        mods: Modifiers::default(),
    }
}

fn mouse_down_with(x: i32, y: i32, mods: Modifiers) -> Event {
    Event::MouseDown {
        x,
        y,
        button: MouseButton::Left,
        mods,
    }
}

fn mouse_up(x: i32, y: i32) -> Event {
    Event::MouseUp {
        x,
        y,
        button: MouseButton::Left,
        mods: Modifiers::default(),
    }
}

/// 跑一帧，返回（是否点击、是否按下、是否悬停）。
fn frame(events: Vec<Event>, state: &mut ui_core::FrameState) -> (bool, bool, bool) {
    let be = ui_backend_soft::SoftBackend::new(200, 100);
    let theme = Theme::dark();
    let mut ui = Ui::begin_frame(&be, &theme, events, state, 0.0);
    let r = ui.button(BTN, "btn", ButtonKind::Normal, true);
    (r.clicked, r.pressed, r.hovered)
}

#[test]
fn click_inside_button_triggers_click() {
    let mut state = ui_core::FrameState::new();
    let (clicked, _, _) = frame(vec![mouse_down(40, 20), mouse_up(40, 20)], &mut state);
    assert!(clicked, "在按钮内按下并松开应触发点击");
}

#[test]
fn press_then_release_outside_does_not_click() {
    // ★ PCL 的关键行为：按下后把鼠标拖出按钮再松开，**不算点击**。
    let mut state = ui_core::FrameState::new();

    let (_, pressed, _) = frame(vec![mouse_down(40, 20)], &mut state);
    assert!(pressed, "按下应进入按下态");

    let (clicked, _, _) = frame(vec![mouse_up(180, 80)], &mut state);
    assert!(!clicked, "拖出按钮后松开不应触发点击");
}

#[test]
fn mouse_leave_cancels_pressed_state() {
    let mut state = ui_core::FrameState::new();
    frame(vec![mouse_down(40, 20)], &mut state);

    let (_, pressed, hovered) = frame(vec![Event::MouseMove { x: 180, y: 80 }], &mut state);
    assert!(!hovered, "鼠标已移出");
    assert!(!pressed, "移出后不应仍是按下态");
}

#[test]
fn press_state_persists_across_frames() {
    let mut state = ui_core::FrameState::new();
    frame(vec![mouse_down(40, 20)], &mut state);

    let (_, pressed, _) = frame(vec![], &mut state);
    assert!(pressed, "按住不放时，按下态应跨帧保持");

    let (clicked, _, _) = frame(vec![mouse_up(40, 20)], &mut state);
    assert!(clicked, "按住多帧后在按钮内松开仍应触发点击");
}

#[test]
fn click_grants_keyboard_focus() {
    let mut state = ui_core::FrameState::new();
    frame(vec![mouse_down(40, 20), mouse_up(40, 20)], &mut state);
    assert!(state.focused().is_some(), "点击按钮后应获得键盘焦点");
}

#[test]
fn enter_activates_focused_button() {
    let mut state = ui_core::FrameState::new();
    frame(vec![mouse_down(40, 20), mouse_up(40, 20)], &mut state);

    let (clicked, _, _) = frame(
        vec![Event::KeyDown {
            key: ui_core::Key::Enter,
            mods: Modifiers::default(),
        }],
        &mut state,
    );
    assert!(clicked, "焦点在按钮上时 Enter 应触发点击");
}

#[test]
fn tab_moves_focus_between_buttons() {
    let be = ui_backend_soft::SoftBackend::new(200, 100);
    let theme = Theme::dark();
    let mut state = ui_core::FrameState::new();

    // 第一帧：登记两个按钮
    {
        let mut ui = Ui::begin_frame(&be, &theme, vec![], &mut state, 0.0);
        ui.button(Rect::new(10, 10, 80, 30), "a", ButtonKind::Normal, true);
        ui.button(Rect::new(10, 50, 80, 30), "b", ButtonKind::Normal, true);
        ui.finish();
    }

    // 第二帧：按 Tab
    {
        let mut ui = Ui::begin_frame(
            &be,
            &theme,
            vec![Event::KeyDown {
                key: ui_core::Key::Tab,
                mods: Modifiers::default(),
            }],
            &mut state,
            0.0,
        );
        ui.button(Rect::new(10, 10, 80, 30), "a", ButtonKind::Normal, true);
        ui.button(Rect::new(10, 50, 80, 30), "b", ButtonKind::Normal, true);
        ui.finish();
    }

    assert!(state.focused().is_some(), "Tab 应设置焦点");
}

#[test]
fn arrow_keys_navigate_spatially() {
    // ★ 方向键导航：焦点在上面的按钮，按 Down 应跳到下面的按钮。
    let be = ui_backend_soft::SoftBackend::new(200, 100);
    let theme = Theme::dark();
    let mut state = ui_core::FrameState::new();

    let draw = |state: &mut ui_core::FrameState| {
        let mut ui = Ui::begin_frame(&be, &theme, vec![], state, 0.0);
        ui.button(Rect::new(10, 10, 80, 30), "top", ButtonKind::Normal, true);
        ui.button(
            Rect::new(10, 50, 80, 30),
            "bottom",
            ButtonKind::Normal,
            true,
        );
        ui.finish();
    };

    draw(&mut state);
    // 焦点先落在第一个按钮上。
    // 注意：`focus_next()` 依赖"本帧登记了哪些控件"，所以必须先渲染一帧
    // （控件在渲染时登记自己），再取焦点。
    let first = state.focus.focus_next();
    state.focus.set_focus(first);
    // 再渲染一帧，让帧末的"焦点同步"把焦点写进共享槽。
    draw(&mut state);
    let top_focus = state.focused();
    assert!(top_focus.is_some());

    // 按 Down
    {
        let mut ui = Ui::begin_frame(
            &be,
            &theme,
            vec![Event::KeyDown {
                key: ui_core::Key::Down,
                mods: Modifiers::default(),
            }],
            &mut state,
            0.0,
        );
        ui.button(Rect::new(10, 10, 80, 30), "top", ButtonKind::Normal, true);
        ui.button(
            Rect::new(10, 50, 80, 30),
            "bottom",
            ButtonKind::Normal,
            true,
        );
        ui.finish();
    }
    assert_ne!(state.focused(), top_focus, "按 Down 后焦点应移动到别的按钮");
}

#[test]
fn disabled_button_does_not_click() {
    let be = ui_backend_soft::SoftBackend::new(200, 100);
    let theme = Theme::dark();
    let mut state = ui_core::FrameState::new();
    let mut ui = Ui::begin_frame(
        &be,
        &theme,
        vec![mouse_down(40, 20), mouse_up(40, 20)],
        &mut state,
        0.0,
    );
    let r = ui.button(BTN, "btn", ButtonKind::Normal, false);
    assert!(!r.clicked, "禁用的按钮不应响应点击");
    assert!(!r.hovered, "禁用的按钮不应有 hover 态");
}

#[test]
fn ctrl_state_is_visible_to_widgets() {
    // 修饰键要能正确传到控件（多选列表的 Ctrl+点击 依赖它）。
    let be = ui_backend_soft::SoftBackend::new(200, 100);
    let theme = Theme::dark();
    let mut state = ui_core::FrameState::new();
    let mods = Modifiers {
        ctrl: true,
        shift: false,
        alt: false,
    };

    let ui = Ui::begin_frame(
        &be,
        &theme,
        vec![mouse_down_with(40, 20, mods)],
        &mut state,
        0.0,
    );
    assert!(ui.ctrl_held(), "Ctrl 按下时 ctrl_held 应为 true");
    assert!(!ui.shift_held(), "未按 Shift 时 shift_held 应为 false");
    ui.finish();
}

#[test]
fn shift_state_is_visible_to_widgets() {
    let be = ui_backend_soft::SoftBackend::new(200, 100);
    let theme = Theme::dark();
    let mut state = ui_core::FrameState::new();
    let mods = Modifiers {
        ctrl: false,
        shift: true,
        alt: false,
    };

    let ui = Ui::begin_frame(
        &be,
        &theme,
        vec![mouse_down_with(40, 20, mods)],
        &mut state,
        0.0,
    );
    assert!(ui.shift_held(), "Shift 按下时 shift_held 应为 true");
    assert!(!ui.ctrl_held());
    ui.finish();
}
