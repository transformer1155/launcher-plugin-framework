//! 回归测试：hover 状态必须改变绘制内容。
//!
//! # 为什么这条测试重要
//!
//! Win32 后端为了避免"无意义的重绘"（鼠标在空白处移动时 CPU 占用偏高），
//! 加入了 `DrawList::same_content` 比较：**内容相同就跳过重绘**。
//!
//! 如果 hover 状态不改变绘制内容，这个优化就会**吃掉交互反馈** ——
//! 鼠标悬停到按钮上时界面不会有任何反应。
//!
//! 这条测试把那个隐含前提变成显式断言：一旦有人改坏 hover 的绘制，
//! 测试会立刻失败，而不是等到用户抱怨"鼠标移上去没反应"。

use ui_core::widget::ButtonKind;
use ui_core::{Event, Modifiers, Rect, Theme, Ui};

fn frame_with(events: Vec<Event>) -> ui_core::DrawList {
    // 用软件后端：它实现 Backend，且不需要窗口。
    let be = ui_backend_soft::SoftBackend::new(200, 100);
    let theme = Theme::dark();
    let mut state = ui_core::FrameState::new();
    let mut ui = Ui::begin_frame(&be, &theme, events, &mut state, 0.0);
    ui.button(Rect::new(10, 10, 80, 30), "btn", ButtonKind::Normal, true);
    ui.finish()
}

/// 推进动画后的帧（dt > 0 让过渡真正走起来）。
///
/// # 为什么需要这个
///
/// hover 现在是**动画过渡**（PCL 的 `AniStart` 范式）：状态改变的那一帧，
/// 动画从"当前值"开始，所以**第一帧的画面与之前完全相同**。
///
/// 这不是 bug，是动画的正常语义。但它意味着"hover 是否改变画面"必须在
/// **动画推进之后**检查。
fn frame_animated(events: Vec<Event>, dt: f32) -> ui_core::DrawList {
    let be = ui_backend_soft::SoftBackend::new(200, 100);
    let theme = Theme::dark();
    let mut state = ui_core::FrameState::new();
    let mut ui = Ui::begin_frame(&be, &theme, events, &mut state, dt);
    ui.button(Rect::new(10, 10, 80, 30), "btn", ButtonKind::Normal, true);
    ui.finish()
}

#[test]
fn hover_changes_draw_content() {
    // 先渲染一帧普通状态，再渲染一帧 hover 状态（带时间推进）。
    let mut state = ui_core::FrameState::new();
    let be = ui_backend_soft::SoftBackend::new(200, 100);
    let theme = Theme::dark();

    let normal = {
        let mut ui = Ui::begin_frame(&be, &theme, vec![], &mut state, 16.0);
        ui.button(Rect::new(10, 10, 80, 30), "btn", ButtonKind::Normal, true);
        ui.finish()
    };
    let hovered = {
        let mut ui = Ui::begin_frame(
            &be,
            &theme,
            vec![Event::MouseMove { x: 50, y: 20 }],
            &mut state,
            16.0,
        );
        ui.button(Rect::new(10, 10, 80, 30), "btn", ButtonKind::Normal, true);
        ui.finish()
    };

    assert!(
        !normal.same_content(&hovered),
        "hover 必须改变绘制内容，否则重绘优化会吞掉交互反馈"
    );
}

#[test]
fn hover_animation_produces_intermediate_frame() {
    // ★ 动画的核心性质：hover 的过渡过程中存在**中间态**，
    // 它既不是普通态，也不是最终的 hover 色。
    //
    // 注意：hover 开始的那一帧画面 == 普通态（动画从当前值出发），
    // 这是动画的正常语义，不是 bug。
    let mut state = ui_core::FrameState::new();
    let be = ui_backend_soft::SoftBackend::new(200, 100);
    let theme = Theme::dark();

    // 第一帧：普通状态
    let normal = {
        let mut ui = Ui::begin_frame(&be, &theme, vec![], &mut state, 16.0);
        ui.button(Rect::new(10, 10, 80, 30), "btn", ButtonKind::Normal, true);
        ui.finish()
    };
    // 第二帧：hover 开始（动画刚启动，画面还是原色）
    let start = {
        let mut ui = Ui::begin_frame(
            &be,
            &theme,
            vec![Event::MouseMove { x: 50, y: 20 }],
            &mut state,
            16.0,
        );
        ui.button(Rect::new(10, 10, 80, 30), "btn", ButtonKind::Normal, true);
        ui.finish()
    };
    // 第三帧：动画推进 50ms —— 应当出现中间色
    let mid = {
        let mut ui = Ui::begin_frame(&be, &theme, vec![], &mut state, 50.0);
        ui.button(Rect::new(10, 10, 80, 30), "btn", ButtonKind::Normal, true);
        ui.finish()
    };

    assert!(
        !normal.same_content(&start),
        "hover 帧应与普通态不同（同一帧内动画已推进 dt）"
    );
    assert!(
        !start.same_content(&mid),
        "动画推进后画面必须改变（否则过渡没生效）"
    );
    assert!(
        !normal.same_content(&mid),
        "中间态应不同于普通态"
    );
}

#[test]
fn identical_input_yields_identical_content() {
    // 反向验证：同样输入两次 → 内容必须相同（否则 `same_content` 永远为 false，
    // 优化就完全失效了）。
    let a = frame_with(vec![Event::MouseMove { x: 50, y: 20 }]);
    let b = frame_with(vec![Event::MouseMove { x: 50, y: 20 }]);
    assert!(
        a.same_content(&b),
        "相同输入必须产生相同绘制内容"
    );
}

#[test]
fn mouse_outside_button_yields_same_content() {
    // 鼠标在按钮外移动 → hover 状态不变 → 内容应与无事件时相同。
    // 这正是"跳过无意义重绘"能生效的前提。
    let none = frame_with(vec![]);
    let outside = frame_with(vec![Event::MouseMove { x: 180, y: 80 }]);
    assert!(
        none.same_content(&outside),
        "鼠标在按钮外时绘制内容不应变化"
    );
}

/// 便捷构造：带修饰键的鼠标按下。
pub fn mouse_down(x: i32, y: i32, button: ui_core::MouseButton) -> Event {
    Event::MouseDown {
        x,
        y,
        button,
        mods: Modifiers::default(),
    }
}

/// 便捷构造：带修饰键的鼠标抬起。
pub fn mouse_up(x: i32, y: i32, button: ui_core::MouseButton) -> Event {
    Event::MouseUp {
        x,
        y,
        button,
        mods: Modifiers::default(),
    }
}
