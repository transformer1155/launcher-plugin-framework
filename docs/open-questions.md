# 需要你确认的遗留问题

规格里没有写死、或我的实现做了**超出规格的决策**的地方，全部列在这里。
每一条都给了我的当前选择与理由，标了影响面，方便你逐条裁决。

---

## Q1. 「无吊销列表」的发行版该怎么处理？（影响：安全等级，高）

**现状**：`Ed25519Verifier` 默认 `require_revocation_list = true`。未装载吊销列表时
**所有**插件都判 `RevocationUnverifiable` → 全部降级到沙箱（fail-closed）。

**问题**：规格说"吊销列表随启动器更新"，但没写"如果启动器没带吊销列表怎么办"。
如果你们的发布流程里存在"首次启动 / 离线环境 / 吊销库尚未下载"的窗口，那默认
fail-closed 会让**所有**插件（包括官方签名的）在那一刻降级为沙箱。

**我的选择**：默认 fail-closed，同时提供 `.without_required_revocation_list()` 逃生门。

**需要你定**：三选一 ——
- (a) 保持 fail-closed，吊销列表必须随二进制打包（推荐，最安全）；
- (b) 允许"无列表 = 无人被吊销"（启动更快，但被吊销的插件在这个窗口里能拿到可信权限）；
- (c) 无列表时只信任"内嵌在二进制里的核心插件"，其余降级。

## Q2. 「运行期包被篡改」该降级还是拒绝？（影响：加载策略，中）

**现状**：重启时重新验签，若发现 `package.digest() != loaded_digest`（运行期间包文件
被改写了），我**标记 Failed 并停止自动重启**，而不是降级到沙箱继续跑。

**理由**：这不是"没签名"，而是"签名之后内容被改"。降级会让篡改后的代码在受限环境里
执行；停止则明确告诉用户"这个包不可信了"。

**但**：规格只说了"校验失败 → 降级"。这一条严格讲超出了规格。

**需要你定**：是接受"篡改 → Failed"，还是要求一律降级？

## Q3. 热重载时信任级别变化（Sandboxed ↔ Trusted）怎么处理？（影响：生命周期，中）

**现状**：`PluginLoader::hot_reload` **拒绝**跨级别迁移，返回明确错误，要求先卸载再加载。
同级迁移（沙箱→沙箱、可信→可信）正常支持。

**问题**：一个原本无签名的插件，作者后来补了官方签名；热重载时验签从 `Unsigned` 变成
`Valid`。规格说"校验时机包括热重载"，但没说"结论变好了要不要升级执行上下文"。

**难点**：升级需要原子地切换**已在册**工具的 `trust_level`、`unit_handle`（清空）与
handler 来源。中途若有在飞调用，会出现"半可信"状态。

**我的选择**：保守拒绝。理由：跨执行上下文迁移一旦做错，就是把不可信代码提权到宿主，
风险远大于"让用户手动重载"。

**需要你定**：是否值得实现原子迁移？（如果实现，我建议方式是"卸载 + 重载"，
并在 UI 上显示"插件需要重启以应用新的信任级别"。）

## Q4. `cancel` 遇到"任务已结束"该返回什么？（影响：接口语义，低）

**现状**：`CancelOutcome` 有 5 个变体，其中规格只定义了 2 个（`Cancelled` /
`Uninterruptible`）。我补了 `AlreadyFinished` / `AlreadyTimedOut` / `NotFound`。

**理由**：规格没写"cancel 一个已经结束的任务"该返回什么。把它塞进 `Cancelled` 是在说谎
（没取消任何东西），塞进 `Uninterruptible` 会误导调用方去重启插件。

**协议视图**：`as_protocol_str()` 把前四个映射为 `"Cancelled"`，只有 `Uninterruptible`
映射为 `"Uninterruptible"` —— 所以对外的二元语义**完全符合规格**，多出来的变体只是
内部的精度。

**需要你定**：认可这个扩展吗？还是要求严格二元返回？

## Q5. 参数校验（`InvalidArguments`）放在第几步？（影响：用户体验，低）

**现状**：检查顺序是
1. 工具存在 → 2. ACL → 3. 执行单元状态 → 4. **参数校验** → 5. 分流执行。

**问题**：规格的四步检查里没有参数校验（错误码表里有 `InvalidArguments`，但没说放在哪）。
我把它放在状态检查**之后**：插件正在重启时，返回 `ProcessRestarting` 比
`InvalidArguments` 更准确 —— 因为用户改参数也没用，得等插件起来。

**替代方案**：放在 ACL 之后、状态之前（参数错误立刻反馈，但重启期间会误导用户去改参数）。

**需要你定**：保持现状，还是调整顺序？

## Q6. `list_tools` 的 `available` 与调用结果的组合语义（影响：UI，低）

**现状**：`list_tools` 在重启期间返回 `available: false, reason: "restarting"`，
而 `call_tool` 立即返回 `ProcessRestarting`。二者组合的正确用法是
"UI 灰掉按钮 + 若用户硬点则显示重启中"。

**问题**：规格没有说明 `available: false` 时是否还应该允许 `call_tool` 被调用。我的实现
**允许调用但立即失败**（不排队），而不是在注册表层面直接拒绝调用。

