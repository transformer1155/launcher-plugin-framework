# 自研 UI 系统 —— 交付说明

一个**零第三方 GUI 依赖**的 Minecraft 启动器界面，含即时模式 UI 内核、两个渲染后端，以及一套
完整的启动器界面与交互（首次引导、首页、版本 / 账户 / 下载 / 设置 / 日志、启动流程、全局反馈）。

窗口采用**无边框自绘标题栏**（不再有经典系统标题栏），绘制走**双缓冲**因此不闪烁；
顶栏右上角的 `— □ ✕` 提供最小化 / 最大化 / 关闭，按住顶栏空白处可拖动窗口。

已具备：**圆角窗口与圆角控件、状态过渡动画、自定义字体、可配置磨玻璃、首次启动引导、
六页业务界面、严格的状态机（启动器 / 账户 / 版本 / 下载）、全局 Toast 与危险操作二次确认、
快捷键（Ctrl 1..6 / F5 / Esc）、拖拽安装、系统托盘**。

---

## 1. 交付物

| 文件 | 说明 |
|---|---|
| `dist/MinecraftLauncher.exe` | **可直接运行的可执行文件**（1,456,980 字节，无需安装，无额外 DLL） |
| `dist/README.md` | 本文件 |
| `dist/screenshots/ui-{home,versions,accounts,downloads,settings,logs}-new.png` | 六页离屏截图（软件后端，1100×720） |
| `dist/screenshots/ui-onboarding-new.png` | 首次启动引导页（软件后端） |
| `dist/screenshots/ui-glass-new.png` | 磨玻璃 + 大圆角效果图（软件后端） |
| `dist/screenshots/ui-win32-shot.png` | **真实 Win32 窗口**自检截图（中文正常、自绘标题栏、圆角控件） |

### 直接运行

双击 `MinecraftLauncher.exe` 即可打开窗口。命令行参数分两组：

```bash
# 渲染 / 验证
MinecraftLauncher.exe                     # 打开图形窗口
MinecraftLauncher.exe --first-run         # 强制首次启动引导（即使已有配置）
MinecraftLauncher.exe --win-selftest      # 真实窗口渲染 8 帧 → 截图 → 自动关闭
MinecraftLauncher.exe --shot home         # 软件后端渲染首页 → PNG + PPM
MinecraftLauncher.exe --shot versions     # 版本管理页
MinecraftLauncher.exe --shot accounts     # 账户管理页
MinecraftLauncher.exe --shot downloads    # 下载中心页
MinecraftLauncher.exe --shot settings     # 设置页
MinecraftLauncher.exe --shot logs         # 日志页
MinecraftLauncher.exe --out <路径>        # 指定截图输出路径

# 外观（也可在「设置」页实时调整；改完下一帧生效）
MinecraftLauncher.exe --font "Microsoft YaHei UI"   # 换系统字体
MinecraftLauncher.exe --font-file C:\fonts\x.ttf --font "X"  # 载入私有 .ttf/.otf
MinecraftLauncher.exe --bitmap-font glyphs.txt       # 软件后端自定义 5×7 点阵字形表
MinecraftLauncher.exe --font-scale 1.2              # 全局字号缩放 0.75..1.5
MinecraftLauncher.exe --glass --blur 18             # 开磨玻璃 + 模糊半径 0..48
MinecraftLauncher.exe --no-glass                   # 关磨玻璃
MinecraftLauncher.exe --radius 14                  # 控件圆角 0..32
MinecraftLauncher.exe --window-radius 24           # 窗口圆角 0..64
MinecraftLauncher.exe --no-anim                    # 关闭所有过渡动画
MinecraftLauncher.exe --light                      # 亮色主题（默认暗色）
```

### 依赖说明

静态依赖仍只有 **Windows 系统自带 DLL**：

```
GDI32.dll  USER32.dll  KERNEL32.dll  msvcrt.dll  ntdll.dll  bcryptprimitives.dll
```

DWM / `SetWindowCompositionAttribute` 这类**可选 API 是运行时 `LoadLibraryW` +
`GetProcAddress` 动态取地址**的：系统不支持时静默降级，不产生静态链接依赖，也不会让程序启动失败。

