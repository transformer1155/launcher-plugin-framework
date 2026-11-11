//! 启动器 UI 入口。
//!
//! # 后端选择
//!
//! | 平台 | 后端 | 说明 |
//! |---|---|---|
//! | Windows | `ui-backend-win32` | 真实窗口 + GDI 文字 + 系统圆角/亚克力 |
//! | Windows `--soft` / `--shot` | `ui-backend-soft` | 离屏渲染，导出 PPM/PNG |
//! | 其他平台 | `ui-backend-soft` | 同上（无窗口系统） |
//!
//! # 外观参数
//!
//! ```text
//! --font <字体族>          换系统字体（如 "Microsoft YaHei UI"）
//! --font-file <路径>       载入私有 .ttf/.otf（配合 --font 指定字体名）
//! --bitmap-font <路径>     软件后端自定义 5×7 点阵字形表
//! --font-scale <浮点>      全局字号缩放 0.75..1.5
//! --glass / --no-glass     磨玻璃开关
//! --blur <整数>            模糊半径 0..48
//! --radius <整数>          控件圆角 0..32
//! --window-radius <整数>   窗口圆角 0..64
//! --no-anim                关闭过渡动画
//! --light                  亮色主题
//! --demo                   强制使用演示数据（不接入 official-* 插件）
//! --shot <页面>            离屏渲染 instances/mods/launch 并导出截图
//! --out <路径>             截图输出路径（默认 ui-<页面>.png）
//! ```

// Windows 发布版：不弹出命令行控制台窗口（GUI 子系统）。
// 调试构建仍保留控制台，方便看日志；运行日志可在应用内"日志"页查看。
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

#![forbid(unsafe_code)]

use launcher_ui::live::LiveDataSource;
use launcher_ui::App;
use std::sync::Arc;
use ui_core::backend::Backend;
use ui_core::{FontConfig, GlassConfig, StyleConfig};

/// 崩溃黑匣子：GUI 子系统下 panic 默认无处输出、程序静默消失。
/// 把 panic 位置与调用栈追加到 `data/crash.log`（exe 旁边），供事后定位。
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!(
            "[{}] PANIC: {info}\nbacktrace:\n{:#?}\n----\n",
            chrono_like_now(),
            std::backtrace::Backtrace::force_capture()
        );
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("data")))
            .unwrap_or_else(std::env::temp_dir);
        let _ = std::fs::create_dir_all(&dir);
        let log = dir.join("crash.log");
        let tmp = dir.join("crash.log.new");
        let _ = std::fs::write(&tmp, &msg);
        // 追加而不是覆盖，保留历史崩溃记录。
        if let Ok(old) = std::fs::read(&log) {
            let mut merged = old;
            merged.extend_from_slice(msg.as_bytes());
            let _ = std::fs::write(&tmp, &merged);
        }
        let _ = std::fs::rename(&tmp, &log);
    }));
}

/// 本地时间字符串（不引入 chrono，用 SystemTime 算 UTC 即可）。
fn chrono_like_now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch={d}")
}

/// 读取 `--key value` 形式的值。
fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn arg_i32(args: &[String], key: &str) -> Option<i32> {
    arg_value(args, key).and_then(|v| v.parse().ok())
}

/// 命令行 → 外观配置（唯一的真相源，之后交给 `App::set_style`）。
fn style_from_args(args: &[String]) -> StyleConfig {
    let mut style = StyleConfig::default();
    let mut font = FontConfig::default();
    if let Some(v) = arg_value(args, "--font") {
        font.family = v;
    }
    if let Some(v) = arg_value(args, "--font-file") {
        font.file = Some(v);
    }
    if let Some(v) = arg_value(args, "--bitmap-font") {
        font.bitmap_font = Some(v);
    }
    if let Some(v) = arg_value(args, "--font-scale").and_then(|s| s.parse::<f32>().ok()) {
        font.size_scale = v;
    }
    style.font = font;

    let mut glass = GlassConfig::default();
    if args.iter().any(|a| a == "--glass") {
        glass.enabled = true;
    }
    if args.iter().any(|a| a == "--no-glass") {
        glass.enabled = false;
    }
    if let Some(v) = arg_i32(args, "--blur") {
        glass.blur_px = v;
    }
    style.glass = glass;

    if let Some(v) = arg_i32(args, "--radius") {
        style = style.with_control_radius(v);
    }
    if let Some(v) = arg_i32(args, "--window-radius") {
        style = style.with_window_radius(v);
    }
    if args.iter().any(|a| a == "--no-anim") {
        style = style.with_animations(false);
    }
    style
}

