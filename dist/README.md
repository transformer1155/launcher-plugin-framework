# 自研 UI 系统 —— 交付说明

一个**零第三方 GUI 依赖**的 Minecraft 启动器界面，含即时模式 UI 内核与两个渲染后端。

当前版本已具备：**圆角窗口与圆角控件、状态过渡动画、自定义字体、可配置磨玻璃、
完整列表交互（多选 / 右键菜单 / 滚动 / 搜索 / 表单）**，以及顶栏的「外观设置」面板。

窗口采用**无边框自绘标题栏**（不再有经典系统标题栏），绘制走**双缓冲**因此不闪烁；
顶栏右上角的 `— □ ✕` 提供最小化 / 最大化 / 关闭，按住顶栏空白处可拖动窗口。

---

## 1. 交付物

| 文件 | 说明 |
|---|---|
| `dist/MinecraftLauncher.exe` | **可直接运行的可执行文件**（949,402 字节，无需安装，无额外 DLL） |
| `dist/README.md` | 本文件 |
| `dist/screenshots/ui-*-new.png` | 三页新截图 + 磨玻璃效果图（软件后端，1100×720） |
| `dist/screenshots/ui-win32-shot.png` | **真实 Win32 窗口**自检截图（中文正常、自绘标题栏、圆角控件） |
| `dist/screenshots/*.png`（旧） | 上一版截图，保留作对照 |

### 直接运行

双击 `MinecraftLauncher.exe` 即可打开窗口。命令行参数分两组：