---

## 2. 界面与交互总览

### 2.1 页面一览

| 页面 | 内容 | 关键交互 |
|---|---|---|
| 首页 Home | 版本 / 账户 / Java / 内存四张卡片（随当前版本 + 当前账户联动）；启动按钮（置灰 / 检查→下载→验证→启动 四阶段进度） | 前往管理、启动游戏 |
| 版本管理 Versions | 列表（加载 / 空 / 错误态）、安装表单、删除二次确认、详情面板 | 设为当前、详情、删除、搜索过滤、滚动 |
| 账户管理 Accounts | 微软授权 / 离线添加、切换、删除（危险确认）、列表 | 登录中状态机、失败重试 |
| 下载中心 Downloads | 资源分类（版本 / Mod / 光影 / 整合包）、详情、任务面板（等待→下载⇄暂停→完成/失败→重试）、下载设置（并发数 / 速度上限 / 代理） | 新建任务、暂停 / 继续、删除、重试 |
| 设置 Settings | 游戏 / 下载 / 启动器 / 账户隐私 / 关于 五个分页；即时保存、保存并恢复默认 | 所有开关 / 输入框即时落盘 |
| 日志 Logs | 日志过滤（全部 / INFO / WARN / ERROR）、清空、崩溃后自动跳转 | 滚动、筛选 |

**任意时刻仅有一个当前版本与一个当前账户**，所有页面随二者联动：未选版本或未登录时，
首页启动按钮置灰并提示对应文案。

### 2.2 首次启动引导（Onboarding）

仅在**首次启动**（无配置文件）出现，也可在「设置 → 关于」里重入。共四步：

1. 欢迎页
2. Java 扫描与手动指定（`--font-file` 风格载入 `java.exe`）
3. 游戏目录导入
4. 配置汇总

引导期间 `LauncherPhase::Onboarding` 为全屏独占页面；完成后写配置并进入 `Idle`。

### 2.3 启动流程（状态机）

```
LauncherPhase: Onboarding → Idle → Launching → Running → Idle
AccountFlow:   未登录 → 登录中(SigningIn) → 已登录(SignedIn) / 失败(Failed)
VersionFlow:   未安装(NotInstalled) → 安装中(Installing) → 已安装(Installed) → 启动中(Launching) → 运行中(Running)
DownloadState: 等待(Waiting) → 下载中(Downloading) ⇄ 暂停(Paused) → 完成(Completed)/失败(Failed) → 重试
```

- 启动前置校验：未选版本 → 置灰并提示「请选择版本」；未登录 → 提示「请先登录账户」。
- 阶段进度：检查运行环境 → 下载缺失资源 → 验证文件完整性 → 启动游戏。
- 启动后按设置最小化到托盘或退出；异常退出写日志并提示（崩溃后日志页自动置顶）。

### 2.4 全局反馈

- **Toast**：时间驱动淡入淡出，成功 / 信息 / 错误三态配色。
- **危险操作警示色确认**：删除版本 / 删除账户使用 `danger_confirm`，确认按钮为警示红，
  文案明确「不可恢复」。
- **加载 / 空 / 错误态**：列表区统一三种占位块（`loading_block` / `empty_block` / `error_block`），
  错误块提供「重试」按钮。
- **快捷键**：`Ctrl+1..6` 切页、`Ctrl+L` 日志、`F5` 刷新、`Esc` 关闭浮层 / 弹窗。
- **拖拽安装**：把整合包或 `.jar` 拖到窗口触发安装（Win32 后端 `WM_DROPFILES`）。
- **系统托盘**：最小化 / 关闭时可缩到托盘（`Shell_NotifyIconW`），点击托盘图标恢复。
- **更新提示**：顶部 banner 提示新版本，可关闭（一次性）。
- **日志入口**：底部状态栏常驻「日志」入口，崩溃后自动跳转日志页。

