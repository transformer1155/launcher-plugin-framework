//! 插件工具集合：把 `ToolDef` 变成框架的 `ToolMeta`。
//!
//! # 与框架的接缝
//!
//! 框架的 `FunctionRef` 要求 handler 是 `Fn(Value) -> BoxToolFuture`，
//! **不带 `PluginContext` 参数**。而规格要求 handler 签名是
//! `async fn(ctx, args) -> Result<ToolResult, ToolError>`。
//!
//! [`erase_handler`] 就是这两者的桥：它把 `ctx` 捕获进闭包，对框架呈现为
//! 一个只要 `Value` 的函数。`ctx` 是廉价克隆（内部全是 `Arc` / `PathBuf`）。
//!
//! # 为什么 `trust_level` / `unit_handle` 由调用方填
//!
//! 它们**不是**插件的声明，而是加载器验签后的结论（可信 / 沙箱）与调度器分配的
//! 执行单元。插件无权自封。因此 [`ToolSet::into_metas`] 要求显式传入，
//! 而不是从 `plugin.json` 的 `trust` 字段读取 —— 那会把"声明"当"结论"。

use std::future::Future;
use std::sync::Arc;

use launcher_plugin_framework::types::{
    BoxToolFuture, FunctionRef, PluginId, ToolMeta, TrustLevel, UnitHandle,
};
use serde_json::Value;

use crate::context::PluginContext;
use crate::schema::ToolArgs;

/// handler 的函数签名（规格指定）。
///
/// 注意 `'static` 的来源：`ctx` 是按值捕获的克隆，`args` 是解析出的拥有数据的结构体。
pub type HandlerFn<A> = Arc<dyn Fn(PluginContext, A) -> BoxHandlerFuture + Send + Sync + 'static>;

pub type BoxHandlerFuture =
    std::pin::Pin<Box<dyn Future<Output = crate::result::PluginResult> + Send>>;

/// 一个工具的完整定义（由 `tool_def!` 宏产生）。
#[derive(Clone)]
pub struct ToolDef {
    /// 短名（不含插件前缀）。
    pub short_name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub destructive: bool,
    pub timeout_ms: u64,
    /// 类型擦除后的 handler（内部已完成 args 解析）。
    pub handler: ErasedHandler,
}

impl std::fmt::Debug for ToolDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolDef")
            .field("short_name", &self.short_name)
            .field("destructive", &self.destructive)
            .field("timeout_ms", &self.timeout_ms)
            .finish_non_exhaustive()
    }
}

/// 擦除参数类型后的 handler：`(ctx, raw_args) -> PluginResult`。
pub type ErasedHandler =
    Arc<dyn Fn(PluginContext, Value) -> BoxHandlerFuture + Send + Sync + 'static>;

