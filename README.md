# Minecraft 启动器 · 单进程插件框架

单 OS 进程、无子进程、无 HTTP / socket / 管道。插件间通信全部是宿主进程内的异步函数调用。

> ⚠️ **先读 [`docs/security-boundary.md`](docs/security-boundary.md)**。
> 这套架构**不是**安全沙箱：可信插件与宿主同等权限，沙箱插件只是逻辑隔离。

> **精确化（重要）**：上面这句约束的落点是「**插件之间**怎么通信」以及框架 `src/` 自身零网络栈，
> 不是「整个进程永不允许出站连接」。宿主侧独立 crate [`host-http/`](host-http/README.md)
> 是唯一允许出站 TCP/TLS 的地方（Modrinth 搜索），它不产生额外 OS 进程，插件也拿不到 `std::net`，
> 只能通过 `ctx.http()` 这个受控端口访问。裁决理由见 [`host-http/README.md`](host-http/README.md) §1。

---

## 1. 这个仓库现在有什么

> 📌 本文件早期版本写的是「只有插件化底层框架，**没有** Mod 管理 / 游戏启动 / UI」。
> 那已经是**过时描述**：业务插件、宿主网络层、自研 UI 与可直接运行的 exe 都已落地。
> 本节为 2026-11-10 的当前状态。

| 层 | 位置 | 内容 | 状态 |
|---|---|---|---|
| L0–L4 **框架核心** | `src/`、`tests/` | 调度器、注册表、重启、验签、MCP 适配 | 完成，91 个测试 |
| **业务插件** | `plugins/` | 4 个插件 / 15 个工具（实例、Mod、启动、沙箱演示） | 完成，190 个测试 |
| **宿主网络** | `host-http/` | 真实出站 HTTPS（纯 Rust TLS + 域名白名单） | 完成，20 个测试 |
| **界面** | `ui/` | 自研即时模式 UI、两个渲染后端、三页界面 | 完成，146 个测试 |
| **交付物** | `dist/` | `MinecraftLauncher.exe`（949,402 字节）+ 截图 + 交付说明 | 可直接运行 |

**四层之间的依赖方向**：`dist/` 是 `ui/` 的构建产物；`ui/` 与 `plugins/` 都依赖 `src/`；
`host-http/` 只被宿主装配时注入 `plugins/official-mod`。
框架 `src/` 不反向依赖任何上层。

```
dist/MinecraftLauncher.exe
        ▲ 构建产物
ui/launcher-ui ──依赖──> ui/ui-core ──实现于──> ui/ui-backend-soft / ui-backend-win32
        ┊
        ┊  ⚠️ Cargo.toml 已声明框架与 common 依赖，
        ┊     但源码尚未调用任何插件工具（见第 8 节）
        ▼
launcher-plugin-framework (src/)  ◀── plugins/*  ◀── host-http（宿主侧注入 ctx.http）
```

---

## 2. 目录结构与四个独立 workspace

```
.
├── Cargo.toml / Cargo.lock     ★ workspace A：框架（根）
├── src/                        框架核心，零网络栈
├── tests/                      框架集成测试（6 个文件 + common/）
├── examples/host_demo.rs       端到端演示
├── docs/                       architecture / security-boundary / open-questions
├── build.ps1                   便捷构建脚本（把 cargo 加进 PATH）
├── plugins/                    ★ workspace B：业务插件
│   ├── common/                 插件共享层（清单 / schema / context / HttpPort）
│   ├── official-instance/      实例管理（4 工具）
│   ├── official-mod/           Mod 管理（4 工具，含在线搜索）
│   ├── official-launch/        启动参数与启动（4 工具）
│   └── sample-community/       沙箱演示插件（3 工具）
├── host-http/                  ★ workspace C：独立单包 crate，唯一出站 HTTPS
├── ui/                         ★ workspace D：自研 UI
│   ├── ui-core/                即时模式内核（#![forbid(unsafe_code)]）
│   ├── ui-backend-soft/        纯内存光栅后端（测试 / 离屏截图）
│   ├── ui-backend-win32/       真实窗口，手写 Win32 FFI（唯一允许 unsafe 的文件）
│   └── launcher-ui/            业务界面三页 + 外观设置 + PNG 编码
└── dist/                       交付目录
    ├── MinecraftLauncher.exe
    ├── README.md                ★ UI 交付说明（最详细的技术文档）
    └── screenshots/             软件后端三页 + 磨玻璃 + 真实 Win32 窗口截图
```

