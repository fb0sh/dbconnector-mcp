use std::str::FromStr;

use base64::Engine;
use bson::{Binary, Bson, Decimal128, Document, Timestamp};
use dbconnector_core::DbError;
use serde_json::{Map, Value};

pub fn json_to_bson(value: &Value) -> Result<Bson, DbError> {
    match value {
        Value::Null => Ok(Bson::Null),
        Value::Bool(v) => Ok(Bson::Boolean(*v)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                    Ok(Bson::Int32(i as i32))
                } else {
                    Ok(Bson::Int64(i))
                }
            } else if let Some(f) = n.as_f64() {
                Ok(Bson::Double(f))
            } else {
                Err(DbError::invalid(
                    "numeric JSON value is not representable as BSON",
                ))
            }
        }
        Value::String(s) => Ok(Bson::String(s.clone())),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(json_to_bson(item)?);
            }
            Ok(Bson::Array(out))
        }
        Value::Object(map) => object_to_bson(map),
    }
}

fn object_to_bson(map: &Map<String, Value>) -> Result<Bson, DbError> {
    if map.len() == 1 {
        let (key, value) = map.iter().next().expect("len checked");
        if let Some(bson) = extended_scalar(key, value)? {
            return Ok(bson);
        }
    }
    let mut doc = Document::new();
    for (key, value) in map {
        doc.insert(key, json_to_bson(value)?);
    }
    Ok(Bson::Document(doc))
}

fn extended_scalar(key: &str, value: &Value) -> Result<Option<Bson>, DbError> {
    match key {
        "$oid" => {
            let hex = value
                .as_str()
                .ok_or_else(|| DbError::invalid("$oid must be a string"))?;
            let oid = bson::oid::ObjectId::parse_str(hex)
                .map_err(|_| DbError::invalid("invalid ObjectId"))?;
            Ok(Some(Bson::ObjectId(oid)))
        }
        "$date" => {
            let dt = match value {
                Value::String(s) => bson::DateTime::parse_rfc3339_str(s)
                    .map_err(|_| DbError::invalid("invalid $date string"))?,
                Value::Object(map) => {
                    let n = map
                        .get("$numberLong")
                        .and_then(Value::as_str)
                        .ok_or_else(|| DbError::invalid("$date object needs $numberLong"))?;
                    let millis: i64 = n
                        .parse()
                        .map_err(|_| DbError::invalid("invalid $date millis"))?;
                    bson::DateTime::from_millis(millis)
                }
                Value::Number(n) => {
                    let millis = n
                        .as_i64()
                        .ok_or_else(|| DbError::invalid("invalid $date number"))?;
                    bson::DateTime::from_millis(millis)
                }
                _ => return Err(DbError::invalid("unsupported $date value")),
            };
            Ok(Some(Bson::DateTime(dt)))
        }
        "$numberDecimal" => {
            let raw = value
                .as_str()
                .ok_or_else(|| DbError::invalid("$numberDecimal must be a string"))?;
            let decimal =
                Decimal128::from_str(raw).map_err(|_| DbError::invalid("invalid Decimal128"))?;
            Ok(Some(Bson::Decimal128(decimal)))
        }
        "$numberLong" => {
            let raw = value
                .as_str()
                .ok_or_else(|| DbError::invalid("$numberLong must be a string"))?;
            let n = raw
                .parse::<i64>()
                .map_err(|_| DbError::invalid("invalid $numberLong"))?;
            Ok(Some(Bson::Int64(n)))
        }
        "$numberInt" => {
            let raw = value
                .as_str()
                .ok_or_else(|| DbError::invalid("$numberInt must be a string"))?;
            let n = raw
                .parse::<i32>()
                .map_err(|_| DbError::invalid("invalid $numberInt"))?;
            Ok(Some(Bson::Int32(n)))
        }
        "$binary" => {
            let obj = value
                .as_object()
                .ok_or_else(|| DbError::invalid("$binary must be an object"))?;
            let encoded = obj
                .get("base64")
                .and_then(Value::as_str)
                .ok_or_else(|| DbError::invalid("$binary.base64 is required"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| DbError::invalid("$binary.base64 is invalid"))?;
            let subtype = obj.get("subType").and_then(Value::as_str).unwrap_or("00");
            let code = u8::from_str_radix(subtype, 16)
                .map_err(|_| DbError::invalid("invalid binary subType"))?;
            Ok(Some(Bson::Binary(Binary {
                subtype: code.into(),
                bytes,
            })))
        }
        _ => Ok(None),
    }
}

pub fn json_to_document(value: &Value) -> Result<Document, DbError> {
    match json_to_bson(value)? {
        Bson::Document(doc) => Ok(doc),
        _ => Err(DbError::invalid("expected a JSON object")),
    }
}

pub fn bson_to_json(value: &Bson) -> Value {
    match value {
        Bson::Double(v) => serde_json::Number::from_f64(*v)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Bson::String(v) | Bson::JavaScriptCode(v) | Bson::Symbol(v) => Value::String(v.clone()),
        Bson::Array(items) => Value::Array(items.iter().map(bson_to_json).collect()),
        Bson::Document(doc) => Value::Object(document_map(doc)),
        Bson::Boolean(v) => Value::Bool(*v),
        Bson::Null | Bson::Undefined => Value::Null,
        Bson::RegularExpression(regex) => json_obj([(
            "$regularExpression",
            json_obj([
                ("pattern", Value::String(regex.pattern.clone())),
                ("options", Value::String(regex.options.clone())),
            ]),
        )]),
        Bson::JavaScriptCodeWithScope(code) => {
            json_obj([("$code", Value::String(code.code.clone()))])
        }
        Bson::Int32(v) => Value::Number((*v).into()),
        Bson::Int64(v) => json_obj([("$numberLong", Value::String(v.to_string()))]),
        Bson::Timestamp(Timestamp { time, increment }) => json_obj([(
            "$timestamp",
            json_obj([
                ("t", Value::Number((*time).into())),
                ("i", Value::Number((*increment).into())),
            ]),
        )]),
        Bson::Binary(binary) => json_obj([(
            "$binary",
            json_obj([
                (
                    "base64",
                    Value::String(base64::engine::general_purpose::STANDARD.encode(&binary.bytes)),
                ),
                (
                    "subType",
                    Value::String(format!("{:02x}", u8::from(binary.subtype))),
                ),
            ]),
        )]),
        Bson::ObjectId(oid) => json_obj([("$oid", Value::String(oid.to_hex()))]),
        Bson::DateTime(dt) => json_obj([(
            "$date",
            Value::String(
                dt.try_to_rfc3339_string()
                    .unwrap_or_else(|_| dt.timestamp_millis().to_string()),
            ),
        )]),
        Bson::Decimal128(decimal) => {
            json_obj([("$numberDecimal", Value::String(decimal.to_string()))])
        }
        Bson::MaxKey => json_obj([("$maxKey", Value::Number(1.into()))]),
        Bson::MinKey => json_obj([("$minKey", Value::Number(1.into()))]),
        Bson::DbPointer(_) => json_obj([("$dbPointer", Value::String("dbpointer".into()))]),
    }
}

pub fn document_to_json(doc: &Document) -> Value {
    Value::Object(document_map(doc))
}

fn document_map(doc: &Document) -> Map<String, Value> {
    doc.iter()
        .map(|(key, value)| (key.to_string(), bson_to_json(value)))
        .collect()
}

fn json_obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Object(Map::from_iter(pairs.map(|(k, v)| (k.to_string(), v))))
}

