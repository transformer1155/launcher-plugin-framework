# 第一批插件

本目录是 `launcher-plugin-framework`（下称**框架**）之上的第一批插件。
框架代码位于上一级 `../src/`，**本批插件没有改动框架任何文件**（含 `examples/host_demo.rs`）。

四个插件、15 个工具。

| 插件 crate | 插件 ID | 工具数 | 声明 trust | review |
|---|---|---|---|---|
| `official-instance` | `com.example.official-instance` | 4 | trusted | official |
| `official-mod` | `com.example.official-mod` | 4 | trusted | official |
| `official-launch` | `com.example.official-launch` | 4 | trusted | official |
| `sample-community` | `com.example.sample-community` | 3 | **sandboxed** | **unreviewed** |

> ⚠️ **`trust` 是声明，不是结论。** 实际信任级别由框架的签名校验结果决定。
> 三个 official 插件在本仓库里**没有真实签名**，因此用当前验签器加载时会被**降级到沙箱**。
> 这正是设计意图：插件无权自封可信。

---

## 目录结构

```
plugins/
├── Cargo.toml              workspace（5 个成员：common + 4 插件）
├── common/                 插件共享层（清单/参数 schema/context/结果/工具集合）
├── official-instance/      实例管理
├── official-mod/           Mod 管理（含网络）
├── official-launch/        启动参数与启动
└── sample-community/       沙箱演示插件
```

每个插件的结构：

```
<plugin>/
├── Cargo.toml
├── plugin.json            插件清单
├── src/
│   ├── lib.rs             入口：tools() / build(data_dir) / build_with_logger(..)
│   ├── store.rs           私有数据持久化
│   └── tools/             每个工具一个文件（handle + def()）
├── tests/tools.rs         单测（直接调 handler，不启动框架）
└── examples/stepN_e2e.rs  端到端演示（经完整框架链路）
```

## 运行

```bash
cd plugins
cargo test --workspace        # 190 个测试（182 passed / 8 ignored）
cargo run --example step1_e2e # 实例插件：loader → registry → MCP tools/call
cargo run --example step3_e2e # Mod 插件：网络白名单强制 + 模拟安装
cargo run --example step4_e2e # 启动插件：配置读写 + 模拟启动
cargo run --example step5_e2e # 沙箱降级 → 崩溃 → 重启 → 恢复（9 步演示）
cargo run --example step6_cross_plugin # ★ 跨插件调用：install_mod 真的去查 official-instance
```

---

## 工具清单

工具名在注册表里是 **`插件ID.短名`**（框架自动加前缀，插件代码里只写短名）。

### official-instance（实例管理）

| 短名 | destructive | timeout | 说明 |
|---|---|---|---|
| `list_instances` | false | 5s | 列出实例，按最近游玩降序 |
| `create_instance` | false | 5s | 创建实例 |
| `switch_instance` | false | 5s | 切换并记录 `last_played` |
| `delete_instance` | **true** | 5s | 删除实例，**必须 `confirm: true`** |

`create_instance(name, game_version, loader)` — `loader` 枚举 `vanilla|fabric|forge`。

**参数校验顺序**（决定了错误码）：

1. schema（宏生成）→ `InvalidArguments`
2. 实例名语义（非空 / ≤64 字符 / 不含 `/ \ : ..` 与控制字符）→ `InvalidArguments`
3. 版本格式（`x.y` 或 `x.y.z`）→ `InvalidArguments`
4. loader 枚举 → `InvalidArguments`
5. 重名（**大小写不敏感 + 忽略首尾空白**）→ 业务错误 `already_exists`
6. 数量上限（200）→ 业务错误 `limit_exceeded`

**为什么名字不区分大小写**：Windows/macOS 的实例目录名不区分大小写，放过去会让两个索引项指向同一目录。

### official-mod（Mod 管理）

| 短名 | destructive | timeout | 网络 | 说明 |
|---|---|---|---|---|
| `list_installed_mods` | false | 5s | 否 | 列出已安装 Mod |
| `search_mods` | false | **15s** | **是** | Modrinth 在线搜索 |
| `install_mod` | **true** | 10s | 否 | **模拟安装**（写占位文件） |
| `remove_mod` | **true** | 5s | 否 | 移除 + 删文件 |

**网络声明**（`plugin.json` → `network`）：

