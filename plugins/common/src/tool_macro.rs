//! 声明式工具注册宏：**schema 与参数解析的唯一真相源**。
//!
//! # 它解决的问题
//!
//! 一个工具需要同时满足五处约束，手写时极易漂移：
//!
//! 1. `plugin.json` 的 `tools` 列表（短名）
//! 2. 注册进 `ToolMeta` 的**全名**（`插件ID.短名`）
//! 3. `inputSchema`（JSON Schema）
//! 4. handler 里的参数解析（必须与 schema 一致）
//! 5. `destructive` / `timeout_ms` 等元数据
//!
//! [`tool_args!`] 让 3 与 4 由**一次**声明产生：`from_value()` 先调用 `crate::json::validate`
//! 用**同一份** schema 校验入参，再做类型转换。因此 schema 与解析在语义上不可能不一致 ——
//! 解析永远不会接受 schema 拒绝的输入，也不会拒绝 schema 接受的输入。
//!
//! [`tool!`] 让 2 与 5 由一次声明产生；第 1 条由 [`crate::registry::ToolSet::manifest_tools`]
//! 从同一批 `ToolDef` 导出，再由 `manifest.check_tools` **双向**比对。
//!
//! # 语法
//!
//! ```ignore
//! tool_args! {
//!     /// 删除实例的参数
//!     pub struct DeleteArgs {
//!         instance_id: r_str()                  = "实例 ID",
//!         confirm:     r_bool()                 = "确认删除，必须为 true",
//!     }
//! }
//!
//! tool_args! {
//!     pub struct SearchArgs {
//!         query:       r_str()                       = "搜索关键词",
//!         loader:      r_enum["vanilla","fabric"]    = "加载器",
//!         limit:       o_u64(1, 50)                  = "返回条数，默认 20",
//!     }
//! }
//! ```
//!
//! 每个字段的形状是 `name: kind TAIL = "描述"`，其中 `TAIL` 是**单个 token tree**：
//!
//! | TAIL 形状 | 含义 | 生成的 schema |
//! |---|---|---|
//! | `()` | 无额外约束 | 只有 `type` |
//! | `(min, max)` | 数值范围 | `minimum` / `maximum` |
//! | `["a","b"]` | 枚举值 | `enum` |
//!
//! `kind` 词表（`r_` = 必填，`o_` = 可选）：
//!
//! | kind | Rust 类型 | schema type |
//! |---|---|---|
//! | `r_str` / `o_str` | `String` / `Option<String>` | string |
//! | `r_bool` / `o_bool` | `bool` / `Option<bool>` | boolean |
//! | `r_u64` / `o_u64` | `u64` / `Option<u64>` | integer |
//! | `r_i64` / `o_i64` | `i64` / `Option<i64>` | integer |
//! | `r_f64` / `o_f64` | `f64` / `Option<f64>` | number |
//! | `r_enum` / `o_enum` | `String` / `Option<String>` | string + enum |
//!
//! # 为什么用单词 kind 而不是 `String` / `Option<String>`
//!
//! `macro_rules!` 无法可靠地把 `Option<String>` 这类复合类型再拆回 schema 类型。
//! 受控的 kind 词表能保证**生成出来的 schema 一定落在框架支持的子集内**：
//! 框架的 `json::validate_schema` 是 fail-closed 的，生成超出子集的 schema 会让
//! `register_tool` 直接失败。

/// kind → Rust 字段类型。
#[macro_export]
macro_rules! arg_ty {
    (r_str) => { ::std::string::String };
    (o_str) => { ::core::option::Option<::std::string::String> };
    (r_bool) => { bool };
    (o_bool) => { ::core::option::Option<bool> };
    (r_u64) => { u64 };
    (o_u64) => { ::core::option::Option<u64> };
    (r_i64) => { i64 };
    (o_i64) => { ::core::option::Option<i64> };
    (r_f64) => { f64 };
    (o_f64) => { ::core::option::Option<f64> };
    (r_enum) => { ::std::string::String };
    (o_enum) => { ::core::option::Option<::std::string::String> };
}

