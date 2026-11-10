//! 极简 JSON Schema 校验器。
//!
//! 只支持工具 `input_schema` 实际会用到的子集：`type` / `properties` / `required` /
//! `additionalProperties` / `items` / `enum` / `const` / `oneOf`。
//!
//! # 为什么自己写 20 行而不是引入 jsonschema crate
//!
//! 规格要求"不要引入未在规格中出现的依赖抽象"。完整 JSON Schema 实现（含
//! `$ref`、`dependencies`、`patternProperties`、`format`）会把框架的依赖面显著放大，
//! 而工具入参校验不需要它们。这里的实现**故意是保守的**：schema 里出现未支持的
//! 关键字时，直接拒绝该 schema 而不是放过参数——fail-closed。

use serde_json::Value;

/// 校验失败原因。调用方把它转成 `ToolErrorCode::InvalidArguments`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError {
    /// JSON Pointer 风格路径，例如 `/foo/0/bar`。
    pub instance_path: String,
    pub message: String,
}

impl SchemaError {
    fn new(path: &str, message: impl Into<String>) -> Self {
        Self {
            instance_path: path.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.instance_path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}（位置 {}）", self.message, self.instance_path)
        }
    }
}

/// 校验 schema 自身是否只用了受支持的子集。在 `register_tool` 时调用一次。
pub fn validate_schema(schema: &Value) -> Result<(), SchemaError> {
    validate_schema_at(schema, "")
}

const SUPPORTED_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "oneOf",
    "anyOf",
    "description",
    "title",
    "default",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    // "$schema" 允许出现在根部，仅作声明，不参与校验。
    "$schema",
    // 常见但纯注释性的键，放过不影响正确性。
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
];

fn validate_schema_at(schema: &Value, path: &str) -> Result<(), SchemaError> {
    let obj = match schema {
        Value::Object(o) => o,
        Value::Bool(_) => return Ok(()), // true/false schema，受支持
        _ => return Err(SchemaError::new(path, "schema 节点必须是对象或布尔值")),
    };

    for key in obj.keys() {
        if !SUPPORTED_KEYWORDS.contains(&key.as_str()) {
            return Err(SchemaError::new(
                path,
                format!("不支持的 schema 关键字 `{key}`（本实现 fail-closed 拒绝）"),
            ));
        }
    }

    if let Some(props) = obj.get("properties") {
        let props = props
            .as_object()
            .ok_or_else(|| SchemaError::new(path, "properties 必须是对象"))?;
        for (name, sub) in props {
            validate_schema_at(sub, &format!("{path}/properties/{name}"))?;
        }
    }
    if let Some(items) = obj.get("items") {
        validate_schema_at(items, &format!("{path}/items"))?;
    }
    for comb in ["oneOf", "anyOf"] {
        if let Some(arr) = obj.get(comb) {
            let arr = arr
                .as_array()
                .ok_or_else(|| SchemaError::new(path, format!("{comb} 必须是数组")))?;
            for (i, sub) in arr.iter().enumerate() {
                validate_schema_at(sub, &format!("{path}/{comb}/{i}"))?;
            }
        }
    }
    Ok(())
}

/// 用 schema 校验一个实例。
pub fn validate(instance: &Value, schema: &Value) -> Result<(), SchemaError> {
    validate_at(instance, schema, "")
}