```json
{ "allow_hosts": ["api.modrinth.com"],
  "reason": "Mod 搜索需要访问 Modrinth 的公开 API；不涉及用户账号与写操作" }
```

代码里的 `official_mod::REQUIRED_HOSTS` 必须与之一致，测试 `manifest_network_matches_code` 断言这一点。

**白名单是两层的**：

- 插件侧：只拼 `https://api.modrinth.com/...`
- 宿主侧：`AllowListedHttp` 在**发请求前**比对白名单，不在则**根本不调用传输层**

测试 `search_allowlist_blocks_unlisted_host` 断言 `transport_calls == 0` —— 不是"发了被拒"，是**从未发出**。

**无网络时**（宿未注入 `HttpPort`）：返回业务错误 `upstream_error`（**不是** `PermissionDenied`），
UI 据此显示"网络不可用"而非"你越权了"。503 / 超时 / 坏 JSON 同样归入 `upstream_error`。

### official-launch（启动管理）

| 短名 | destructive | timeout | 说明 |
|---|---|---|---|
| `get_launch_config` | false | 5s | 读配置；**未配置返回默认值** + `is_default: true` |
| `set_launch_config` | **true** | 5s | 全量覆盖配置 |
| `launch_game` | **true** | 10s | **模拟启动，`pid` 恒为 `null`** |
| `get_last_log` | false | 10s | 读日志尾部，大文件自动截断 |

`memory_mb` 边界 **512..=32768**。低于 2048 给 WARN 但不阻断；低于 512 直接拒绝（`precondition_failed`）。

`set_launch_config` 是**全量覆盖**（三个字段都传），不做增量合并 ——
否则"我想清空 game_args"无法表达（空数组与"没传"无法区分）。

`launch_game` 返回：

```json
{ "pid": null, "simulated": true, "instance_id": "...", "memory_mb": 6144,
  "note": "模拟启动：未创建任何 OS 进程（框架约束：单进程、禁止子进程）..." }
```

**为什么 `pid` 是 `null` 而不是假数字**：假 pid 会让 UI 的 `kill` / 进程检测逻辑**静默地错**，
且错误延后暴露。`null` 迫使调用方显式处理"没有真实进程"。

两条静态断言（`launch_never_mentions_real_process_creation`、`no_process_spawn_anywhere_in_launch_plugin`）
扫描源码，断言不含 `std::process::Command` / `tokio::process` / `.spawn()`。

### sample-community（沙箱演示）

| 短名 | destructive | timeout | 说明 |
|---|---|---|---|
| `hello` | false | 5s | 打招呼（证明降级后功能完好） |
| `slow_task` | false | **2s** | 用 `tokio::time::sleep` → **可中断超时** |
| `crash_me` | **true** | 5s | 故意 panic → 沙箱崩溃 → 自动重启 |

**无签名**（`plugin.json` 里没有 `signature` 字段），`trust: sandboxed` + `review: unreviewed`，
因此必然降级到沙箱，并触发 Q8 要求的 WARN：

```
插件 com.example.sample-community 为沙箱级且未人工审核（review=unreviewed），
运行在受限沙箱；请勿给它授予额外能力
```

`slow_task` 刻意用 **tokio** 的 sleep 而不是 `std::thread::sleep`：

| 实现 | handler 状态 | 框架判定 | 结果 |
|---|---|---|---|
| `tokio::time::sleep` | parked（`in_poll=false`） | 可中断 | `Timeout`，单元**保持 Running** |
| `std::thread::sleep` | 卡在 poll 内 | 不可中断 | `Uninterruptible` + 单元**崩溃** |

`crash_me` 标 `destructive: true` 的理由不是"改数据"，而是**它会让该插件的所有其他工具短暂不可用**
（整个单元被重建）。

---

## 宿主如何装配

四个插件共用同一套装配形状：

```rust
// 1) 每个插件构造 (ctx, ToolSet)
let (ctx, toolset) = official_instance::build(data_dir_for("com.example.official-instance"))?;

// 2) 可选：注入能力（仅 official-mod 需要网络）
let ctx = ctx.with_http(AllowListedHttp::new(host_http_impl, &network_decl));
// 可选：注入跨插件调用能力（需要 &Arc<dyn ToolRegistry> 的 Weak）
let ctx = ctx.with_registry(Arc::downgrade(&registry));

// 3) 交给框架 loader
let build_tools = Arc::new(move |unit: Option<UnitHandle>| {
    let trust = if unit.is_some() { TrustLevel::Sandboxed } else { TrustLevel::Trusted };
    toolset.into_metas(&ctx, trust, unit, vec![PluginId::new("*")])
});
let loaded = loader.load(package, unit_config, build_tools).await?;
```