> 软件后端截图没有 CJK 字体，中文显示为方块占位 —— 这是**预期行为**，仅用于验证布局与配色；
> **真实窗口中中文完全正常**，见 `screenshots/ui-win32-shot.png`。
> 软件后端的 `— □ ✕` 已内置 5×7 点阵，不会退化成空心方块；PNG/PPM 导出时透明窗口圆角用桌面底色填充，
> 因此**窗口圆角在软件截图里不明显**（真实窗口由 `CreateRoundRectRgn` + `SetWindowRgn` 裁形）。

---

## 2.5 交互逻辑（对标 PCL2）

交互层参照 PCL 的 `MyButton.xaml.vb` / `MyListItem.xaml.vb` / `MyPageRight.vb`。

| PCL 的做法 | 本实现的对应 |
|---|---|
| `IsMouseDown` 字段，MouseDown 置位 | `PressTracker`（跨帧持久） |
| `MouseUp` 时检查 `IsMouseDown` 才 `RaiseEvent Click` | `PressTracker::end(id)` + 抬起必须在同一控件内 |
| `MouseLeave` 时若仍按下 → 取消按下态 | `detect_click` 里鼠标移出即 `press.cancel(id)` |
| `MouseDown` 时调用 `Focus()` | 点击按钮 → `focus_ring.set_focus()` + `request_focus()` |
| 控件有独立 `Uuid`，动画可被 `AniStop` 打断 | `WidgetId::derive`（FNV-1a，跨帧稳定） |
| 页面契约 `PageOnEnter` / `PageOnExit` | `Page` 枚举 + 滑入/淡入动画 |

### 关键修正：拖出不算点击

按钮内按下、拖出、再松开**不**触发点击；抬起落在别处算这次按下结束但不触发。
`launcher-ui/tests/interaction.rs` 逐条锁定（含「三个控件 + 跨两帧」端到端）。

### 右键菜单结果跨帧

菜单命中测试发生在 `Ui::finish()`，结果写入 `FrameState::menu_action`（`Rc<Cell<Option<usize>>>`）
跨帧槽，业务代码在下一帧开头用 `ui.take_menu_click()` 取走。下标**包含分隔线**占位。

### 抬起事件不能被第一个控件吞掉

曾有一个让**整个界面点不动**的缺陷：`detect_click` 对 `MouseUp` 无条件标记已消费，
于是第一个控件（顶栏按钮）吃掉所有抬起事件。`interaction.rs` 的
`mouse_up_reaches_the_widget_that_was_pressed` 用「三控件 + 跨两帧」事件序列锁死。

### 无边框窗口的标题栏与拖动

`WS_POPUP` + `WS_EX_APPWINDOW` 去掉系统标题栏（经典样式看起来像 XP），顶栏即标题栏。
圆角由 `CreateRoundRectRgn` 基于 `GetWindowRect` 裁出，最大化时自动取消。

| 操作 | 路径 |
|---|---|
| 最小化 / 最大化 / 关闭 | 顶栏 `— □ ✕` → `Ui::request_window_action` → `FrameState` 共享槽 → 主循环 → `ShowWindow` / `PostMessageW(WM_CLOSE)` |
| 拖动窗口 | 按住顶栏空白处 → `WindowAction::Drag` → `SendMessageW(WM_NCLBUTTONDOWN, HTCAPTION)` |
| 键盘输入 | `WM_LBUTTONDOWN` 时 `SetFocus(hwnd)` |
| 鼠标离开窗口 | `TrackMouseEvent(TME_LEAVE)` → `Event::MouseLeave` 清按下态 / hover |

### 离屏截图的页面动画冻结

`--shot` 用 `dt = 0` 渲染一帧，而 `set_page` 会把入场动画归零（动画开启时），导致内容区被裁成
**0 宽**而整页消失。`App::freeze_page_animation()` 在渲染前把入场动画置为完成态，截图得到完整成品。

---

## 3. 架构

```
launcher-ui          业务界面（引导 + 六页 + 状态机 + 全局反馈）  ← 不认识 Win32
        ↑
ui-core              即时模式 UI 内核（布局/控件/事件/绘制指令/外观配置）  零依赖
        ↑
ui-backend-soft      软件光栅后端（纯内存，可测试/可截图）
ui-backend-win32     Win32 GDI 后端（真实窗口，手写 FFI）
```