```bash
# 渲染 / 验证
MinecraftLauncher.exe                     # 打开图形窗口
MinecraftLauncher.exe --win-selftest      # 真实窗口渲染 8 帧 → 截图 → 自动关闭
MinecraftLauncher.exe --shot instances    # 软件后端渲染实例页 → PNG + PPM
MinecraftLauncher.exe --shot mods         # 渲染 Mod 页
MinecraftLauncher.exe --shot launch       # 渲染启动页
MinecraftLauncher.exe --soft              # 软件后端渲染首页
MinecraftLauncher.exe --out <路径>        # 指定截图输出路径

# 外观（也可在窗口里的「外观设置」面板实时调整）
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

## 2. 三页界面与交互

### 实例页

实例列表（名称 / 版本 / 加载器 / 上次游玩）、**新建实例**（名称 / 版本 / 加载器表单）、
**删除**（二次确认对话框，支持批量）、**全选**、**Ctrl 单项切换 / Shift 范围选择**、
**右键菜单**（设为当前 / 新建 / 删除）、**列表滚动**与滚动条。

### Mod 页

**实时本地过滤**的搜索框、已安装 Mod 列表、安装 Mod 表单、逐行移除按钮、右键菜单、列表滚动。
安装与移除会真正修改本地 `DataStore`，不是只弹提示。

### 启动页

内存上限 / JVM 参数 / 游戏参数都是**可编辑输入框**，比例条可点击设定内存，
「保存配置」写回 `DataStore`；「启动游戏」写日志（明确标记 `simulated`、`pid=null`，
**不创建任何 OS 进程**）；「查看日志」展开可滚动日志面板。

### 外观设置（顶栏按钮）

磨玻璃开关与模糊强度、窗口圆角、控件圆角、字号百分比、动画开关、明暗主题、字体族输入。
侧栏实时显示当前「圆角 Npx / 磨玻璃 开|关 / 字体 xxx」。
**改完下一帧生效**（即时模式的一帧延迟，见 4.6）。

> 软件后端截图没有 CJK 字体，中文显示为方块占位 —— 这是**预期行为**，仅用于验证布局与配色；
> **真实窗口中中文完全正常**，见 `screenshots/ui-win32-shot.png`。
> PNG/PPM 导出时透明窗口圆角会用桌面底色填充，因此**窗口圆角在软件截图里不明显**；
> 真实窗口由 `CreateRoundRectRgn` + `SetWindowRgn` 裁出圆角，另有 Win11
> `DWMWA_WINDOW_CORNER_PREFERENCE` 兜底。

---

## 2.5 交互逻辑（对标 PCL2）

交互层参照 `E:\cache\PCL2\PCL` 的 `MyButton.xaml.vb` / `MyListItem.xaml.vb` / `MyPageRight.vb`。

| PCL 的做法 | 本实现的对应 |
|---|---|
| `IsMouseDown` 字段，MouseDown 置位 | `PressTracker`（跨帧持久） |
| `MouseUp` 时检查 `IsMouseDown` 才 `RaiseEvent Click` | `PressTracker::end(id)` + 抬起必须在同一控件内 |
| `MouseLeave` 时若仍按下 → 取消按下态 | `detect_click` 里鼠标移出即 `press.cancel(id)` |
| `MouseDown` 时调用 `Focus()` | 点击按钮 → `focus_ring.set_focus()` + `request_focus()` |
| 控件有独立 `Uuid`，动画可被 `AniStop` 打断 | `WidgetId::derive`（FNV-1a，跨帧稳定） |
| 页面契约 `PageOnEnter` / `PageOnExit` | `Page` 枚举 + `App::switch_page` + 滑入/淡入动画 |

### 关键修正：拖出不算点击

初版有个真实缺陷：**按钮上按下、拖出、再松开仍会触发点击**。现在完整语义如下
（`launcher-ui/tests/interaction.rs` 逐条锁定）：

| 场景 | 结果 |
|---|---|
| 按钮内按下 + 按钮内松开 | 触发点击 |
| 按钮内按下 + 按钮外松开 | **不触发**（PCL 范式） |
| 按钮内按下 + 鼠标移出 | 取消按下态 |
| 按住不放（跨多帧） | 保持按下态，松开时仍触发 |
| 禁用按钮 | 完全不响应 |
| 点击按钮 | 获得键盘焦点 |
| 焦点在按钮上按 Enter | 触发点击 |
| 按 Tab | 焦点在可聚焦控件间循环 |

### 两个焦点概念必须统一

初版有 `Ui::focus`（输入框用）与 `FocusRing`（键盘导航用）两个互不同步的焦点。
现在按钮点击会同时写两者，`FocusRing` 是权威；`Ui::finish` 里再按方向键事件调用
`focus_arrow` 做空间导航。

### Tab 的处理时机

Tab 需要"本帧登记了哪些可聚焦控件"才能决定往哪跳，而登记发生在控件绘制时，
所以处理必须**推迟到帧末 `Ui::finish`**。焦点在下一帧生效，这是可接受的。

### 右键菜单结果跨帧

菜单的命中测试发生在 `Ui::finish()`，而 `finish()` 会消耗 `Ui`。因此结果写入
`FrameState::menu_action` 这个 `Rc<Cell<Option<usize>>>` 跨帧槽，业务代码在
**下一帧开头**用 `ui.take_menu_click()` 取走（取完即清空）。

下标**包含分隔线**：实例菜单是 `[设为当前(0), 新建(1), 分隔线(2), 删除(3)]`，
分隔线占位但不可点击。`launcher-ui/tests/appearance.rs` 用 4 帧事件序列把这条契约锁死。

### 原「尚未实现」清单 —— 现已全部补齐

| 能力 | 状态 | 实现要点 |
|---|---|---|
| 方向键导航 | 已实现 | `FocusRing::focus_arrow` 按几何距离找最近控件 |
| 动画过渡 | 已实现 | `AnimationState` + `Easing::OutQuad/OutBack`，三通道颜色 / 缩放 / 透明度 |
| 按压缩放 | 已实现 | `Slot::Scale` 按下时缩到 0.955 |
| 右键菜单 | 已实现 | `ContextMenu` + 圆角浮层 + hover 高亮 + 跨帧结果槽 |
| 列表多选 | 已实现 | `Selection` + `Shift` 范围选择锚点 |
| 光标闪烁 | 已实现 | 两段动画接力（1→0→1） |
| 页面进入动画 | 已实现 | `App::page_anim_ms` 驱动滑入 + 淡入 |
| 列表滚动 / 裁剪区 | 已实现 | `Ui::scroll_area` + `PushClip/PopClip`（Win32 用 `SaveDC` + `IntersectClipRect`） |
| toast 提示 | 已实现 | 时间驱动淡入淡出 |

### 2.6 关键修正：抬起事件不能被第一个控件吞掉

曾有一个让**整个界面点不动**的缺陷：`Ui::detect_click` 对 `MouseUp` 无条件执行
`consumed[i] = true`，于是第一个调用控件检测的按钮（顶栏「外观设置」）会吃掉
所有抬起事件，排在它后面的控件永远收不到点击。**单控件单元测试测不出这个问题**。

现在抬起事件只归**当初按下的那个控件**：先用只读的 `PressTracker::is_pressed(id)`
认领，再 `end(id)` 收尾；抬起落在别处时也算这次按下结束（拖出取消），但不触发点击。
`launcher-ui/tests/interaction.rs` 的 `mouse_up_reaches_the_widget_that_was_pressed`
用「三个控件 + 跨两帧」的事件序列把这条契约锁死。

### 2.7 无边框窗口的标题栏与拖动

`WS_POPUP` + `WS_EX_APPWINDOW` 去掉了系统标题栏（那个经典样式看起来像 XP），顶栏
就是标题栏。圆角由 `CreateRoundRectRgn` 基于 **`GetWindowRect`**（窗口矩形）裁出，
最大化时用 `IsZoomed` 自动取消裁角。

| 操作 | 路径 |
|---|---|
| 最小化 / 最大化 / 关闭 | 顶栏 `— □ ✕` → `Ui::request_window_action` → `FrameState` 共享槽 → 主循环 `perform_window_action` → `ShowWindow` / `PostMessageW(WM_CLOSE)` |
| 拖动窗口 | 按住顶栏空白处 → `Ui::title_bar` 认领未被按钮消费的 `MouseDown` → `WindowAction::Drag` → `ReleaseCapture` + `SendMessageW(WM_NCLBUTTONDOWN, HTCAPTION)` |
| 键盘输入 | `WM_LBUTTONDOWN` 时 `SetFocus(hwnd)`（popup 不会自动获得焦点，否则 `WM_CHAR` 不来） |
| 鼠标离开窗口 | `TrackMouseEvent(TME_LEAVE)` → `Event::MouseLeave` 清掉按下态 / hover |

`WindowAction` 定义在 `ui-core`（只表达意图），执行在 Win32 后端；软件后端 / 离屏渲染
直接忽略，不污染内核。窗口按钮的 `— □ ✕` 在软件后端也内置了 5×7 点阵字形，
截图不会退化成空心方块。

---

## 3. 架构

```
launcher-ui          业务界面（三页 + 外观设置）  ← 不认识 Win32
        ↑