/// kind + TAIL → `($json_ty, required, min, max, enum_values)` 的标准化元组。
///
/// 全部字段都归一化成同一个元组，`field_spec!` 与 `arg_parse!` 都消费它，
/// 从而两者共享同一份"必填性 / 类型 / 范围 / 枚举"事实。
#[macro_export]
macro_rules! arg_meta {
    // ---- 字符串 ----
    (r_str, ()) => { ("string", true, None::<f64>, None::<f64>, None::<Vec<&'static str>>) };
    (o_str, ()) => { ("string", false, None::<f64>, None::<f64>, None::<Vec<&'static str>>) };
    // ---- 布尔 ----
    (r_bool, ()) => { ("boolean", true, None::<f64>, None::<f64>, None::<Vec<&'static str>>) };
    (o_bool, ()) => { ("boolean", false, None::<f64>, None::<f64>, None::<Vec<&'static str>>) };
    // ---- 无范围整数 ----
    (r_u64, ()) => { ("integer", true, None::<f64>, None::<f64>, None::<Vec<&'static str>>) };
    (o_u64, ()) => { ("integer", false, None::<f64>, None::<f64>, None::<Vec<&'static str>>) };
    (r_i64, ()) => { ("integer", true, None::<f64>, None::<f64>, None::<Vec<&'static str>>) };
    (o_i64, ()) => { ("integer", false, None::<f64>, None::<f64>, None::<Vec<&'static str>>) };
    // ---- 带范围整数 ----
    (r_u64, ($min:literal, $max:literal)) => {
        ("integer", true, Some($min as f64), Some($max as f64), None::<Vec<&'static str>>)
    };
    (o_u64, ($min:literal, $max:literal)) => {
        ("integer", false, Some($min as f64), Some($max as f64), None::<Vec<&'static str>>)
    };
    (r_i64, ($min:literal, $max:literal)) => {
        ("integer", true, Some($min as f64), Some($max as f64), None::<Vec<&'static str>>)
    };
    (o_i64, ($min:literal, $max:literal)) => {
        ("integer", false, Some($min as f64), Some($max as f64), None::<Vec<&'static str>>)
    };
    // ---- 浮点 ----
    (r_f64, ()) => { ("number", true, None::<f64>, None::<f64>, None::<Vec<&'static str>>) };
    (o_f64, ()) => { ("number", false, None::<f64>, None::<f64>, None::<Vec<&'static str>>) };
    (r_f64, ($min:literal, $max:literal)) => {
        ("number", true, Some($min as f64), Some($max as f64), None::<Vec<&'static str>>)
    };
    (o_f64, ($min:literal, $max:literal)) => {
        ("number", false, Some($min as f64), Some($max as f64), None::<Vec<&'static str>>)
    };
    // ---- 枚举 ----
    (r_enum, [$($v:literal),+ $(,)?]) => {
        ("string", true, None::<f64>, None::<f64>, Some(vec![$($v),+]))
    };
    (o_enum, [$($v:literal),+ $(,)?]) => {
        ("string", false, None::<f64>, None::<f64>, Some(vec![$($v),+]))
    };
}

/// 生成一个 `Field`（schema 描述）。
#[macro_export]
macro_rules! field_spec {
    ($kind:ident, $name:ident, $tail:tt, $desc:literal) => {{
        let (ty, required, min, max, enum_values) = $crate::arg_meta!($kind, $tail);
        $crate::schema::Field {
            name: stringify!($name),
            ty,
            required,
            min,
            max,
            enum_values,
            items: None,
            description: $desc,
        }
    }};
}

/// 生成一个字段的解析表达式。
///
/// **先由 `crate::arg_parse::parse_field` 用 schema 同一份事实校验**，
/// 再按 kind 做类型转换。因此"schema 说必填"与"解析要求必填"不可能不一致。
#[macro_export]
macro_rules! arg_parse {
    ($v:expr, $kind:ident, $name:ident, $tail:tt) => {{
        let (ty, required, min, max, enum_values) = $crate::arg_meta!($kind, $tail);
        $crate::arg_parse::read::<$crate::arg_ty!($kind)>(
            $v,
            stringify!($name),
            ty,
            required,
            min,
            max,
            enum_values.as_deref(),
        )?
    }};
}

/// 声明参数结构体：同时生成 schema 与解析。
#[macro_export]
macro_rules! tool_args {
    (
        $(#[$smeta:meta])*
        pub struct $ty:ident {
            $(
                $fname:ident : $kind:ident $tail:tt = $desc:literal
            ),* $(,)?
        }
    ) => {
        $(#[$smeta])*
        #[derive(Debug, Clone)]
        pub struct $ty {
            $( pub $fname: $crate::arg_ty!($kind), )*
        }

        impl $crate::schema::ToolArgs for $ty {
            fn args_schema() -> ::serde_json::Value {
                let fields = [ $( $crate::field_spec!($kind, $fname, $tail, $desc) ),* ];
                $crate::schema::object_schema(&fields)
            }

            fn from_value(
                v: &::serde_json::Value,
            ) -> ::core::result::Result<Self, ::std::string::String> {
                // 第一步：用**同一份** schema 校验。schema 与解析因此不会漂移。
                let schema = <Self as $crate::schema::ToolArgs>::args_schema();
                if let Err(e) = $crate::json::validate(v, &schema) {
                    return ::core::result::Result::Err(e.to_string());
                }
                // 第二步：schema 已保证类型正确，这里只做取值。
                ::core::result::Result::Ok(Self {
                    $( $fname: $crate::arg_parse!(v, $kind, $fname, $tail), )*
                })
            }
        }
    };
}

/// 声明一个工具定义。
#[macro_export]
macro_rules! tool_def {
    (
        name: $name:literal,
        description: $desc:literal,
        destructive: $destructive:literal,
        timeout_ms: $timeout:literal,
        args: $args:ty,
        handler: $handler:expr
    ) => {
        $crate::registry::ToolDef {
            short_name: $name,
            description: $desc,
            input_schema: <$args as $crate::schema::ToolArgs>::args_schema(),
            destructive: $destructive,
            timeout_ms: $timeout,
            handler: $crate::registry::erase_handler::<$args, _, _>($handler),
        }
    };
}