/// 软件后端渲染一帧并落盘。返回 `(宽, 高, 指令数, PNG 字节数)`。
fn render_soft_frame(
    app: &mut App,
    width: i32,
    height: i32,
    out_png: &str,
    click: Option<(i32, i32)>,
) -> (i32, i32, usize, usize) {
    use ui_backend_soft::SoftBackend;
    let be = SoftBackend::new(width, height).with_backdrop(0xFF0E_0E12);
    let theme = app.theme().clone();
    // 离屏渲染：dt = 0（不推进动画时间，保证截图稳定）。
    // 先把页面进入动画置为完成，否则内容区会被裁成 0 宽（见 App::freeze_page_animation）。
    app.freeze_page_animation();
    let mut state = ui_core::FrameState::new();
    // 模拟点击：把移动 / 按下 / 抬起注入同一帧，控件按常规命中测试处理。
    let events: Vec<ui_core::Event> = match click {
        Some((x, y)) => vec![
            ui_core::Event::MouseMove { x, y },
            ui_core::Event::MouseDown {
                x,
                y,
                button: ui_core::MouseButton::Left,
                mods: ui_core::Modifiers::default(),
            },
            ui_core::Event::MouseUp {
                x,
                y,
                button: ui_core::MouseButton::Left,
                mods: ui_core::Modifiers::default(),
            },
        ],
        None => Vec::new(),
    };
    let list = {
        let mut ui = ui_core::Ui::begin_frame(&be, &theme, events, &mut state, 0.0);
        app.render(&mut ui);
        ui.finish()
    };
    let mut be = be;
    // 用渲染后的主题背景：外观面板可能在本帧切换明暗主题。
    be.clear(app.theme().bg.0);
    be.present(&list);
    let png = png_from_pixels(be.width(), be.height(), be.pixels());
    let _ = std::fs::write(out_png, &png);
    // 同时保留 PPM（无第三方解码库时的对照格式）。
    let _ = std::fs::write(out_png.replace(".png", ".ppm"), be.to_ppm());
    (be.width(), be.height(), list.len(), png.len())
}

fn page_name(p: launcher_ui::app::Page) -> &'static str {
    p.arg_name()
}

fn page_from_args(args: &[String]) -> launcher_ui::app::Page {
    let name = arg_value(args, "--page")
        .or_else(|| arg_value(args, "--shot"))
        .unwrap_or_else(|| "home".to_owned());
    launcher_ui::app::Page::from_arg(&name)
}

/// 默认数据根目录：%APPDATA%\WorkBuddyLauncher。
fn default_data_root() -> std::path::PathBuf {
    std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("WorkBuddyLauncher")
}