fn validate_at(instance: &Value, schema: &Value, path: &str) -> Result<(), SchemaError> {
    match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => {
            return Err(SchemaError::new(
                path,
                "此位置不允许任何值（schema 为 false）",
            ))
        }
        Value::Object(_) => {}
        _ => return Err(SchemaError::new(path, "schema 节点非法")),
    }
    let obj = schema.as_object().expect("上面已确认是对象");

    // const / enum 优先：命中即通过其后的 type 检查（但仍做类型检查，见下）。
    if let Some(expected) = obj.get("const") {
        if instance != expected {
            return Err(SchemaError::new(
                path,
                format!("值必须等于 const {}", expected),
            ));
        }
    }
    if let Some(variants) = obj.get("enum") {
        let variants = variants
            .as_array()
            .ok_or_else(|| SchemaError::new(path, "enum 必须是数组"))?;
        if !variants.iter().any(|v| v == instance) {
            return Err(SchemaError::new(
                path,
                format!("值不在 enum 允许集合内：{instance}"),
            ));
        }
    }

    // oneOf / anyOf：递归尝试。
    if let Some(variants) = obj.get("oneOf") {
        let variants = variants.as_array().expect("validate_schema 已确认");
        let ok = variants
            .iter()
            .filter(|v| validate_at(instance, v, path).is_ok())
            .count();
        if ok != 1 {
            return Err(SchemaError::new(
                path,
                format!("必须恰好匹配 oneOf 中的一个分支，实际匹配 {ok} 个"),
            ));
        }
    }
    if let Some(variants) = obj.get("anyOf") {
        let variants = variants.as_array().expect("validate_schema 已确认");
        if !variants
            .iter()
            .any(|v| validate_at(instance, v, path).is_ok())
        {
            return Err(SchemaError::new(path, "不匹配 anyOf 中任何分支"));
        }
    }

    if let Some(ty) = obj.get("type") {
        check_type(instance, ty, path)?;
    }

    // 数值范围
    if let Some(n) = instance.as_f64() {
        if let Some(min) = obj.get("minimum").and_then(Value::as_f64) {
            if n < min {
                return Err(SchemaError::new(path, format!("值 {n} 小于 minimum {min}")));
            }
        }
        if let Some(max) = obj.get("maximum").and_then(Value::as_f64) {
            if n > max {
                return Err(SchemaError::new(path, format!("值 {n} 大于 maximum {max}")));
            }
        }
    }
    if let Some(s) = instance.as_str() {
        if let Some(min) = obj.get("minLength").and_then(Value::as_u64) {
            if (s.chars().count() as u64) < min {
                return Err(SchemaError::new(path, format!("字符串长度小于 {min}")));
            }
        }
        if let Some(max) = obj.get("maxLength").and_then(Value::as_u64) {
            if (s.chars().count() as u64) > max {
                return Err(SchemaError::new(path, format!("字符串长度大于 {max}")));
            }
        }
    }
    if let Some(arr) = instance.as_array() {
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
            if (arr.len() as u64) < min {
                return Err(SchemaError::new(path, format!("数组长度小于 {min}")));
            }
        }
        if let Some(max) = obj.get("maxItems").and_then(Value::as_u64) {
            if (arr.len() as u64) > max {
                return Err(SchemaError::new(path, format!("数组长度大于 {max}")));
            }
        }
        if let Some(items) = obj.get("items") {
            for (i, item) in arr.iter().enumerate() {
                validate_at(item, items, &format!("{path}/{i}"))?;
            }
        }
    }

    // 对象属性
    if let Some(map) = instance.as_object() {
        let props = obj.get("properties").and_then(Value::as_object);
        if let Some(required) = obj.get("required").and_then(Value::as_array) {
            for name in required {
                let name = name
                    .as_str()
                    .ok_or_else(|| SchemaError::new(path, "required 元素必须是字符串"))?;
                if !map.contains_key(name) {
                    return Err(SchemaError::new(path, format!("缺少必填字段 `{name}`")));
                }
            }
        }
        for (key, value) in map {
            match props.and_then(|p| p.get(key)) {
                Some(sub) => {
                    validate_at(value, sub, &format!("{path}/{key}"))?;
                }
                None => {
                    // additionalProperties 默认 true（JSON Schema 规范行为）。
                    match obj.get("additionalProperties") {
                        Some(Value::Bool(false)) => {
                            return Err(SchemaError::new(
                                path,
                                format!("不允许的额外字段 `{key}`"),
                            ));
                        }
                        Some(Value::Bool(true)) | None => {}
                        Some(sub) => {
                            validate_at(value, sub, &format!("{path}/{key}"))?;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn check_type(instance: &Value, ty: &Value, path: &str) -> Result<(), SchemaError> {
    // type 可以是字符串或字符串数组。
    let names: Vec<&str> = match ty {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => {
            let mut v = Vec::with_capacity(a.len());
            for x in a {
                v.push(
                    x.as_str()
                        .ok_or_else(|| SchemaError::new(path, "type 数组元素必须是字符串"))?,
                );
            }
            v
        }
        _ => return Err(SchemaError::new(path, "type 必须是字符串或字符串数组")),
    };

    let matches = names.iter().any(|name| match *name {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "number" => instance.is_number(),
        "integer" => instance.is_i64() || instance.is_u64(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        _ => false,
    });

    if matches {
        Ok(())
    } else {
        Err(SchemaError::new(
            path,
            format!(
                "类型不匹配，期望 {:?}，实际 {}",
                names,
                json_type_name(instance)
            ),
        ))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_valid_object() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "depth": {"type": "integer", "minimum": 0, "maximum": 8}
            },
            "required": ["path"],
            "additionalProperties": false
        });
        assert!(validate_schema(&schema).is_ok());
        assert!(validate(&json!({"path": "/tmp"}), &schema).is_ok());
        assert!(validate(&json!({"path": "/tmp", "depth": 3}), &schema).is_ok());
    }

    #[test]
    fn rejects_missing_required() {
        let schema = json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        });
        let err = validate(&json!({}), &schema).unwrap_err();
        assert!(err.message.contains("path"), "{err}");
    }

    #[test]
    fn rejects_wrong_type() {
        let schema = json!({"type": "object", "properties": {"n": {"type": "integer"}}});
        let err = validate(&json!({"n": "x"}), &schema).unwrap_err();
        assert!(err.message.contains("integer"), "{err}");
    }

    #[test]
    fn rejects_extra_field() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "additionalProperties": false
        });
        let err = validate(&json!({"a": "x", "b": 1}), &schema).unwrap_err();
        assert!(err.message.contains('b'), "{err}");
    }

    #[test]
    fn fail_closed_on_unsupported_keyword() {
        let schema = json!({"type": "object", "patternProperties": {".*": {}}});
        let err = validate_schema(&schema).unwrap_err();
        assert!(err.message.contains("patternProperties"), "{err}");
    }

    #[test]
    fn enum_and_const() {
        let schema = json!({"enum": ["a", "b"]});
        assert!(validate(&json!("a"), &schema).is_ok());
        assert!(validate(&json!("c"), &schema).is_err());

        let schema = json!({"const": 7});
        assert!(validate(&json!(7), &schema).is_ok());
        assert!(validate(&json!(8), &schema).is_err());
    }
}