然后经 `McpAdapter` 的 `tools/list` / `tools/call` 暴露给 UI。

**注意 `trust` 的来源**：`build_tools` 只能拿到 `Option<UnitHandle>`，所以上面的写法用
"有单元 → Sandboxed"推导。真实加载器返回的 `loaded.trust_level` 才是权威值。

### 清单一致性校验（重要）

`plugin.json` 的 `tools` 必须与代码注册的**完全一致**（双向）：

```rust
let manifest = PluginManifestV1::parse(include_bytes!("plugin.json"))?;
manifest.check_tools(&toolset.manifest_tools())?;  // 多一个、少一个都会失败
```

四个插件的测试里都有这一条（`tool_set_matches_manifest`）。

---

## 数据落盘位置

所有插件只写自己的私有数据目录（`ctx.data_dir()`，由宿主指定），不碰其他路径。

| 插件 | 文件 |
|---|---|
| `official-instance` | `instances.json` |
| `official-mod` | `mods.json`；`instances/<instance_id>/mods/*.jar` |
| `official-launch` | `launch.json`；`instances/<instance_id>/logs/latest.log` |
| `sample-community` | 不落盘 |

全部采用**临时文件 + rename** 的原子写（避免崩溃时留下截断 JSON）。

---

## 已知限制与待决问题

### 1. `plugin.json` 的 `signature` 字段目前**不会被解析** ⚠️

三个 official 插件的 `plugin.json` 里写了：

```json
"signature": { "algorithm": "ed25519", "key_id": "...", "signature_hex": "PLACEHOLDER_..." }
```

但 `PluginManifestV1` **没有** `signature` 字段 —— serde 会**静默忽略**它（已验证：带 signature 的清单照常解析成功）。

真正的签名材料在框架的 `PluginPackage.signature: Option<PluginSignature>` 里，由**打包工具**产生，
与 `plugin.json` 无关。

**这意味着**：`plugin.json` 里那段 `signature` 只是给未来的打包流程留的占位说明，
现在**没有强制力，也没有被读取**。不要误以为改了 `signature_hex` 就能改变信任级别。

### 2. 真实启动游戏与"无子进程"约束冲突（架构级）

`launch_game` 返回 `pid: null`。要让游戏真的跑起来，必须打破框架的
"单 OS 进程 / 无子进程"前提。这是**框架级决策**，不是插件能定的。

### 3. 网络：真实实现已完成（`host-http`）

**真实出站 HTTPS 已实现并通过真实网络验证**，详见 [`../host-http/README.md`](../host-http/README.md)。

| 能力 | 状态 |
|---|---|
| 真实 TLS（`rustls` + 纯 Rust `rustls-rustcrypto`） | ✅ 与 Modrinth 完成握手 |
| 真实 Mod 搜索（`search_mods`） | ✅ 取回真实数据（JEI 等） |
| 域名白名单强制（在最外层） | ✅ 非白名单域名请求**不发出** |
| URL 转义（含 facet 编码） | ✅ 修复了导致 400 的 bug |
| 响应体上限 / 超时 / chunked | ✅ |
| 不跟随重定向（防白名单绕过） | ✅ |
| **Mod 真实下载** | ❌ 仍是 `install_mod` 写占位文件（`simulated: true`） |
| 下载后文件校验（sha1/sha512） | ❌ |
| 依赖解析 | ❌ |

**边界**：`host-http` 是**宿主侧独立 crate**，不在框架 `src/`、不在插件里。
插件只能通过 `ctx.http()` 使用它，拿不到 `std::net`。

启用手册见 [`../host-http/README.md`](../host-http/README.md) §7。

### 4. 跨插件调用：已端到端验证（`install_mod` → `list_instances`）

`PluginContext::call_tool` 现在**有真实使用者和端到端验证**：`install_mod` 会通过它查询
`official-instance.list_instances`，确认目标实例是否存在。

验证覆盖（`plugins/official-mod/tests/cross_plugin.rs`，6 个测试）：