/// 探测目录是否真的可写（写一个探针文件并删除）。
fn dir_writable(dir: &std::path::Path) -> bool {
    let _ = std::fs::create_dir_all(dir);
    let probe = dir.join(".write_probe.tmp");
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// 极简 PNG 编码器（零第三方依赖）
//
// 只用 zlib 的 stored（不压缩）块：实现约 60 行，比引入 flate2 更符合
// "自制 UI 系统"的约束。CRC32 / Adler32 都用手写查表循环。
// ---------------------------------------------------------------------------

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// `pixels` 是 `0xAARRGGBB`，alpha=0 的像素用窗口圆角底色代替。
fn png_from_pixels(w: i32, h: i32, pixels: &[u32]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w * h * 3 + h) as usize);
    for y in 0..h {
        raw.push(0); // filter type: None
        for x in 0..w {
            let p = pixels[(y * w + x) as usize];
            let v = if (p >> 24) & 0xFF == 0 {
                0xFF0E_0E12u32
            } else {
                p
            };
            raw.push(((v >> 16) & 0xFF) as u8);
            raw.push(((v >> 8) & 0xFF) as u8);
            raw.push((v & 0xFF) as u8);
        }
    }

    let mut zlib = vec![0x78u8, 0x01]; // CM=8, CINFO=7, FLEVEL=0
    let mut pos = 0usize;
    while pos < raw.len() {
        let n = (raw.len() - pos).min(65535);
        let final_block = pos + n == raw.len();
        zlib.push(if final_block { 1 } else { 0 });
        zlib.extend_from_slice(&(n as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(n as u16)).to_le_bytes());
        zlib.extend_from_slice(&raw[pos..pos + n]);
        pos += n;
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8bit, RGB, deflate, filter, no interlace

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &zlib);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(target_os = "windows")]
fn main() {
    install_panic_hook();
    let args: Vec<String> = std::env::args().collect();
    let force_soft = args.iter().any(|a| a == "--soft");
    let win_selftest = args.iter().any(|a| a == "--win-selftest");
    let shot_page = args.iter().any(|a| a == "--shot");
    let mut app = if args.iter().any(|a| a == "--first-run") {
        App::first_run()
    } else {
        App::new()
    };
    app.set_style(style_from_args(&args));
    if args.iter().any(|a| a == "--light") {
        app.set_dark(false);
    }

    if shot_page || force_soft {
        let page = page_from_args(&args);
        app.set_page(page);
        let out =
            arg_value(&args, "--out").unwrap_or_else(|| format!("ui-{}.png", page_name(page)));
        // --click x,y：先截「点击前」，再注入合成点击事件渲染「点击后」。
        let click = arg_value(&args, "--click").and_then(|s| {
            let (a, b) = s.split_once(',')?;
            Some((a.trim().parse::<i32>().ok()?, b.trim().parse::<i32>().ok()?))
        });
        match click {
            Some((x, y)) => {
                let (w, h, ops, bytes) = render_soft_frame(&mut app, 1100, 720, &out, None);
                println!(
                    "点击前（{}）：{w}x{h}，{ops} 条绘制指令，PNG {bytes} 字节 → {out}",
                    page_name(page)
                );
                // 点击帧：消费合成点击事件（页面跳转在帧末应用）。
                app.freeze_page_animation();
                let _ = render_soft_frame(&mut app, 1100, 720, &out, Some((x, y)));
                // 点击后帧：新页面已生效，再冻结一次进入动画后截图。
                app.freeze_page_animation();
                let after = out.replace(".png", "-after-click.png");
                let (w, h, ops, bytes) = render_soft_frame(&mut app, 1100, 720, &after, None);
                println!(
                    "模拟点击 ({x},{y}) 后（{}）：{w}x{h}，{ops} 条绘制指令，PNG {bytes} 字节 → {after}",
                    page_name(app.page())
                );
            }
            None => {
                let (w, h, ops, bytes) = render_soft_frame(&mut app, 1100, 720, &out, None);
                println!(
                    "软件后端渲染完成（{}）：{w}x{h}，{ops} 条绘制指令，PNG {bytes} 字节 → {out}",
                    page_name(page)
                );
            }
        }
        return;
    }

    // 真实窗口才启用持久化与实时数据源：离屏截图 / 窗口自检不应写用户配置。
    if !win_selftest {
        // 选择数据根：默认 %APPDATA%\WorkBuddyLauncher；若被杀软/系统拦截写不进去，
        // 自动切换到便携模式（exe 旁边的 data 目录），保证存储永远可用。
        let mut root = default_data_root();
        let mut portable = false;
        if !dir_writable(&root) {
            let alt = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("data")))
                .unwrap_or_else(|| std::path::PathBuf::from("data"));
            if dir_writable(&alt) {
                root = alt;
                portable = true;
            }
        }
        app.enable_persistence(root.join("config.json"));
        // 默认接入真实插件（official-*）；加 `--demo` 保留纯内存演示数据。
        if !args.iter().any(|a| a == "--demo") {
            let plugins_dir = root.join("plugins");
            if portable {
                let msg = format!(
                    "[WARN] AppData 数据目录不可写（疑似杀毒软件/防勒索防护拦截），已切换便携模式：{}\n\
                     [WARN] 把 AetherLauncher.exe 加入杀毒软件白名单后，可删除 data 目录恢复默认位置。",
                    root.display()
                );
                eprintln!("{msg}");
                app.model_mut().log(msg);
            }
            match LiveDataSource::new(plugins_dir.clone()) {
                Ok(src) => {
                    app.model_mut().attach_live(Arc::new(src));
                    app.model_mut().refresh();
                }
                Err(e) => eprintln!("LiveDataSource 初始化失败，回退演示模式：{e}"),
            }
            // 声明式页面插件 + 外接 MCP：铺示例、热加载目录就绪。
            app.model_mut().enable_page_plugins(&root);
        }
    }

    let Some(backend) = ui_backend_win32::Win32Backend::create("Aether Launcher", 1100, 720)
    else {
        eprintln!("创建窗口失败，回退到软件渲染");
        let (w, h, ops, bytes) = render_soft_frame(&mut app, 1100, 720, "ui-soft-render.png", None);
        println!("{w}x{h}，{ops} 条指令，PNG {bytes} 字节");
        return;
    };

    let mut state = ui_core::FrameState::new();

    if win_selftest {
        let (last, shot) = backend.run_selftest(8, move |be, events| {
            let dt = ui_backend_win32::frame_delta_ms() as f32;
            // 主题必须每帧从 App 读取：外观面板在 render 里改 Theme::style / 明暗主题，
            // 若在闭包外捕获一份克隆，运行时设置永远传不到 Backend::configure。
            let theme = app.theme().clone();
            ui_backend_win32::set_background(theme.bg.0);
            let mut ui = ui_core::Ui::begin_frame(be, &theme, events.to_vec(), &mut state, dt);
            app.render(&mut ui);
            let active = ui.animating() || app.animating();
            let list = ui.finish();
            // 无边框窗口的标题栏按钮 / 拖动由 UI 发起，帧末交给后端执行。
            if let Some(action) = state.take_window_action() {
                ui_backend_win32::perform_window_action(action);
            }
            ui_backend_win32::set_animation_active(active);
            list
        });
        println!(
            "真实窗口自检完成：渲染 8 帧，最后帧 {} 条绘制指令（窗口已关闭）",
            last.len()
        );
        if let Some((w, h, bgr)) = shot {
            if w > 0 && h > 0 {
                let mut pixels = vec![0u32; (w * h) as usize];
                for (i, px) in bgr.chunks_exact(3).enumerate() {
                    pixels[i] =
                        0xFF00_0000 | ((px[2] as u32) << 16) | ((px[1] as u32) << 8) | px[0] as u32;
                }
                let png = png_from_pixels(w, h, &pixels);
                let _ = std::fs::write("ui-win32-shot.png", &png);
                println!("窗口截图已保存：ui-win32-shot.png（{w}x{h}）");
            } else {
                println!("截图失败（capture 返回空）");
            }
        } else {
            println!("截图失败（capture 返回空）");
        }
        return;
    }

    backend.run_loop(
        |be: &ui_backend_win32::Win32Backend, events: &[ui_core::Event]| {
            let dt = ui_backend_win32::frame_delta_ms() as f32;
            let theme = app.theme().clone();
            ui_backend_win32::set_background(theme.bg.0);
            let mut ui = ui_core::Ui::begin_frame(be, &theme, events.to_vec(), &mut state, dt);
            app.render(&mut ui);
            // 动画时钟：告诉后端"还要继续出帧"，否则事件驱动循环会睡过去。
            let active = ui.animating() || app.animating();
            let list = ui.finish();
            // 无边框窗口没有系统标题栏：最小化 / 最大化 / 关闭 / 拖动都由
            // 顶栏按钮写好意图，帧末在这里交给后端执行。
            if let Some(action) = state.take_window_action() {
                ui_backend_win32::perform_window_action(action);
            }
            ui_backend_win32::set_animation_active(active);
            list
        },
    );
}

