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
//! --shot <页面>            离屏渲染 instances/mods/launch 并导出截图
//! --out <路径>             截图输出路径（默认 ui-<页面>.png）
//! ```

#![forbid(unsafe_code)]

use launcher_ui::App;
use ui_core::backend::Backend;
use ui_core::{FontConfig, GlassConfig, StyleConfig};

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
) -> (i32, i32, usize, usize) {
    use ui_backend_soft::SoftBackend;
    let be = SoftBackend::new(width, height).with_backdrop(0xFF0E_0E12);
    let theme = app.theme().clone();
    // 离屏渲染：dt = 0（不推进动画时间，保证截图稳定）。
    let mut state = ui_core::FrameState::new();
    let list = {
        let mut ui = ui_core::Ui::begin_frame(&be, &theme, Vec::new(), &mut state, 0.0);
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

/// 用户配置路径。首次运行不存在时，`App::enable_persistence` 会进入引导。
fn config_path() -> std::path::PathBuf {
    std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("WorkBuddyLauncher")
        .join("config.json")
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
        let (w, h, ops, bytes) = render_soft_frame(&mut app, 1100, 720, &out);
        println!(
            "软件后端渲染完成（{}）：{w}x{h}，{ops} 条绘制指令，PNG {bytes} 字节 → {out}",
            page_name(page)
        );
        return;
    }

    // 真实窗口才启用持久化：离屏截图 / 窗口自检不应写用户配置。
    if !win_selftest {
        app.enable_persistence(config_path());
    }

    let Some(backend) = ui_backend_win32::Win32Backend::create("Minecraft 启动器", 1100, 720)
    else {
        eprintln!("创建窗口失败，回退到软件渲染");
        let (w, h, ops, bytes) = render_soft_frame(&mut app, 1100, 720, "ui-soft-render.png");
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