### 为什么是即时模式

保留模式需要持久控件树 + 差量更新，代码量约 3 倍，且"哪块该重绘"极易出错。
启动器只有几百个控件，每帧全量重绘完全可接受。

### 绘制指令集（9 种）

`FillRect` / `Text` / `Line` / `StrokeRect` / `PushClip` / `PopClip`，
加上本版新增的 `FillRoundRect` / `StrokeRoundRect` / `BlurRegion`。

半径由 `clamp_radius` 夹到 `min(w,h)/2`，是绘制内容的一部分（`DrawList::same_content` 会比较），
改圆角不会被"内容没变就不重绘"的优化吃掉。

### 外观配置的分层

`StyleConfig { font, glass, window_radius, control_radius, animations }` 放在 `ui-core`，表达**意图**；
`Backend::configure(&StyleConfig)` 每帧由 `Ui::begin_frame` 调用，后端自比前后配置、变了才重建平台资源：

- 软件后端：像素级圆角 / 两趟积分图 box blur / 自定义点阵字形表 / 字号缩放 / 透明窗口角；
- Win32 后端：`RoundRect` / `CreateRoundRectRgn` + `SetWindowRgn` / DWM 圆角偏好 /
  Acrylic-Mica backdrop / `AddFontResourceExW(FR_PRIVATE)` 私有字体 / `CreateFontW` 自定义字体族。

### 主题必须每帧从 App 读取

`App::theme` 是唯一真相源。Windows 消息循环每帧 `let theme = app.theme().clone()` 并同步
`set_background(theme.bg.0)`，运行时「设置」改的配置才会生效（明暗切换、圆角、字体等）。

### 双缓冲与重绘调度

`WM_PAINT` 整帧先画进 `CreateCompatibleDC` + `CreateCompatibleBitmap`，最后一次 `BitBlt` 上屏；
`WM_ERASEBKGND` 返回 1 阻止系统白色画刷先擦一遍，窗口类也不注册背景画刷。

主循环事件驱动（有输入 / 动画心跳才出帧，`DrawList::same_content` 跳过无意义重绘），
但每帧标脏后立刻 `UpdateWindow`，避免动画心跳被 `WM_PAINT` 挤到队尾造成跳帧抖动。

---

## 4. 关键技术决策

### 4.1 手写 Win32 FFI，不用 `windows-sys`

本机 `windows-sys` 编译失败（需要 `dlltool` 生成 import library 而本机 `dlltool` 有缺陷）。
改用 `#[link]` + `extern "system"` 直接声明所需函数；未文档化的 DWM / composition API 走动态取地址。

### 4.2 软件后端用内置 5×7 点阵 + 可替换字形表

引入 `ab_glyph`/`fontdue` 与"自制 UI"前提冲突。软件后端只服务布局验证：内置 ASCII 点阵，
另支持 `--bitmap-font` 载入自定义 5×7 字形表。真实字体由 Win32 后端负责。

### 4.3 `unsafe` 面积被限制在一个文件

- `ui-core` / `ui-backend-soft` / `launcher-ui`：`#![forbid(unsafe_code)]`
- `ui-backend-win32/src/imp.rs`：唯一允许 `unsafe` 的文件

### 4.4 布局用 `i32` 而非 `f32`

像素是整数，浮点除法会在边界出现差一像素的缝隙/重叠。

### 4.5 ID 用 FNV-1a 手写哈希

标准库不保证 `DefaultHasher` 跨运行稳定；ID 不稳定会让上一帧 hover 的按钮下一帧变成别的控件。

### 4.6 动画心跳按需开关

事件驱动的消息循环在没有输入时会睡着，动画会卡在中间态。后端提供 `set_animation_active(bool)`：
有动画时开 16ms `WM_TIMER` 发 `Event::Tick`，动画结束后留 20 帧宽限再关定时器回到 0% CPU。
关掉 `--no-anim` 后心跳完全不启动。

### 4.7 一帧延迟是即时模式的正常语义