**这里没有一个统一的顶层 workspace**：四个 workspace 各自有 `Cargo.toml` / `Cargo.lock` / `target/`。
因此在仓库根执行 `cargo test` **只会跑框架的 91 个测试**，不会覆盖插件、网络层和 UI。

---

## 3. 快速开始

```bash
# 框架（根）
cargo test                                  # 91 passed
cargo run --example host_demo               # 可信调用 → 超时 → panic → 重启 → MCP

# 插件
cd plugins && cargo test --workspace        # 182 passed / 8 ignored（需可写 TMP，见下）
cd plugins && cargo run --example step6_cross_plugin   # 跨插件调用演示

# 宿主网络
cd host-http && cargo test                  # 17 passed / 3 ignored（真实网络默认忽略）

# UI
cd ui && cargo test --workspace             # 145 passed / 1 ignored
cd ui && cargo build -p launcher-ui --release
```

**直接运行界面**：双击 `dist/MinecraftLauncher.exe`，或

```bash
dist/MinecraftLauncher.exe --win-selftest    # 真实窗口渲染 8 帧 + 截图后自动关闭
dist/MinecraftLauncher.exe --shot instances  # 软件后端离屏渲染 → PNG + PPM
dist/MinecraftLauncher.exe --glass --blur 18 --window-radius 24
```

完整命令行参数（字体 / 磨玻璃 / 圆角 / 动画 / 明暗主题）见 [`dist/README.md`](dist/README.md) §1。

> **本机环境提示**：`plugins/` 的 9 个临时目录测试在默认 `%TEMP%` 下会报
> `PermissionDenied (os error 5)`（与代码逻辑无关，是目录权限）。把 `TMP` / `TEMP`
> 指到项目内可写目录即可全部通过：
> ```bash
> cd plugins && mkdir -p target/lpf-tmp && \
>   TMP="$PWD/target/lpf-tmp" TEMP="$PWD/target/lpf-tmp" cargo test --workspace
> ```
> 构建时偶发的 `error deleting lock file ... 拒绝访问 (os error 5)` 是同一类增量编译文件锁问题。

需要 Rust 1.85+；本机使用 `stable-x86_64-pc-windows-gnu`。

---

## 4. 框架核心（`src/`）

```
src/
├── error.rs            10 个错误码 + MCP 同构映射
├── types.rs            工具元数据、信任级别、函数引用、结果形状
├── json.rs             inputSchema 校验（fail-closed 子集）
├── catch_unwind.rs     异步 panic 捕获（包住 Future::poll）
├── scheduler/
│   ├── state.rs        六状态状态机 + 合法迁移表
│   ├── unit.rs         虚拟进程执行单元（探针、配额、超时、取消）
│   ├── reaper.rs       空闲 / 最长存活回收
│   └── mod.rs          Scheduler trait
├── registry/
│   ├── impl_registry.rs  **call_tool 完整分流逻辑**
│   └── mod.rs            ToolRegistry trait
├── restart.rs          退避 + 熔断 + 五步重建
├── signature/          包整体验签 + 吊销列表
├── loader.rs           验签 → 信任分级 → 降级
└── mcp.rs              无状态 MCP 适配（无 initialize 握手）
```

`call_tool` 的分流顺序：**① 工具存在 → ② ACL → ③ 执行单元状态 / `note`
→ ④ `input_schema` 参数校验 → ⑤ 按 `TrustLevel` 分流**（可信直调，沙箱走调度器）。
参数校验插在状态检查之后是刻意选择：插件重启中返回 `ProcessRestarting` 比让用户去改参数更准确（Q5）。

### 三条最容易实现错的地方（本实现已处理）

**1. 超时计时器必须在 handler 之外** —— handler 跑在独立 spawn 的 task 上，计时器在调用方 task 上 race。
否则 handler 阻塞 poll 时计时器永不触发，就无从区分「可中断」与「不可中断」。

**2. 可中断性靠探针，不靠心跳计数**

| handler 此刻 | `in_poll` | abort 生效？ |
|---|---|---|
| `await sleep(10s)`（parked） | `false` | ✅ 立刻 |
| `std::thread::sleep(1s)` | `true`（持续 1s） | ❌ 无效 |

判据是「`in_poll` 连续为 true 超过 `uninterruptible_grace_ms`」。用「心跳是否前进」会**误判** parked 的 future。

**3. 重启的「重启中」标记要在退避之前** —— 一旦进入重启流程就先 `note_availability(Restarting)`，
早于 1 秒退避。且该标记在崩溃回调里**同步**完成（只拿锁、不 await），否则会出现
「调用返回 `ProcessCrashed` 但列表显示 available」的窗口。

### 关键接口

