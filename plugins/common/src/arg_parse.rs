//! 参数读取：把 `serde_json::Value` 安全地转成 Rust 类型，错误文案可读。
//!
//! 这里的 [`read`] 是宏生成代码的唯一取值入口。它接收**与 schema 同源的元数据**
//! （`ty / required / min / max / enum_values`），因此不会出现"schema 允许但解析拒绝"。

use serde_json::Value;

/// 从值里读取一个字段。
///
/// # 契约
///
/// 调用前 `crate::json::validate(v, schema)` 已经通过，因此本函数：
/// - 不会遇到缺失的必填字段（但仍做防御性检查，给出可读错误）；
/// - 不会遇到类型错误的字段（但仍做防御性检查）；
/// - `min` / `max` 由 schema 保证，这里的重复检查只为生成**更好读**的错误文案。
pub fn read<T>(
    v: &Value,
    name: &str,
    json_ty: &str,
    required: bool,
    min: Option<f64>,
    max: Option<f64>,
    enum_values: Option<&[&str]>,
) -> Result<T, String>
where
    T: FieldRead,
{
    <T as FieldRead>::read_field(v, name, json_ty, required, min, max, enum_values)
}

/// 字段读取的具体实现。为每种目标类型实现一次。
pub trait FieldRead: Sized {
    fn read_field(
        v: &Value,
        name: &str,
        json_ty: &str,
        required: bool,
        min: Option<f64>,
        max: Option<f64>,
        enum_values: Option<&[&str]>,
    ) -> Result<Self, String>;
}

fn missing(name: &str) -> String {
    format!("缺少必填参数 `{name}`")
}

fn wrong_type(name: &str, want: &str, got: &Value) -> String {
    format!(
        "参数 `{name}` 类型错误：期望 {want}，实际 {}",
        json_type_name(got)
    )
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 枚举校验：给出"允许哪些"的错误文案。
///
/// 即便 schema 已经用 `enum` 拦过，这里仍单独检查 —— 因为宏生成的结构体
/// 可能被 `from_value` 之外的代码直接构造，多一道检查不会有害。
fn check_enum(s: &str, name: &str, allowed: Option<&[&str]>) -> Result<(), String> {
    if let Some(allowed) = allowed {
        if !allowed.contains(&s) {
            return Err(format!(
                "参数 `{name}` 必须是 {:?} 之一，实际 `{s}`",
                allowed
            ));
        }
    }
    Ok(())
}

// ---- String ----

impl FieldRead for String {
    fn read_field(
        v: &Value,
        name: &str,
        _json_ty: &str,
        required: bool,
        _min: Option<f64>,
        _max: Option<f64>,
        enum_values: Option<&[&str]>,
    ) -> Result<Self, String> {
        match v.get(name) {
            None | Some(Value::Null) => {
                if required {
                    Err(missing(name))
                } else {
                    Ok(String::new())
                }
            }
            Some(Value::String(s)) => {
                check_enum(s, name, enum_values)?;
                Ok(s.clone())
            }
            Some(other) => Err(wrong_type(name, "string", other)),
        }
    }
}

// ---- Option<String> ----

impl FieldRead for Option<String> {
    fn read_field(
        v: &Value,
        name: &str,
        _json_ty: &str,
        _required: bool,
        _min: Option<f64>,
        _max: Option<f64>,
        enum_values: Option<&[&str]>,
    ) -> Result<Self, String> {
        match v.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => {
                check_enum(s, name, enum_values)?;
                Ok(Some(s.clone()))
            }
            Some(other) => Err(wrong_type(name, "string", other)),
        }
    }
}

// ---- bool ----

impl FieldRead for bool {
    fn read_field(
        v: &Value,
        name: &str,
        _json_ty: &str,
        required: bool,
        _min: Option<f64>,
        _max: Option<f64>,
        _enum_values: Option<&[&str]>,
    ) -> Result<Self, String> {
        match v.get(name) {
            None | Some(Value::Null) => {
                if required {
                    Err(missing(name))
                } else {
                    Ok(false)
                }
            }
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(wrong_type(name, "boolean", other)),
        }
    }
}

impl FieldRead for Option<bool> {
    fn read_field(
        v: &Value,
        name: &str,
        _json_ty: &str,
        _required: bool,
        _min: Option<f64>,
        _max: Option<f64>,
        _enum_values: Option<&[&str]>,
    ) -> Result<Self, String> {
        match v.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(other) => Err(wrong_type(name, "boolean", other)),
        }
    }
}

// ---- 整数 ----

fn check_range(n: f64, name: &str, min: Option<f64>, max: Option<f64>) -> Result<(), String> {
    if let Some(min) = min {
        if n < min {
            return Err(format!("参数 `{name}` 不能小于 {min}，实际 {n}"));
        }
    }
    if let Some(max) = max {
        if n > max {
            return Err(format!("参数 `{name}` 不能大于 {max}，实际 {n}"));
        }
    }
    Ok(())
}

impl FieldRead for u64 {
    fn read_field(
        v: &Value,
        name: &str,
        _json_ty: &str,
        required: bool,
        min: Option<f64>,
        max: Option<f64>,
        _enum_values: Option<&[&str]>,
    ) -> Result<Self, String> {
        match v.get(name) {
            None | Some(Value::Null) => {
                if required {
                    Err(missing(name))
                } else {
                    Ok(0)
                }
            }
            Some(Value::Number(n)) => {
                let raw = n
                    .as_u64()
                    .ok_or_else(|| format!("参数 `{name}` 必须是非负整数，实际 {n}"))?;
                check_range(raw as f64, name, min, max)?;
                Ok(raw)
            }
            Some(other) => Err(wrong_type(name, "integer", other)),
        }
    }
}