ui-core              即时模式 UI 内核（布局/控件/事件/绘制指令/外观配置）  零依赖
        ↑
ui-backend-soft      软件光栅后端（纯内存，可测试/可截图）
ui-backend-win32     Win32 GDI 后端（真实窗口，手写 FFI）
```

### 为什么是即时模式

保留模式需要持久控件树 + 差量更新，代码量约 3 倍，且"哪块该重绘"极易出错。
启动器只有几百个控件，每帧全量重绘完全可接受。

### 绘制指令集（现在是 9 种）

`FillRect` / `Text` / `Line` / `StrokeRect` / `PushClip` / `PopClip`，
加上本版新增的：

| 指令 | 语义 |
|---|---|
| `FillRoundRect` | 圆角填充，半径由 `clamp_radius` 夹到 `min(w,h)/2` |
| `StrokeRoundRect` | 圆角描边（不能拆成 4 条矩形，否则角是方的） |
| `BlurRegion` | 对区域**已绘制像素**做模糊后处理，再叠 tint |

半径是绘制内容的一部分，`DrawList::same_content` 会比较它 —— 所以改圆角不会被
"内容没变就不重绘"的优化吃掉。

### 外观配置的分层

`StyleConfig { font, glass, window_radius, control_radius, animations }` 放在 `ui-core`，
表达的是**意图**；`Backend::configure(&StyleConfig)` 每帧由 `Ui::begin_frame` 调用，
后端自己比较前后配置、变了才重建平台资源：

- 软件后端：像素级圆角 / 两趟积分图 box blur / 自定义点阵字形表 / 字号缩放 / 透明窗口角；
- Win32 后端：`RoundRect` / `CreateRoundRectRgn` + `SetWindowRgn` / DWM 圆角偏好 /
  Acrylic-Mica backdrop / `AddFontResourceExW(FR_PRIVATE)` 私有字体 / `CreateFontW` 自定义字体族。

### 文本测量的缓存

两个后端都缓存 `text_width`（软件后端 `RefCell<HashMap>`，Win32 字体句柄 + 测量缓存）。
**字体配置变化时必须清缓存**，否则换了字号仍用旧宽度排版。

### 主题必须每帧从 App 读取

`App::theme` 是唯一真相源。Windows 消息循环若在闭包外捕获一份 `Theme` 克隆，
运行时「外观设置」改的配置就永远传不到 `Backend::configure` 和控件。
现在 `run_loop` / `run_selftest` 每帧 `let theme = app.theme().clone()`，并同步
`set_background(theme.bg.0)`，明暗切换才真正生效。

### 双缓冲与重绘调度

`WM_PAINT` 不再直接往屏幕 DC 上「清屏 → 逐条 GDI 绘制」：整帧先画进
`CreateCompatibleDC` + `CreateCompatibleBitmap`，最后一次 `BitBlt` 上屏；
`WM_ERASEBKGND` 返回 1 阻止系统白色画刷先擦一遍，窗口类也不再注册背景画刷。

主循环仍是事件驱动（有输入 / 动画心跳才出帧，`DrawList::same_content` 跳过无意义重绘），
但每帧标脏后立刻 `UpdateWindow`：动画心跳每 16ms 一条消息，若只 `InvalidateRect`，
`WM_PAINT` 会被不断挤到队尾，表现为跳帧式抖动。双缓冲保证这次同步绘制
只是一次 BitBlt，用户看不到中间过程。

---

## 4. 关键技术决策

### 4.1 手写 Win32 FFI，不用 `windows-sys`

本机 `windows-sys` 编译失败（需要 `dlltool` 生成 import library，而本机 rustup 自带的
`dlltool` 有 `CreateProcess` 缺陷）。改用 `#[link]` + `extern "system"` 直接声明所需函数。