```rust
// 调度器（只服务沙箱）
trait Scheduler {
    fn create_unit(&self, config: UnitConfig) -> BoxFuture<'_, Result<UnitHandle, LoadError>>;
    fn submit(&self, unit: UnitHandle, task: Task, timeout_ms: u64)
        -> BoxFuture<'_, TaskOutcome>;                      // 必 resolve
    fn cancel(&self, unit: UnitHandle, task_id: TaskId) -> CancelOutcome;  // 可报 Uninterruptible
    fn destroy(&self, unit: UnitHandle) -> BoxFuture<'_, ()>;
    fn on_crash(&self, unit: UnitHandle, cb: CrashCallback) -> Result<SubscriptionId, SchedulerError>;
    fn get_state(&self, unit: UnitHandle) -> Option<UnitState>;
}

// 注册表（可信 + 沙箱都往这里登记）
trait ToolRegistry {
    fn register_tool(&self, meta: ToolMeta) -> Result<(), RegistryError>;
    fn unregister_tool(&self, caller: &PluginId, name: &str) -> Result<(), RegistryError>;
    fn list_tools(&self, caller: &PluginId, filter: &ToolFilter) -> Vec<ToolListing>;
    fn call_tool(&self, caller: &PluginId, name: &str, args: Value)
        -> RegistryFuture<'_, ToolResult>;
    fn restart_unit(&self, caller: &PluginId, plugin_id: &PluginId)
        -> RegistryFuture<'_, Result<(), ToolError>>;       // 仅平台层
}
```

---

## 5. 插件与宿主网络

4 个插件、15 个工具，工具在注册表里的名字是 `插件ID.短名`。三个 official 插件在当前仓库
**没有真实签名**，因此会被验签器降级到沙箱 —— `plugin.json` 里的 `trust` 只是声明，不是结论。

| 插件 | 工具数 | 要点 |
|---|---|---|
| `official-instance` | 4 | 实例增删改查，名字大小写不敏感去重，`delete_instance` 需 `confirm` |
| `official-mod` | 4 | Modrinth 在线搜索（真实 HTTPS）、域名白名单、跨插件校验实例是否存在 |
| `official-launch` | 4 | 启动配置读写、日志尾部；`launch_game` 返回 `pid: null` |
| `sample-community` | 3 | 无签名沙箱演示：可中断超时 / 故意 panic → 自动重启 |

`host-http/` 的边界：插件只能通过 `HttpPort` 访问网络；白名单在**最外层**比对
（非白名单域名请求根本不发出）、不跟随重定向、只接受 `https://`、响应体 4 MB 上限、
socket 超时 + `spawn_blocking`。详见 [`host-http/README.md`](host-http/README.md) 与
[`plugins/README.md`](plugins/README.md)。

---

## 6. UI 与交付物

`ui/` 是零第三方 GUI 依赖的自研即时模式 UI：`Ui::begin_frame` 每帧构造，控件返回 `Response`，
`DrawList` 交给后端 `present()`。当前具备：

- **圆角窗口与圆角控件**、状态过渡动画、可配置磨玻璃、自定义字体 / 字号缩放、明暗主题
- **无边框自绘标题栏**（`— □ ✕` + 顶栏拖动）、**内存 DC 双缓冲**（不闪烁）
- 三页界面的真实交互：多选 / 范围选择 / 右键菜单 / 滚动 / 搜索 / 表单 / 二次确认 / toast
- 软件后端与 Win32 后端；后者手写 FFI，DWM / composition 等可选 API 运行时动态取地址

`dist/MinecraftLauncher.exe` 可直接运行，`dist/screenshots/ui-win32-shot.png` 是真实窗口截图。
**技术细节、命令行参数、绘制指令集、踩过的坑与完整已知限制全部在
[`dist/README.md`](dist/README.md)** —— 该文件是本仓库最详细的 UI 文档。

---

## 7. 测试与静态检查现状

| workspace | 测试数 | 结果 |
|---|---:|---|
| 框架（根） | 91 | 91 passed |
| `plugins/` | 190 | 182 passed / 8 ignored（真实网络） |
| `host-http/` | 20 | 17 passed / 3 ignored（真实网络） |
| `ui/` | 146 | 145 passed / 1 ignored |
| **合计** | **447** | **435 passed / 12 ignored** |

`ui/` 内部分布：`ui-core` 90（含 1 ignored）、`ui-backend-soft` 16、`launcher-ui` 40、
`ui-backend-win32` 0（FFI 由 `--win-selftest` 的真实窗口验证）。