impl FieldRead for Option<u64> {
    fn read_field(
        v: &Value,
        name: &str,
        _json_ty: &str,
        _required: bool,
        min: Option<f64>,
        max: Option<f64>,
        _enum_values: Option<&[&str]>,
    ) -> Result<Self, String> {
        match v.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => {
                let raw = n
                    .as_u64()
                    .ok_or_else(|| format!("参数 `{name}` 必须是非负整数，实际 {n}"))?;
                check_range(raw as f64, name, min, max)?;
                Ok(Some(raw))
            }
            Some(other) => Err(wrong_type(name, "integer", other)),
        }
    }
}

impl FieldRead for i64 {
    fn read_field(
        v: &Value,
        name: &str,
        _json_ty: &str,
        required: bool,
        min: Option<f64>,
        max: Option<f64>,
        _enum_values: Option<&[&str]>,
    ) -> Result<Self, String> {
        match v.get(name) {
            None | Some(Value::Null) => {
                if required {
                    Err(missing(name))
                } else {
                    Ok(0)
                }
            }
            Some(Value::Number(n)) => {
                let raw = n
                    .as_i64()
                    .ok_or_else(|| format!("参数 `{name}` 必须是整数，实际 {n}"))?;
                check_range(raw as f64, name, min, max)?;
                Ok(raw)
            }
            Some(other) => Err(wrong_type(name, "integer", other)),
        }
    }
}

impl FieldRead for Option<i64> {
    fn read_field(
        v: &Value,
        name: &str,
        _json_ty: &str,
        _required: bool,
        min: Option<f64>,
        max: Option<f64>,
        _enum_values: Option<&[&str]>,
    ) -> Result<Self, String> {
        match v.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => {
                let raw = n
                    .as_i64()
                    .ok_or_else(|| format!("参数 `{name}` 必须是整数，实际 {n}"))?;
                check_range(raw as f64, name, min, max)?;
                Ok(Some(raw))
            }
            Some(other) => Err(wrong_type(name, "integer", other)),
        }
    }
}

// ---- 浮点 ----

impl FieldRead for f64 {
    fn read_field(
        v: &Value,
        name: &str,
        _json_ty: &str,
        required: bool,
        min: Option<f64>,
        max: Option<f64>,
        _enum_values: Option<&[&str]>,
    ) -> Result<Self, String> {
        match v.get(name) {
            None | Some(Value::Null) => {
                if required {
                    Err(missing(name))
                } else {
                    Ok(0.0)
                }
            }
            Some(Value::Number(n)) => {
                let raw = n
                    .as_f64()
                    .ok_or_else(|| format!("参数 `{name}` 必须是数值，实际 {n}"))?;
                check_range(raw, name, min, max)?;
                Ok(raw)
            }
            Some(other) => Err(wrong_type(name, "number", other)),
        }
    }
}

impl FieldRead for Option<f64> {
    fn read_field(
        v: &Value,
        name: &str,
        _json_ty: &str,
        _required: bool,
        min: Option<f64>,
        max: Option<f64>,
        _enum_values: Option<&[&str]>,
    ) -> Result<Self, String> {
        match v.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => {
                let raw = n
                    .as_f64()
                    .ok_or_else(|| format!("参数 `{name}` 必须是数值，实际 {n}"))?;
                check_range(raw, name, min, max)?;
                Ok(Some(raw))
            }
            Some(other) => Err(wrong_type(name, "number", other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_string_missing() {
        let v = json!({});
        let err = read::<String>(&v, "name", "string", true, None, None, None).unwrap_err();
        assert!(err.contains("name") && err.contains("必填"), "{err}");
    }

    #[test]
    fn required_string_wrong_type() {
        let v = json!({"name": 1});
        let err = read::<String>(&v, "name", "string", true, None, None, None).unwrap_err();
        assert!(err.contains("类型错误"), "{err}");
    }

    #[test]
    fn enum_violation_lists_allowed() {
        let v = json!({"loader": "quilt"});
        let err = read::<String>(
            &v,
            "loader",
            "string",
            true,
            None,
            None,
            Some(&["vanilla", "fabric", "forge"]),
        )
        .unwrap_err();
        assert!(err.contains("quilt") && err.contains("vanilla"), "{err}");
    }

    #[test]
    fn u64_range_checked() {
        let v = json!({"limit": 51});
        let err =
            read::<u64>(&v, "limit", "integer", true, Some(1.0), Some(50.0), None).unwrap_err();
        assert!(err.contains("50"), "{err}");

        let ok = read::<u64>(
            &json!({"limit": 20}),
            "limit",
            "integer",
            true,
            Some(1.0),
            Some(50.0),
            None,
        );
        assert_eq!(ok.unwrap(), 20);
    }

    #[test]
    fn negative_u64_rejected() {
        let v = json!({"limit": -1});
        let err = read::<u64>(&v, "limit", "integer", true, None, None, None).unwrap_err();
        assert!(err.contains("非负"), "{err}");
    }

    #[test]
    fn option_absent_is_none() {
        let v = json!({});
        let got = read::<Option<String>>(&v, "x", "string", false, None, None, None).unwrap();
        assert_eq!(got, None);
        let got = read::<Option<u64>>(&v, "y", "integer", false, None, None, None).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn bool_read() {
        assert!(read::<bool>(&json!({"b": true}), "b", "boolean", true, None, None, None).unwrap());
        let err =
            read::<bool>(&json!({"b": "yes"}), "b", "boolean", true, None, None, None).unwrap_err();
        assert!(err.contains("boolean"), "{err}");
    }
}
