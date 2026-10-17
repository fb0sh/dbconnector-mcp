use base64::Engine;
use redis::Value;
use serde_json::{json, Map, Value as Json};

pub fn encode_value(value: &Value) -> Json {
    match value {
        Value::Nil => Json::Null,
        Value::Int(v) => Json::Number((*v).into()),
        Value::Double(v) => serde_json::Number::from_f64(*v)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Boolean(v) => Json::Bool(*v),
        Value::SimpleString(v) => json!({"encoding": "utf8", "text": v}),
        Value::Okay => json!({"encoding": "utf8", "text": "OK"}),
        Value::BulkString(bytes) => encode_bytes(bytes),
        Value::Array(items) | Value::Set(items) => {
            Json::Array(items.iter().map(encode_value).collect())
        }
        Value::Map(items) => {
            let mut object = Map::new();
            for (key, value) in items {
                let name = match encode_bytes_text(key) {
                    Some(text) => text,
                    None => format!("b64:{}", bytes_of(key).map(base64_of).unwrap_or_default()),
                };
                object.insert(name, encode_value(value));
            }
            Json::Object(object)
        }
        Value::Attribute { data, .. } => encode_value(data),
        other => json!({"encoding": "utf8", "text": format!("{other:?}")}),
    }
}

pub fn encode_bytes(bytes: &[u8]) -> Json {
    match std::str::from_utf8(bytes) {
        Ok(text) => json!({"encoding": "utf8", "text": text}),
        Err(_) => json!({
            "encoding": "base64",
            "base64": base64::engine::general_purpose::STANDARD.encode(bytes)
        }),
    }
}

fn encode_bytes_text(value: &Value) -> Option<String> {
    match value {
        Value::SimpleString(text) => Some(text.clone()),
        Value::BulkString(bytes) => std::str::from_utf8(bytes).ok().map(str::to_string),
        Value::Int(v) => Some(v.to_string()),
        _ => None,
    }
}

fn bytes_of(value: &Value) -> Option<&[u8]> {
    match value {
        Value::BulkString(bytes) => Some(bytes),
        Value::SimpleString(text) => Some(text.as_bytes()),
        _ => None,
    }
}

fn base64_of(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode_payload(
    text: Option<&str>,
    base64_value: Option<&str>,
) -> Result<Vec<u8>, dbconnector_core::DbError> {
    match (text, base64_value) {
        (Some(text), None) => Ok(text.as_bytes().to_vec()),
        (None, Some(encoded)) => base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| dbconnector_core::DbError::invalid("value base64 is invalid")),
        _ => Err(dbconnector_core::DbError::invalid(
            "provide exactly one of text or base64",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_round_trip_is_lossless() {
        let raw = vec![0xff, 0xfe, 0x00, 0x01];
        let encoded = encode_bytes(&raw);
        assert_eq!(encoded["encoding"], "base64");
        let back = decode_payload(None, encoded["base64"].as_str()).unwrap();
        assert_eq!(back, raw);
    }

    #[test]
    fn utf8_is_marked() {
        let encoded = encode_bytes("hello".as_bytes());
        assert_eq!(encoded["encoding"], "utf8");
        assert_eq!(encoded["text"], "hello");
    }
}