未文档化的 DWM / composition API 走动态取地址，避免 GNU import library 链接失败。

### 4.2 软件后端用内置 5×7 点阵 + 可替换字形表

引入 `ab_glyph`/`fontdue` 与"自制 UI"前提冲突。软件后端只服务布局验证：
内置 ASCII 点阵，另支持 `--bitmap-font` 载入自定义 5×7 字形表
（`A 0e 11 11 1f 11 11 11` 或 `U+0041 ...`，支持 `0x` / `0b` / 逗号分隔）。
真实字体由 Win32 后端负责。

### 4.3 `unsafe` 面积被限制在一个文件

- `ui-core` / `ui-backend-soft` / `launcher-ui`：`#![forbid(unsafe_code)]`
- `ui-backend-win32/src/imp.rs`：唯一允许 `unsafe` 的文件

### 4.4 布局用 `i32` 而非 `f32`

像素是整数，浮点除法会在边界出现差一像素的缝隙/重叠。

### 4.5 ID 用 FNV-1a 手写哈希

标准库不保证 `DefaultHasher` 跨运行稳定；ID 不稳定会让上一帧 hover 的按钮下一帧变成别的控件。

### 4.6 动画心跳按需开关

事件驱动的消息循环在没有输入时会睡着，动画会卡在中间态。后端提供
`set_animation_active(bool)`：有动画时开 16ms `WM_TIMER` 发 `Event::Tick`，
动画结束后留 20 帧宽限（约 320ms）确保最后一帧落到终点，再关定时器回到 0% CPU。
关掉 `--no-anim` 后 `App::animating()` 恒为 false，心跳完全不启动。