| 场景 | 期望行为 |
|---|---|
| 实例存在 | 安装成功 |
| **实例不存在** | 拒绝，`not_found`，且**不创建任何目录**（无副作用） |
| **实例插件未加载** | **降级放行**（保证 `official-mod` 可独立使用） |
| ctx 未注入注册表 | 降级放行 |
| 注册表已被 drop | 返回明确错误"注册表已释放"，不 panic |
| caller 身份 | 是 `com.example.official-mod` 本身（ACL 按此判定） |

**"降级放行"是刻意选择，不是遗漏**：两个插件可独立加载/卸载，若"查不到实例插件就拒绝安装"，
则只装 `official-mod` 的用户会完全用不了它，且两者加载顺序会变成隐式依赖。

区分"无法校验"与"确认不存在"靠框架错误码：

| 上游结果 | 处理 |
|---|---|
| `ToolNotFound`（实例插件没装） | 放行 |
| `PermissionDenied`（ACL 未给） | 放行 |
| `ProcessRestarting` / `ProcessCrashed` | 放行（不因别人重启而放大故障） |
| 调用成功但列表里无此 ID | **拒绝** |

**残留限制**：这是"尽力而为"的校验，不是事务性保证 ——
实例可能在 `list_instances` 与写盘之间被删除（TOCTOU）。当前靠"装到不存在的实例里只是多一个目录"
来兜底，没有做跨插件锁（框架也没有提供事务原语）。

### 5. 内存上限仍是声明性的

框架的 `UnitConfig::memory_limit_bytes` 无法在纯 Rust 单元里强制执行
（Q8 已裁决保持当前架构）。沙箱是**故障隔离**，不是安全隔离。

---

## 测试

**190 个测试：182 passed / 8 ignored**（框架另有 91 个，未受影响）。
8 个忽略的都是真实网络测试，手动用 `--ignored` 运行。

下表按 `cargo test -p <crate> -- --list` 计数（含 `#[ignore]` 与所有 test target）：

| 套件 | 数量 | 覆盖 |
|---|---|---|
| `common` | 52 | 清单校验、schema 派生、参数解析、context、结果构造、临时目录支持 |
| `official-instance` | 38 | 存储/校验 + 4 个工具全维度 |
| `official-mod` | 55 | 存储 + modrinth + 白名单强制 + 模拟安装 + **跨插件调用** + 真实网络（ignored） |
| `official-launch` | 33 | 存储/日志尾部 + 4 个工具 |
| `sample-community` | 12 | 3 个工具 + 清单断言 |
| **合计** | **190** | |

每个工具至少覆盖：正常路径 / 参数非法 / 业务错误 / 边界值 / 失败路径。

`cargo fmt --check` 通过，`cargo clippy --workspace --all-targets` **零警告**。

> **本机 `%TEMP%` 权限问题（与代码逻辑无关）**：默认临时目录下 `common` 的 9 个
> `test_support` / `data_dir` 测试会报 `PermissionDenied (os error 5)`（创建目录被拒）。
> 把 `TMP` / `TEMP` 指到项目内可写目录即可全部通过：
> ```bash
> mkdir -p target/lpf-tmp && \
>   TMP="$PWD/target/lpf-tmp" TEMP="$PWD/target/lpf-tmp" cargo test --workspace
> ```
> 仓库根 README 的「测试与静态检查现状」一节记录的是这组可写临时目录下的结果。

> **已知的框架层时序抖动（与本批插件无关）**：
> 框架的 `tests/restart_manager.rs::crash_triggers_restart_and_tool_becomes_available_again`
> 在全量并发运行时**偶发**失败（单独跑或重跑必过）。根因是它用毫秒级退避来验证重启，
> 而对"多久应恢复"的假设偏紧。这是框架原有测试的性质，本批插件未改动框架、也未引入此问题。

---

## 安全边界（重申）

三个 official 插件声明 `trust: trusted`，但**签名校验是唯一防线**。在当前仓库（无真实签名）里，
它们全部会被降级到沙箱。一旦某个插件通过验签进入宿主上下文：

- 它的未捕获 native 崩溃、`exit()`、内存破坏、OOM **会带走整个启动器**
- `catch_unwind` 只挡 Rust unwind panic，挡不住上述任何一种

`sample-community` 的沙箱只是**逻辑隔离 + 配额 + 超时**，挡不住 native 逃逸。
Q8 已裁决保持此架构，**沙箱插件准入需人工 review** —— 这就是 `plugin.json` 里
`review` 字段的用途（`unreviewed` 加载时打 WARN）。