pub fn pipeline_has_write_stage(pipeline: &[Document]) -> bool {
    pipeline.iter().any(document_has_write)
}

fn document_has_write(doc: &Document) -> bool {
    doc.iter()
        .any(|(key, value)| key_is_write(key) || bson_has_write(value))
}

fn bson_has_write(value: &Bson) -> bool {
    match value {
        Bson::Document(doc) => document_has_write(doc),
        Bson::Array(items) => items.iter().any(bson_has_write),
        _ => false,
    }
}

fn key_is_write(key: &str) -> bool {
    matches!(key, "$out" | "$merge")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_extended_types() {
        let input = json!({
            "_id": {"$oid": "656e6b2f9f1b2c3d4e5f6789"},
            "when": {"$date": "2024-01-02T03:04:05Z"},
            "money": {"$numberDecimal": "12.34"},
            "blob": {"$binary": {"base64": "AQID", "subType": "00"}}
        });
        let bson = json_to_bson(&input).unwrap();
        let output = bson_to_json(&bson);
        assert_eq!(output["_id"]["$oid"], "656e6b2f9f1b2c3d4e5f6789");
        assert_eq!(output["money"]["$numberDecimal"], "12.34");
        assert_eq!(output["blob"]["$binary"]["base64"], "AQID");
        assert!(output["when"]["$date"]
            .as_str()
            .unwrap()
            .starts_with("2024-01-02"));
    }

    #[test]
    fn detects_nested_write_stages() {
        let pipeline = vec![json_to_document(&json!({
            "$facet": {"out": [{"$out": "dest"}]}
        }))
        .unwrap()];
        assert!(pipeline_has_write_stage(&pipeline));
        let read = vec![json_to_document(&json!({"$match": {"a": 1}})).unwrap()];
        assert!(!pipeline_has_write_stage(&read));
    }
}