### 4.7 一帧延迟是即时模式的正常语义

`App::render` 里改 `Theme::style`，本帧 `Ui` 持有的是帧首传入的主题引用，
新配置从**下一帧**开始作用于绘制与 `Backend::configure`。这比在帧中间重建平台资源安全得多。

---

## 5. 开发过程中修掉的真实 bug

| bug | 现象 | 根因 |
|---|---|---|
| 数字点阵写错 | `1.20.1` → `l.70.1` | `1`/`0`/`7` 点阵位错 |
| 字母 A 最后一行写错 | `JAR` → `J0R` | A 底行收口写成 `01110`，应为两条腿 `10001` |
| FNV 素数写错 | ID 分布变差 | 分组错位，正确是 `0x100000001b3` |
| 排序比较反向 | "最近游玩"跑到最后 | `sort_by` 分支写反 |
| **窗口持续闪烁（第一层）** | 不停闪，CPU 100% | 重绘自激循环，见 5.1 |
| **窗口持续闪烁（第二层）** | 动画时肉眼可见「清屏 → 重画」 | GDI 直绘无双缓冲 + 系统擦背景 + 光标动画轨道永不停，见 5.1 |
| **所有点击失效** | 除第一个按钮外点什么都没反应 | `detect_click` 对 `MouseUp` 无条件标记已消费，见 2.6 |
| **XP 风系统标题栏** | 窗口顶上一条经典样式标题栏 | `WS_OVERLAPPEDWINDOW` + `SetWindowRgn` 让 DWM 回退经典装饰，见 2.7 |
| **运行时外观设置无效** | 改圆角/字体/主题没反应 | 消息循环捕获了过期的 `Theme` 克隆，见第 3 节 |
| **右键菜单点了没动作** | 菜单能开，点击无效果 | `finish()` 后 `Ui` 被消耗，命中结果无处可取；改为跨帧槽 |
| **滚轮几乎不动** | 一个 notch 只滚 1px | 后端已把 120 归一化成 ±1，却当成像素用；改为 48px/notch |
| **圆角半径过大导致畸形** | 窄控件被削成椭圆 | `clamp_radius` 夹到短边一半 |

### 5.1 闪烁的根因：重绘自激循环

初版 `run_loop` 每处理一条消息都调用 `InvalidateRect`，而 `WM_PAINT` 本身也是消息，
于是形成自激循环。修复为**事件驱动**：有输入 / 尺寸变化才重绘，纯 `WM_PAINT` 不生成新帧；
`WM_PAINT` 里保留（而非消费）绘制数据；再加 `DrawList::same_content` 比较跳过无意义重绘。

| 场景 | 修复前 | 修复后 |
|---|---|---|
| 完全静止 | ~100% | **0%** |
| 鼠标在按钮上移动 | ~100% | **0.66%** |
| 鼠标在空白区移动 | ~100% | **1.97%** |

**闪烁其实有两层，修完第一层后第二层仍在**：

| 层 | 现象 | 根因 | 修复 |
|---|---|---|---|
| 1 | 永不停歇重绘、CPU 100% | 每消息 `InvalidateRect` 与 `WM_PAINT` 自激 | 事件驱动 + `same_content` |
| 2 | 交互 / 动画时肉眼看到「清屏 → 一条条画出来」 | `WM_PAINT` 直接把背景与几十条 GDI 指令依次画到**屏幕 DC**；`DefWindowProc` 还会用系统白画刷处理 `WM_ERASEBKGND` 先擦一遍 | 内存 DC 双缓冲 + 单次 `BitBlt`；`WM_ERASEBKGND` 返回 1；窗口类不注册背景画刷 |

还有一条放大闪烁的隐藏因素：输入框光标是「两段接力」的循环动画，失焦后轨道没停，
`AnimationState::any_animating()` 永远为 true → 16ms 心跳永不关闭 → 没有动画需求时
仍每帧全屏重绘。现在 `text_input` 失焦会 `anim_stop(id, Slot::Caret)`，心跳能正常收尾。

### 5.2 为什么 hover 反馈没被优化吃掉