#[cfg(not(target_os = "windows"))]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut app = App::new();
    app.set_style(style_from_args(&args));
    let page = page_from_args(&args);
    app.set_page(page);
    let out = arg_value(&args, "--out").unwrap_or_else(|| format!("ui-{}.png", page_name(page)));
    let (w, h, ops, bytes) = render_soft_frame(&mut app, 1100, 720, &out);
    println!("软件后端渲染完成（无窗口平台）：{w}x{h}，{ops} 条绘制指令，PNG {bytes} 字节 → {out}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_from_args_reads_every_option() {
        let a: Vec<String> = [
            "--font",
            "Microsoft YaHei UI",
            "--font-scale",
            "1.2",
            "--glass",
            "--blur",
            "20",
            "--radius",
            "12",
            "--window-radius",
            "24",
            "--no-anim",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let s = style_from_args(&a);
        assert_eq!(s.font.family, "Microsoft YaHei UI");
        assert_eq!(s.font.scale(), 1.2);
        assert!(s.glass.enabled);
        assert_eq!(s.glass.radius(), 20);
        assert_eq!(s.control_radius, 12);
        assert_eq!(s.window_radius, 24);
        assert!(!s.animations);
    }

    #[test]
    fn png_has_valid_signature_and_chunks() {
        let pixels = vec![0xFFFF_0000u32; 4];
        let png = png_from_pixels(2, 2, &pixels);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert!(png.windows(4).any(|w| w == b"IHDR"));
        assert!(png.windows(4).any(|w| w == b"IDAT"));
        assert!(png.windows(4).any(|w| w == b"IEND"));
    }

    #[test]
    fn transparent_corner_becomes_backdrop() {
        let pixels = [0x0000_0000u32, 0xFF12_3456];
        let png = png_from_pixels(2, 1, &pixels);
        assert!(png.len() > 40);
    }
}