`ui/` 的 `cargo fmt --all -- --check` 通过、`cargo clippy --workspace --all-targets` 代码零警告、
release 构建通过、`--win-selftest` 真实窗口渲染 8 帧并截图成功。
`plugins/` 的 fmt / clippy 亦为零警告（见 [`plugins/README.md`](plugins/README.md) §测试）。

> 数字口径：`cargo test --workspace -- --list` 计数，含 `#[ignore]` 与所有 test target。
> 真实网络测试默认忽略，手动用 `cargo test -- --ignored --nocapture` 运行。

---

## 8. 已知限制（诚实记录，不是 bug 清单）

### 框架层

1. `UnitConfig::memory_limit_bytes` 是**声明性**字段，纯 Rust 单元无法强制内存上限；要真限制需 WASM 沙箱。
2. `catch_unwind` 挡不住 `abort` / 段错误 / OOM / `exit()` / `panic="abort"` 的 panic —— 它是「panic 保险丝」。
3. 沙箱插件的 native 代码可读宿主内存：**逻辑隔离 ≠ 安全隔离**。
4. 热重载不支持跨信任级别迁移（Q3）。
5. 框架的 `tests/restart_manager.rs` 有一条毫秒级退避断言在全量并发时**偶发**失败，单独跑必过。

### 插件与网络层

6. `launch_game` 返回 `pid: null` / `simulated: true`。要让游戏真跑起来必须打破「单进程、无子进程」前提，这是**架构级决策**。
7. `install_mod` 仍写占位文件；真实 Mod 下载、sha1/sha512 校验、依赖解析**尚未实现**。
8. `plugin.json` 里的 `signature` 字段目前**不会被解析**（serde 静默忽略），改它不影响信任级别。
9. `host-http` 不做证书固定 / HTTP/2 / 连接池 / per-host 限流；`rustls-rustcrypto` 是 alpha 版。

### UI 层

10. **UI 尚未接入插件工具**：`launcher-ui/Cargo.toml` 声明了框架与 `common` 依赖，但源码没有任何调用；
    界面操作只写本地 `DataStore`。数据形状与真实返回一致，接入时换 `LiveDataSource` 即可。
11. 在线 Mod 搜索未接网络（搜索框只做本地过滤）；外观设置不持久化；无边框窗口不能拖拽边缘缩放；高 DPI 仍是粗估。
12. Win32 的 `BlurRegion` 是半透明 tint 近似；软件后端无 CJK 点阵（软件截图中文是方块，真实窗口正常）。

---

## 9. 遗留决策与文档索引

| 文档 | 内容 |
|---|---|
| [`docs/architecture.md`](docs/architecture.md) | 框架分层（L0–L4）、依赖方向、计时器与 handler 分离 |
| [`docs/security-boundary.md`](docs/security-boundary.md) | **必读**：可信 / 沙箱边界、宿主前置条件 |
| [`docs/open-questions.md`](docs/open-questions.md) | Q1–Q8 遗留决策的权威记录 |
| [`plugins/README.md`](plugins/README.md) | 15 个工具、装配形状、跨插件调用、网络白名单 |
| [`host-http/README.md`](host-http/README.md) | 技术选型、SSRF 修复、真实网络 bug 复盘 |
| [`dist/README.md`](dist/README.md) | UI 架构、交互范式、绘制指令、交付与验证 |

影响最大的三条：

| # | 问题 | 影响 |
|---|---|---|
| Q1 | 无吊销列表的发行版是否 fail-closed | 安全等级（当前默认 fail-closed + 逃生门） |
| Q2 | 运行期包被篡改：`Failed` 还是降级 | 加载策略（当前标记 Failed 并停止重启） |
| Q8 | 是否改用 WASM 沙箱 | 架构（会改 handler 签名，越晚改成本越高） |

> **Q8 的文档状态**：`docs/open-questions.md` 仍保留「需要你定」的原始措辞；
> 而 `plugins/README.md` 记载的是「已裁决保持当前架构，沙箱准入改为人工 review」。
> 当前**代码行为**按后者落地。两处措辞尚未对齐，若要做正式决策记录请先更新 `docs/open-questions.md`。

---

## 10. 安全边界（三句话版）

1. **可信插件就是宿主**：它的 native 崩溃、`exit()`、OOM 会带走整个启动器。签名校验是唯一防线。
2. **沙箱插件不是安全沙箱**：单宿主进程内是逻辑隔离 + API 限制 + 资源配额，
   **挡不住 native 代码逃逸**，除非执行单元实现为 WASM 字节码沙箱。
3. **重启管理器不覆盖可信插件**：可信插件崩溃 = 宿主故障，恢复靠用户重启启动器。