/// 把 `Fn(ctx, A) -> impl Future<Output = PluginResult>` 擦除成 [`ErasedHandler`]。
///
/// 它负责两件规格要求的事：
///
/// 1. **参数解析**：`A::from_value(args)` 用 schema 同源的事实校验入参，
///    失败返回 `InvalidArguments`（框架码，不是业务码）。
/// 2. **结果归一**：把 `Result<ToolResult, ToolError>` 收敛为框架的 `ToolResult`。
///
/// 因为这两件都发生在这里，插件作者写 handler 时只需要关心业务。
pub fn erase_handler<A, F, Fut>(f: F) -> ErasedHandler
where
    A: ToolArgs + Send + 'static,
    F: Fn(PluginContext, A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = crate::result::PluginResult> + Send + 'static,
{
    Arc::new(move |ctx: PluginContext, raw: Value| {
        // 解析在**进入 handler 之前**完成：非法参数不会执行任何业务逻辑。
        // 这符合规格 Q5（参数校验在状态检查之后、执行之前）。
        let parsed = match A::from_value(&raw) {
            Ok(v) => v,
            Err(msg) => {
                let e = ctx.err(
                    launcher_plugin_framework::error::ToolErrorCode::InvalidArguments,
                    msg,
                );
                return Box::pin(async move { Err(e) }) as BoxHandlerFuture;
            }
        };
        let fut = f(ctx, parsed);
        Box::pin(fut) as BoxHandlerFuture
    })
}

/// 一个插件的全部工具。
pub struct ToolSet {
    pub plugin_id: PluginId,
    pub tools: Vec<ToolDef>,
}

impl ToolSet {
    pub fn new(plugin_id: impl Into<PluginId>, tools: Vec<ToolDef>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            tools,
        }
    }

    /// 导出短名列表，供 `manifest.check_tools` 做双向一致性校验。
    pub fn manifest_tools(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.short_name).collect()
    }

    /// 生成框架的 `ToolMeta`。
    ///
    /// `trust` 与 `unit` 来自加载器/调度器，**不是**插件的自我声明。
    pub fn into_metas(
        &self,
        ctx: &PluginContext,
        trust: TrustLevel,
        unit: Option<UnitHandle>,
        allowed_callers: Vec<PluginId>,
    ) -> Vec<ToolMeta> {
        self.tools
            .iter()
            .map(|def| {
                let full_name = format!("{}.{}", self.plugin_id, def.short_name);
                // 把 ctx + ErasedHandler 包成框架要的 `Fn(Value) -> BoxToolFuture`。
                let handler_ctx = ctx.clone();
                let handler = Arc::clone(&def.handler);
                let fr = FunctionRef::new(
                    self.plugin_id.clone(),
                    full_name.clone(),
                    move |args: Value| -> BoxToolFuture {
                        let ctx = handler_ctx.clone();
                        let h = Arc::clone(&handler);
                        Box::pin(
                            async move { crate::result::into_framework_result(h(ctx, args).await) },
                        )
                    },
                );
                ToolMeta {
                    name: full_name,
                    description: def.description.to_owned(),
                    input_schema: def.input_schema.clone(),
                    handler: fr,
                    trust_level: trust,
                    allowed_callers: allowed_callers.clone(),
                    destructive: def.destructive,
                    timeout_ms: def.timeout_ms,
                    owner_plugin_id: self.plugin_id.clone(),
                    unit_handle: unit.clone(),
                }
            })
            .collect()
    }

    /// 校验工具集合自身是否自洽（重名 / 空 / 描述缺失）。
    ///
    /// 这是"代码侧"的自检；"代码 vs 清单"的比对由 `manifest.check_tools` 负责。
    pub fn validate(&self) -> Result<(), String> {
        if self.tools.is_empty() {
            return Err("ToolSet 为空".to_owned());
        }
        let mut seen = std::collections::BTreeSet::new();
        for t in &self.tools {
            crate::manifest::validate_tool_name(t.short_name).map_err(|e| e.to_string())?;
            if !seen.insert(t.short_name) {
                return Err(format!("工具短名重复：{}", t.short_name));
            }
            if t.description.is_empty() {
                return Err(format!("工具 {} 缺少 description", t.short_name));
            }
            if t.timeout_ms == 0 {
                return Err(format!("工具 {} 的 timeout_ms 必须 > 0", t.short_name));
            }
            // 生成的 schema 必须在框架支持的子集内（fail-closed 提前暴露）。
            launcher_plugin_framework::json::validate_schema(&t.input_schema)
                .map_err(|e| format!("工具 {} 的 schema 非法：{e}", t.short_name))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 同 crate 内的 `#[macro_export]` 宏需要显式引入才可用。
    use crate::{tool_args, tool_def};
    use serde_json::json;

    tool_args! {
        pub struct EchoArgs {
            message: r_str() = "要回显的内容",
        }
    }

    async fn echo_handler(_ctx: PluginContext, args: EchoArgs) -> crate::result::PluginResult {
        Ok(crate::result::success(json!({"echo": args.message })))
    }

    fn make_set() -> ToolSet {
        let def = tool_def! {
            name: "echo",
            description: "回显",
            destructive: false,
            timeout_ms: 5_000,
            args: EchoArgs,
            handler: echo_handler
        };
        ToolSet::new("com.example.demo", vec![def])
    }

    #[tokio::test]
    async fn erased_handler_parses_and_calls() {
        let ctx = PluginContext::for_test("com.example.demo");
        let set = make_set();
        let h = Arc::clone(&set.tools[0].handler);
        // ErasedHandler 返回 PluginResult，测试里先收敛成框架形状再断言。
        let out = crate::result::into_framework_result(h(ctx, json!({"message": "hi"})).await);
        assert!(!out.is_error);
        assert_eq!(out.structured_content.unwrap()["echo"], "hi");
    }

    #[tokio::test]
    async fn erased_handler_rejects_bad_args_before_business_logic() {
        let ctx = PluginContext::for_test("com.example.demo");
        let set = make_set();
        let h = Arc::clone(&set.tools[0].handler);
        let out = crate::result::into_framework_result(h(ctx, json!({})).await);
        assert!(out.is_error);
        assert_eq!(
            out.structured_content.as_ref().unwrap()["code"],
            "InvalidArguments"
        );
    }

    #[test]
    fn macros_produce_subset_valid_schema() {
        let set = make_set();
        set.validate().expect("工具集合应当自洽");
    }

    #[test]
    fn into_metas_uses_given_trust_and_unit() {
        let ctx = PluginContext::for_test("com.example.demo");
        let set = make_set();
        let metas = set.into_metas(&ctx, TrustLevel::Trusted, None, vec![PluginId::new("*")]);
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].name, "com.example.demo.echo");
        assert!(metas[0].trust_level.is_trusted());
        // 框架的元数据自洽性检查必须通过
        metas[0]
            .validate()
            .expect("生成的 ToolMeta 必须通过框架校验");
    }

    #[test]
    fn sandboxed_metas_require_unit() {
        let ctx = PluginContext::for_test("com.example.demo");
        let set = make_set();
        let unit = UnitHandle::new(PluginId::new("com.example.demo"), 1);
        let metas = set.into_metas(
            &ctx,
            TrustLevel::Sandboxed,
            Some(unit),
            vec![PluginId::new("*")],
        );
        metas[0].validate().unwrap();
    }

    #[test]
    fn manifest_tools_exports_short_names() {
        let set = make_set();
        assert_eq!(set.manifest_tools(), vec!["echo"]);
    }

    #[test]
    fn validate_catches_duplicates() {
        let d = tool_def! {
            name: "echo",
            description: "回显",
            destructive: false,
            timeout_ms: 5_000,
            args: EchoArgs,
            handler: echo_handler
        };
        let set = ToolSet::new("com.example.demo", vec![d.clone(), d]);
        let err = set.validate().unwrap_err();
        assert!(err.contains("重复"), "{err}");
    }
}
