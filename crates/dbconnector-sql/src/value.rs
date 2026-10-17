use base64::Engine;
use dbconnector_core::DbError;
use serde_json::{json, Map, Value};
use sqlx::any::AnyArguments;
use sqlx::Arguments;

#[derive(Debug, Clone)]
pub enum BoundValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Json(Value),
}

pub fn parse_params(params: Option<&[Value]>) -> Result<Vec<BoundValue>, DbError> {
    let Some(params) = params else {
        return Ok(Vec::new());
    };
    params.iter().map(parse_param).collect()
}

fn parse_param(value: &Value) -> Result<BoundValue, DbError> {
    if let Some(obj) = value.as_object() {
        if let Some(tag) = obj.get("t").and_then(Value::as_str) {
            return parse_typed(tag, obj);
        }
    }
    match value {
        Value::Null => Ok(BoundValue::Null),
        Value::Bool(v) => Ok(BoundValue::Bool(*v)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(BoundValue::Int(i))
            } else if let Some(u) = n.as_u64() {
                if u > i64::MAX as u64 {
                    Ok(BoundValue::Text(u.to_string()))
                } else {
                    Ok(BoundValue::Int(u as i64))
                }
            } else if let Some(f) = n.as_f64() {
                Ok(BoundValue::Float(f))
            } else {
                Err(DbError::invalid("numeric parameter is not representable"))
            }
        }
        Value::String(s) => Ok(BoundValue::Text(s.clone())),
        Value::Array(_) | Value::Object(_) => Ok(BoundValue::Json(value.clone())),
    }
}

fn parse_typed(tag: &str, obj: &Map<String, Value>) -> Result<BoundValue, DbError> {
    let raw = obj.get("v");
    match tag {
        "null" => Ok(BoundValue::Null),
        "bool" => raw
            .and_then(Value::as_bool)
            .map(BoundValue::Bool)
            .ok_or_else(|| DbError::invalid("bool parameter needs v")),
        "int" => raw
            .and_then(Value::as_i64)
            .map(BoundValue::Int)
            .ok_or_else(|| DbError::invalid("int parameter needs integer v")),
        "float" => raw
            .and_then(Value::as_f64)
            .map(BoundValue::Float)
            .ok_or_else(|| DbError::invalid("float parameter needs numeric v")),
        "decimal" | "text" => raw
            .and_then(Value::as_str)
            .map(|s| BoundValue::Text(s.to_string()))
            .ok_or_else(|| DbError::invalid("text parameter needs string v")),
        "bytes" => {
            let encoded = raw
                .and_then(Value::as_str)
                .ok_or_else(|| DbError::invalid("bytes parameter needs base64 string v"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| DbError::invalid("bytes parameter is not valid base64"))?;
            Ok(BoundValue::Bytes(bytes))
        }
        "json" => raw
            .cloned()
            .map(BoundValue::Json)
            .ok_or_else(|| DbError::invalid("json parameter needs v")),
        other => Err(DbError::invalid(format!(
            "unknown parameter type `{other}`"
        ))),
    }
}

pub fn bind_args(values: &[BoundValue]) -> Result<AnyArguments<'_>, DbError> {
    let mut args = AnyArguments::default();
    for value in values {
        let result = match value {
            BoundValue::Null => args.add(None::<String>),
            BoundValue::Bool(v) => args.add(*v),
            BoundValue::Int(v) => args.add(*v),
            BoundValue::Float(v) => args.add(*v),
            BoundValue::Text(v) => args.add(v.as_str()),
            BoundValue::Bytes(v) => args.add(v.as_slice()),
            BoundValue::Json(v) => args.add(v.to_string()),
        };
        result.map_err(|err| DbError::invalid(format!("failed to bind parameter: {err}")))?;
    }
    Ok(args)
}

pub fn cell_from_raw(
    type_name: &str,
    text: Option<String>,
    bytes: Option<Vec<u8>>,
    json: Option<Value>,
    integer: Option<i64>,
    float: Option<f64>,
    boolean: Option<bool>,
) -> Value {
    let type_name = type_name.to_ascii_lowercase();
    if matches!(type_name.as_str(), "bool" | "boolean") {
        if let Some(v) = boolean {
            return Value::Bool(v);
        }
    }
    if is_int_type(&type_name) {
        if let Some(v) = integer {
            return Value::Number(v.into());
        }
    }
    if is_float_type(&type_name) {
        if let Some(v) = float {
            return serde_json::Number::from_f64(v)
                .map(Value::Number)
                .unwrap_or(Value::Null);
        }
    }
    if is_bytes_type(&type_name) {
        if let Some(v) = bytes {
            // MySQL TEXT arrives as BLOB through sqlx Any. Valid UTF-8 is tagged
            // as text; other bytes stay base64 so the payload is not altered.
            if let Ok(text) = std::str::from_utf8(&v) {
                return json!({"encoding": "utf8", "text": text});
            }
            return json!({
                "encoding": "base64",
                "base64": base64::engine::general_purpose::STANDARD.encode(v)
            });
        }
    }
    if is_json_type(&type_name) {
        if let Some(v) = json {
            return v;
        }
    }
    if is_decimal_type(&type_name) {
        if let Some(v) = text {
            return Value::String(v);
        }
    }
    if let Some(v) = text {
        return Value::String(v);
    }
    if let Some(v) = integer {
        return Value::Number(v.into());
    }
    if let Some(v) = boolean {
        return Value::Bool(v);
    }
    if let Some(v) = float {
        return serde_json::Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or(Value::Null);
    }
    Value::Null
}

fn is_int_type(name: &str) -> bool {
    matches!(
        name,
        "int2"
            | "int4"
            | "int8"
            | "integer"
            | "bigint"
            | "smallint"
            | "tinyint"
            | "serial"
            | "bigserial"
    ) || name.contains("int")
}

fn is_float_type(name: &str) -> bool {
    matches!(name, "float4" | "float8" | "float" | "double" | "real")
}

fn is_decimal_type(name: &str) -> bool {
    name.contains("numeric") || name.contains("decimal")
}

fn is_bytes_type(name: &str) -> bool {
    matches!(
        name,
        "bytea" | "blob" | "binary" | "varbinary" | "tinyblob" | "mediumblob" | "longblob"
    ) || name.contains("blob")
        || name == "bytea"
}

fn is_json_type(name: &str) -> bool {
    name.contains("json")
}