`same_content` 有个隐含前提：**hover 状态必须改变绘制内容**。
`launcher-ui/tests/hover_test.rs` 把这个前提变成显式断言（hover 改变内容、
相同输入内容相同、鼠标在按钮外内容不变）。

---

## 6. 测试与静态检查

```
cargo fmt --all -- --check              通过
cargo clippy --workspace --all-targets 零警告（代码层面）
cargo test --workspace                 145 passed / 1 ignored
cargo build -p launcher-ui --release   通过
MinecraftLauncher.exe --win-selftest   真实窗口渲染 8 帧 + 截图，通过
```

| crate | 测试数 | 覆盖 |
|---|---|---|
| `ui-core` | 90（含 1 ignored） | 颜色/矩形/裁剪栈/ID/主题、圆角与模糊指令、动画、菜单、滚动、焦点 |
| `ui-backend-soft` | 16 | 像素填充/裁剪、alpha、字形、**圆角像素/窗口透明角/box blur/自定义字形表/标题栏图标点阵** |
| `ui-backend-win32` | 0（FFI 类型/事件映射在 Windows 目标下为条件编译） | 真实窗口由 `--win-selftest` 验证 |
| `launcher-ui` | 40 | PCL 交互范式、hover 回归、外观配置/菜单跨帧/滚动/字体缩放、PNG 编码、**多控件抬起分派/窗口动作/真实布局端到端点击** |

> 构建时偶发 `warning: error deleting lock file for incremental compilation session directory
> ... 拒绝访问 (os error 5)`，是本机增量编译目录的文件锁问题，与代码无关，不影响产物。

---

## 7. 已知限制（诚实记录）

| 限制 | 影响 | 说明 |
|---|---|---|
| **UI 未接插件工具** | 操作只改本地 `DataStore`，不真正执行游戏/下载 | 接入签名与装配管线后换 `LiveDataSource` 即可；Demo 数据结构与真实返回一致 |
| **启动游戏是 simulated** | 不创建 OS 进程 | 日志明确写 `pid=null`，避免误导 |
| **Win32 的 `BlurRegion` 是近似** | GDI 面板只有半透明 tint | 真实像素模糊在软件后端；系统级 Acrylic/Mica 依赖 DWM，取不到 API 时静默降级 |
| **软件后端无 CJK 点阵** | 软件截图中文是方块 | 预期行为；真实窗口中文正常。标题栏 `— □ ✕` 已内置点阵，不会变方块 |
| **软件截图的窗口圆角不明显** | PNG/PPM 里透明角被桌面底色填充 | 真实窗口由 `SetWindowRgn` 裁形 |
| **无边框窗口不能拖拽边缘缩放** | 只能通过最大化 / 还原改变尺寸 | `WS_POPUP` 不带 `WS_THICKFRAME`；若需自由缩放须额外处理 `WM_NCHITTEST` 的边缘命中 |
| **在线 Mod 搜索未接网络** | 搜索框只做本地过滤 | 需要接 `host-http` / `official-mod` 工具 |
| **外观设置不持久化** | 重启后回到默认 | 需要配置文件读写 |
| **无高 DPI 精确适配** | 125%/150% 下字号可能偏小 | 按屏幕宽度粗估，未用 `GetDpiForWindow` / `WM_DPICHANGED` |
| **PNG 用 stored deflate** | 1100×720 截图约 2.37 MB | 零第三方依赖的取舍，换取约 60 行手写编码器 |

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

1. **接插件**：`launcher-ui` 加 `LiveDataSource`，用 tokio 后台线程调
   `official-instance` / `official-mod` / `official-launch`，把结果写进 `DataStore` 快照。
2. **高 DPI**：改用 `GetDpiForWindow` + `WM_DPICHANGED`。
3. **外观持久化**：把 `StyleConfig` 序列化到用户配置目录，启动时读回。
4. **Win32 真实像素模糊**：若需要面板级真磨玻璃，可换 DirectComposition / 离屏 DIB + blur。
5. **配置字体文件选择器**：目前 `--font-file` / `--bitmap-font` 只能走命令行路径。
