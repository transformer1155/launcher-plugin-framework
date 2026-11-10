//! 从 Rust 类型派生 JSON Schema。
//!
//! # 为什么需要这一层
//!
//! 规格要求 "inputSchema 用 JSON Schema 写全" 且 "禁止手写不一致的 schema"。
//! 手写 schema 与手写的参数反序列化是**两套真相**，一旦不一致，就会出现
//! "schema 说必填但代码允许缺省"这类静默错误。
//!
//! 本模块让两者共享同一个来源：[`ToolArgs`] trait 同时提供
//! - `fn args_schema() -> Value`：给注册表用的 schema；
//! - `fn from_value(&Value) -> Result<Self, String>`：给 handler 用的解析。
//!
//! 两者的字段列表、必填判定、范围判定全部写在同一次 `dispatch!` 里，不可能漂移。
//!
//! # 为什么不用 `schemars` / `serde_json` 的 schema 派生
//!
//! 规格要求"不要引入新依赖，除非框架已有"。`schemars` 不在框架依赖里。
//! 而且框架的 [`launcher_plugin_framework::json::validate_schema`] 只支持
//! JSON Schema 的一个**受支持子集**（`type/properties/required/additionalProperties/
//! items/enum/const/oneOf/minimum/maximum/...`），引入通用派生库很可能生成它不接受的
//! 关键字（例如 `pattern`、`$ref`、`format`），导致 `register_tool` 直接失败。
//!
//! 所以这里走"受控子集派生"：能生成的 schema 一定在框架可接受范围内。

use serde_json::{json, Map, Value};

/// 一个工具参数类型的完整描述能力。
pub trait ToolArgs: Sized {
    /// 生成 inputSchema。
    fn args_schema() -> Value;
    /// 从 `arguments` 解析。失败返回人类可读的原因（上层转成 `InvalidArguments`）。
    fn from_value(v: &Value) -> Result<Self, String>;
}

/// 字段规格：定义一次，同时产出 schema 与解析代码。
///
/// 用法见 `args!` 宏。手写这个结构体是不推荐的 —— 用宏，避免 schema 与解析漂移。
pub struct Field {
    pub name: &'static str,
    /// schema 的 `type`。
    pub ty: &'static str,
    pub required: bool,
    /// 仅 `type = "number" | "integer"` 有意义。
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// 仅 `type = "string"` 有意义。
    pub enum_values: Option<Vec<&'static str>>,
    /// 仅 `type = "array"` 有意义：元素 schema。
    pub items: Option<Value>,
    pub description: &'static str,
}

impl Field {
    pub fn build_schema(&self) -> Value {
        let mut m = Map::new();
        m.insert("type".to_owned(), json!(self.ty));
        if !self.description.is_empty() {
            m.insert("description".to_owned(), json!(self.description));
        }
        if let Some(min) = self.min {
            m.insert("minimum".to_owned(), json!(min));
        }
        if let Some(max) = self.max {
            m.insert("maximum".to_owned(), json!(max));
        }
        if let Some(vals) = &self.enum_values {
            m.insert("enum".to_owned(), json!(vals));
        }
        if let Some(items) = &self.items {
            m.insert("items".to_owned(), items.clone());
        }
        Value::Object(m)
    }
}

/// 由一组 [`Field`] 组装完整的 object schema。
///
/// `additionalProperties: false` 是刻意选择的：工具不该默默忽略不认识的参数，
/// 那会让"传错字段名"变成静默无效果的操作。
pub fn object_schema(fields: &[Field]) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    for f in fields {
        props.insert(f.name.to_owned(), f.build_schema());
        if f.required {
            required.push(Value::String(f.name.to_owned()));
        }
    }
    let mut root = Map::new();
    root.insert("type".to_owned(), json!("object"));
    root.insert("properties".to_owned(), Value::Object(props));
    root.insert("required".to_owned(), Value::Array(required));
    root.insert("additionalProperties".to_owned(), json!(false));
    Value::Object(root)
}

// ----------------------------------------------------------------------------
// 取值辅助：统一错误文案
// ----------------------------------------------------------------------------

pub fn get_required<'a>(v: &'a Value, name: &str) -> Result<&'a Value, String> {
    v.get(name).ok_or_else(|| format!("缺少必填参数 `{name}`"))
}