`App::render` 里改 `Theme::style`，本帧 `Ui` 持有的是帧首传入的主题引用，新配置从**下一帧**开始生效。

---

## 5. 开发过程中修掉的真实 bug

| bug | 现象 | 根因 |
|---|---|---|
| 数字点阵写错 | `1.20.1` → `l.70.1` | `1`/`0`/`7` 点阵位错 |
| 字母 A 最后一行写错 | `JAR` → `J0R` | A 底行收口写成 `01110`，应为两条腿 `10001` |
| FNV 素数写错 | ID 分布变差 | 正确是 `0x100000001b3` |
| **窗口持续闪烁（两层）** | CPU 100% / 肉眼看到「清屏→重画」 | 重绘自激循环 + GDI 直绘无双缓冲 + 光标动画轨道永不停 |
| **所有点击失效** | 除第一个按钮外点什么都没反应 | `detect_click` 对 `MouseUp` 无条件标记已消费 |
| **XP 风系统标题栏** | 窗口顶上一条经典样式标题栏 | `WS_OVERLAPPEDWINDOW` + `SetWindowRgn` 让 DWM 回退经典装饰 |
| **运行时设置无效** | 改圆角/字体/主题没反应 | 消息循环捕获了过期的 `Theme` 克隆 |
| **离屏截图整页空白** | 非首页只有一个空壳（38 条指令） | `--shot` 的 `dt=0` + 入场动画归零导致内容裁成 0 宽；`freeze_page_animation` 修复 |

### 闪烁的根因：重绘自激循环 + 无双缓冲

初版每处理一条消息都 `InvalidateRect`，与 `WM_PAINT` 自激；且 `WM_PAINT` 直接把背景与几十条 GDI
指令依次画到屏幕 DC，`DefWindowProc` 还会用系统白画刷先擦背景。修复为：事件驱动 + `same_content`
跳过、内存 DC 双缓冲 + 单次 `BitBlt`、`WM_ERASEBKGND` 返回 1、窗口类不注册背景画刷、
`text_input` 失焦 `anim_stop(Slot::Caret)` 让心跳正常收尾。

### 离屏截图整页空白

`--shot` 用 `dt=0` 且 `set_page` 把 `page_anim_ms` 归零，入场动画 clip 宽度 = `content.w * 0 = 0`，
整页被裁掉。新增 `App::freeze_page_animation()` 在渲染前把动画置为完成态，截图得到完整成品。

---

## 6. 测试与静态检查

```
cargo fmt --all -- --check              通过
cargo clippy --workspace --all-targets 零警告（代码层面）
cargo test --workspace                 165 passed / 1 ignored
cargo build -p launcher-ui --release   通过
MinecraftLauncher.exe --win-selftest   真实窗口渲染 8 帧 + 截图，通过
```

| crate | 测试数 | 覆盖 |
|---|---|---|
| `ui-core` | 89 | 颜色/矩形/裁剪栈/ID/主题、圆角与模糊指令、动画、菜单、滚动、焦点 |
| `ui-backend-soft` | 16 | 像素填充/裁剪、alpha、字形、圆角像素/窗口透明角/box blur/自定义字形表/标题栏图标点阵 |
| `ui-backend-win32` | 0（FFI 在 Windows 目标下条件编译） | 真实窗口由 `--win-selftest` 验证 |
| `launcher-ui` | 60（含 24 条 `flow.rs` 状态机） | PCL 交互范式、hover 回归、多控件抬起分派/窗口动作/真实布局端到端点击、状态机四态转换（启动器/账户/版本/下载）、二次确认、托盘恢复、快捷键 |

> 构建时偶发 `warning: error deleting lock file ... (os error 5)`，是本机增量编译目录的文件锁问题，
> 与代码无关，不影响产物。

---

## 7. 已知限制（诚实记录）