**理由**：`available` 是给 UI 的提示，不是准入开关。真正的准入是 `call_tool` 里的状态检查。
两处都有检查，但 `available: false` 不阻止调用进入 `call_tool` —— 否则竞态窗口里
"UI 显示可用但调用被拒"会更令人困惑。

**需要你定**：认可吗？

## Q7. 沙箱崩溃的错误码粒度（影响：可观测性，低）

**现状**：
- 沙箱 handler panic → `ProcessCrashed`
- 超时且可中断 → `Timeout`
- 超时且不可中断 → `Uninterruptible`（同时升级为单元级崩溃）
- 重启失败 → `ProcessFailed`
- 重启中 → `ProcessRestarting`

规格的 `ProcessCrashed` 与 `Uninterruptible` 有语义重叠（后者也是崩溃）。我让
`Uninterruptible` **优先**返回（因为它是更具体的原因），单元状态同时变为 `Crashed`。

**需要你定**：如果你希望"不可中断超时"在协议层表现为 `ProcessCrashed`（把
`Uninterruptible` 只作为内部原因），我可以改。当前选择是"更具体的错误码优先"。

## Q8. 是否要把 `memory_limit_bytes` 换成真正的 WASM 沙箱？（影响：架构，大）

**现状**：`memory_limit_bytes` 是**声明性字段**，无法强制执行（见
`docs/security-boundary.md` 第 2.1 节）。

**问题**：如果你们的产品定位里"沙箱"要能真正限制内存与 CPU，那纯 Rust 单元架构
做不到，必须把执行单元换成 WASM 字节码沙箱（wasmtime / wasmer 之类）。

**这是一个架构级决策**，会改变：
- handler 的形态（从 `Arc<dyn Fn>` 变成 WASM 导出函数）；
- 参数/返回值通道（从 `serde_json::Value` 变成 WASM 内存里的序列化数据）；
- 启动开销（实例化 WASM 模块 vs 直接调用）。

**需要你定**：当前的"逻辑隔离 + 配额"是否满足产品需求？如果是"否"，我们应当**现在**
改架构，而不是在后面补 —— 因为 handler 的类型签名会变。

---

## 附：规格里我认为已经完整落地的部分（无需确认）

| 规格条目 | 落地位置 | 测试 |
|---|---|---|
| 单 OS 进程、无子进程/HTTP/socket/管道 | 全局（`Cargo.toml` 依赖里无网络栈） | — |
| 可信插件直接调用 + `catch_unwind` 包 poll | `catch_unwind.rs` / `impl_registry.rs::dispatch_trusted` | `panic_capture.rs` (6) |
| 沙箱插件走调度器 | `impl_registry.rs::dispatch_sandboxed` | `sandbox_semantics.rs` |
| 验签失败降级（不拒绝） | `loader.rs` | `signature_downgrade.rs` |
| `create_unit` / `submit` / `cancel` / `destroy` / `on_crash` / `get_state` | `scheduler/mod.rs` | `sandbox_semantics.rs` |
| 工具级超时优先、不可中断升级崩溃 | `scheduler/unit.rs` | 2 个专项测试 |
| `cancel` 能报 `Uninterruptible` | `scheduler/unit.rs::cancel` | 2 个专项测试 |
| 工具元数据 9 个字段 | `types.rs::ToolMeta` | `types.rs` 单测 |
| `register/unregister/list/call/restart_unit` | `registry/` | `registry_acl.rs` (12) |
| ACL 在 list 与 call 两处一致 | `impl_registry.rs::allows_caller` 复用 | `registry_acl.rs` |
| 四步检查 + 分流 | `impl_registry.rs::call_tool` | `registry_acl.rs` |
| 指数退避 1/2/4/8/…/30s | `restart.rs::BackoffPolicy` | 单测 |
| 5 分钟窗口 > 5 次 → CircuitOpen | `restart.rs::CircuitBreaker` | 单测 + 集成 |
| 重启流程 5 步 | `restart.rs::rebuild` | `restart_manager.rs` |
| 重启失败 → Failed 不再重试 | `restart.rs::give_up` | `restart_manager.rs` |
| 重启期间 available=false + 立即报错不排队 | `note_availability` + `call_tool` | `restart_manager.rs` |
| 包整体签名覆盖清单+代码 | `signature/mod.rs::digest` | `signature_downgrade.rs` |
| 吊销列表自身签名 | `signature/revocation.rs` | `signature_downgrade.rs` |
| 三个校验时机（加载/重启/热重载） | `loader.rs` + `restart.rs` | — |
| MCP tools/list 五字段 | `mcp.rs` | `mcp_semantics.rs` |
| MCP tools/call `{content,structuredContent,isError}` | `mcp.rs` | `mcp_semantics.rs` |
| 崩溃与业务错误协议同构 | `types.rs::ToolResult::error` | `mcp_semantics.rs` |
| 10 个错误码 | `error.rs::ToolErrorCode` | 各测试按码断言 |
| 所有接口异步化 | 全部返回 `impl Future` / `BoxFuture` | — |