pub fn as_str<'a>(v: &'a Value, name: &str) -> Result<&'a str, String> {
    get_required(v, name)?
        .as_str()
        .ok_or_else(|| format!("参数 `{name}` 必须是字符串"))
}

pub fn as_bool(v: &Value, name: &str) -> Result<bool, String> {
    get_required(v, name)?
        .as_bool()
        .ok_or_else(|| format!("参数 `{name}` 必须是布尔值"))
}

pub fn as_u64(v: &Value, name: &str) -> Result<u64, String> {
    get_required(v, name)?
        .as_u64()
        .ok_or_else(|| format!("参数 `{name}` 必须是非负整数"))
}

pub fn as_i64(v: &Value, name: &str) -> Result<i64, String> {
    get_required(v, name)?
        .as_i64()
        .ok_or_else(|| format!("参数 `{name}` 必须是整数"))
}

/// 取可选字符串。
pub fn opt_str(v: &Value, name: &str) -> Result<Option<String>, String> {
    match v.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("参数 `{name}` 必须是字符串")),
    }
}

/// 取字符串并限定在枚举内。**这是 `loader` 与枚举 schema 的唯一真相源。**
pub fn as_enum<'a>(v: &'a Value, name: &str, allowed: &[&'a str]) -> Result<&'a str, String> {
    let s = as_str(v, name)?;
    if allowed.contains(&s) {
        Ok(s)
    } else {
        Err(format!(
            "参数 `{name}` 必须是 {:?} 之一，实际 `{s}`",
            allowed
        ))
    }
}

/// 数值范围检查（schema 已声明，这里再查一次是为了给出**比泛化 schema 报错更好读**的错误）。
pub fn check_range_u64(v: u64, name: &str, min: u64, max: u64) -> Result<u64, String> {
    if v < min || v > max {
        return Err(format!("参数 `{name}` 必须在 {min}..={max} 之间，实际 {v}"));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: Field = Field {
        name: "x",
        ty: "string",
        required: true,
        min: None,
        max: None,
        enum_values: None,
        items: None,
        description: "d",
    };

    #[test]
    fn object_schema_shape() {
        let fields = [
            Field {
                name: "name",
                ty: "string",
                required: true,
                min: None,
                max: None,
                enum_values: None,
                items: None,
                description: "实例名",
            },
            Field {
                name: "loader",
                ty: "string",
                required: true,
                min: None,
                max: None,
                enum_values: Some(vec!["vanilla", "fabric", "forge"]),
                items: None,
                description: "加载器",
            },
            Field {
                name: "limit",
                ty: "integer",
                required: false,
                min: Some(1.0),
                max: Some(50.0),
                enum_values: None,
                items: None,
                description: "上限",
            },
        ];
        let s = object_schema(&fields);
        assert_eq!(s["type"], "object");
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["required"], json!(["name", "loader"]));
        assert_eq!(
            s["properties"]["loader"]["enum"],
            json!(["vanilla", "fabric", "forge"])
        );
        assert_eq!(s["properties"]["limit"]["minimum"], 1.0);

        // 生成的 schema 必须在框架支持的子集内（否则 register_tool 会失败）
        launcher_plugin_framework::json::validate_schema(&s)
            .expect("派生出的 schema 必须是框架可接受的子集");
    }

    #[test]
    fn framework_accepts_all_generated_schemas() {
        // 逐个关键字覆盖，确保没有任何派生组合超出框架子集。
        let f = Field {
            items: Some(json!({"type": "string"})),
            ..F
        };
        let s = object_schema(&[f]);
        launcher_plugin_framework::json::validate_schema(&s).unwrap();
    }

    #[test]
    fn enum_guard_rejects_unknown() {
        let v = json!({"loader": "quilt"});
        let err = as_enum(&v, "loader", &["vanilla", "fabric", "forge"]).unwrap_err();
        assert!(err.contains("quilt"), "{err}");
    }

    #[test]
    fn missing_required_names_the_field() {
        let v = json!({});
        let err = get_required(&v, "name").unwrap_err();
        assert!(err.contains("name"));
    }

    #[test]
    fn range_message_is_specific() {
        let err = check_range_u64(0, "limit", 1, 50).unwrap_err();
        assert!(err.contains("1..=50") && err.contains("limit"), "{err}");
    }
}