| 限制 | 影响 | 说明 |
|---|---|---|
| **UI 已接入插件工具（可选演示）** | 默认走真实插件（`LiveDataSource`），`--demo` 回退纯内存数据 | 实例增删改查/启动均落到 `official-*` 插件；演示数据形状与真实返回一致 |
| **启动游戏：真实 spawn（默认探测）** | `launch_game` 真实拉起 Java 子进程并返回真实 PID，UI 轮询真实退出码 | 默认入口 `LaunchEntry::None` 只跑 `java -version` 探测；指向真实 jar/主类即进游戏。`--demo` 下仍为内存模拟 |
| **Win32 的 `BlurRegion` 是近似** | GDI 面板只有半透明 tint | 真实像素模糊在软件后端；系统级 Acrylic/Mica 依赖 DWM，取不到 API 时静默降级 |
| **软件后端无 CJK 点阵** | 软件截图中文是方块 | 预期行为；真实窗口中文正常。标题栏 `— □ ✕` 已内置点阵 |
| **软件截图的窗口圆角不明显** | PNG/PPM 里透明角被桌面底色填充 | 真实窗口由 `SetWindowRgn` 裁形 |
| **无边框窗口不能拖拽边缘缩放** | 只能最大化 / 还原 | `WS_POPUP` 不带 `WS_THICKFRAME`；需额外 `WM_NCHITTEST` 边缘命中 |
| **在线资源未接网络** | 下载中心只有本地模拟任务 | 需要接 `host-http` / `official-mod` 等工具 |
| **外观/引导配置不持久化** | 重启后回到默认（除首启动写一份默认配置） | 需要配置文件读写 |
| **无高 DPI 精确适配** | 125%/150% 下字号可能偏小 | 按屏幕宽度粗估，未用 `GetDpiForWindow` / `WM_DPICHANGED` |
| **PNG 用 stored deflate** | 1100×720 截图约 2.37 MB | 零第三方依赖的取舍（约 60 行手写编码器） |

---

## 8. 从源码构建

```bash
cd ui
cargo test --workspace
cargo build -p launcher-ui --release
# 产物：ui/target/release/launcher-ui.exe → 复制到 dist/MinecraftLauncher.exe
```

需要 Rust 1.85+。本机使用 GNU toolchain（`stable-x86_64-pc-windows-gnu`）。

---

## 9. 下一步建议

1. ~~**接插件**~~（已完成）：`launcher-ui` 新增 `LiveDataSource`，进程内 `tokio` 桥接调用
   `official-instance` / `official-mod` / `official-launch`，结果映射进 `LauncherModel`。
2. ~~**真实启动**~~（已完成）：`launch_game` 真实 spawn Java 子进程并返回真实 PID，UI 轮询真实退出码收尾。
   剩余：把子进程 stdout/stderr 桥接到日志页；提供 jar / 主类选择器以配置 `LaunchEntry`。
3. ~~**插件发现 / 管理**~~（已完成）：UI 新增「插件」页——扫描插件目录的 `plugin.json`
   （`plugins_discovery.rs`），合并内嵌内置清单展示；支持安装（复制目录进插件目录后重扫）、
   卸载非内置插件。启动器界面自身也以 `com.example.launcher-ui` 清单作为被管理插件出现。
   注意单进程单二进制的现实约束：非内置插件安装后展示为「需重新编译启动器才能加载」。
4. ~~**Java 自动探测**~~（已完成）：`official-launch` 新增 `detect_java` 工具与 `java.rs`
   （扫 PATH / JAVA_HOME / 常见安装目录 / `.minecraft\runtime`，解析 `java -version`）；
   设置页与首次引导的 Java 扫描均改为真实探测，本机实测发现 2 个运行时（Microsoft 25.0.1、
   Oracle 1.8.0_51，均在 `.minecraft\runtime`）。
5. **高 DPI**：改用 `GetDpiForWindow` + `WM_DPICHANGED`。
6. **配置持久化**：把 `Settings` 与引导状态序列化到用户配置目录。
7. **托盘菜单**：托盘图标右键菜单（显示 / 退出 / 快速启动）。
8. **外观字体文件选择器**：目前 `--font-file` / `--bitmap-font` 只能走命令行路径。
9. **动态插件加载**：把「需重新编译」的提示升级为真正的 out-of-process 沙箱宿主
   （每插件一个子进程 + IPC），才能热加载第三方插件。
