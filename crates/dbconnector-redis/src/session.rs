#![allow(clippy::too_many_arguments)]

use std::time::Duration;

use dbconnector_core::{redact_secrets, DbError, ErrorCode};
use redis::{AsyncCommands, Client, Value};
use serde_json::{json, Value as Json};

use crate::encode::{decode_payload, encode_bytes, encode_value};

pub struct RedisSession {
    manager: redis::aio::ConnectionManager,
    pub read_only: bool,
    pub server_version: String,
    pub product: String,
}

impl RedisSession {
    pub async fn connect(database_url: &str, read_only: bool) -> Result<Self, DbError> {
        let client = Client::open(database_url).map_err(connect_err)?;
        let manager = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(connect_err)?;
        let mut conn = manager.clone();
        let pong: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(connect_err)?;
        if pong != "PONG" {
            return Err(DbError::new(
                ErrorCode::ConnectionFailed,
                "Redis PING did not return PONG",
            ));
        }
        let info: String = redis::cmd("INFO")
            .arg("server")
            .query_async(&mut conn)
            .await
            .unwrap_or_default();
        let server_version = info_value(&info, "redis_version").unwrap_or_else(|| "unknown".into());
        let product = if info_value(&info, "server_name")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("valkey")
            || info.to_ascii_lowercase().contains("valkey_version")
        {
            "valkey".into()
        } else {
            "redis".into()
        };
        Ok(Self {
            manager,
            read_only,
            server_version,
            product,
        })
    }

    pub async fn scan(
        &self,
        cursor: &str,
        pattern: Option<&str>,
        count: u64,
        timeout: Duration,
    ) -> Result<Json, DbError> {
        let count = count.clamp(1, 1000);
        let mut cmd = redis::cmd("SCAN");
        cmd.arg(cursor);
        if let Some(pattern) = pattern {
            cmd.arg("MATCH").arg(pattern);
        }
        cmd.arg("COUNT").arg(count);
        let value = self.query(&mut cmd, timeout).await?;
        let (next_cursor, keys) = match value {
            Value::Array(items) if items.len() == 2 => {
                let cursor = value_text(&items[0]).unwrap_or_else(|| "0".into());
                let keys = match &items[1] {
                    Value::Array(keys) | Value::Set(keys) => {
                        keys.iter().map(encode_value).collect::<Vec<_>>()
                    }
                    other => vec![encode_value(other)],
                };
                (cursor, keys)
            }
            other => {
                return Err(DbError::new(
                    ErrorCode::QueryFailed,
                    format!("unexpected SCAN reply: {}", encode_value(&other)),
                ))
            }
        };
        Ok(json!({
            "next_cursor": next_cursor,
            "keys": keys,
            "returned": keys.len(),
            "scan_note": "SCAN may return duplicate keys and can miss keys written during iteration. COUNT is a hint.",
        }))
    }

    pub async fn key_info(&self, key: &str, timeout: Duration) -> Result<Json, DbError> {
        let key_type: String = self.cmd_string("TYPE", &[key], timeout).await?;
        let ttl: i64 = self.cmd_i64("TTL", &[key], timeout).await?;
        let pttl: i64 = self.cmd_i64("PTTL", &[key], timeout).await?;
        let mut memory = None;
        if let Ok(value) = self.try_i64("MEMORY", &["USAGE", key], timeout).await {
            memory = Some(value);
        }
        Ok(json!({
            "key": encode_bytes(key.as_bytes()),
            "type": key_type,
            "ttl_seconds": ttl,
            "pttl_millis": pttl,
            "memory_bytes": memory,
        }))
    }

    pub async fn get(&self, key: &str, timeout: Duration) -> Result<Json, DbError> {
        let mut conn = self.manager.clone();
        let value: Option<Vec<u8>> = tokio::time::timeout(timeout, conn.get(key))
            .await
            .map_err(|_| timeout_err())?
            .map_err(query_err)?;
        Ok(json!({
            "key": encode_bytes(key.as_bytes()),
            "found": value.is_some(),
            "value": value.as_deref().map(encode_bytes),
        }))
    }

    pub async fn set(
        &self,
        key: &str,
        text: Option<&str>,
        base64_value: Option<&str>,
        ex: Option<u64>,
        px: Option<u64>,
        nx: bool,
        xx: bool,
        timeout: Duration,
    ) -> Result<Json, DbError> {
        self.ensure_write()?;
        if nx && xx {
            return Err(DbError::invalid("nx and xx cannot both be true"));
        }
        if ex.is_some() && px.is_some() {
            return Err(DbError::invalid("ex and px cannot both be set"));
        }
        let bytes = decode_payload(text, base64_value)?;
        let mut cmd = redis::cmd("SET");
        cmd.arg(key).arg(bytes);
        if let Some(ex) = ex {
            cmd.arg("EX").arg(ex);
        }
        if let Some(px) = px {
            cmd.arg("PX").arg(px);
        }
        if nx {
            cmd.arg("NX");
        }
        if xx {
            cmd.arg("XX");
        }
        let value = self.query(&mut cmd, timeout).await?;
        Ok(json!({"applied": !matches!(value, Value::Nil), "reply": encode_value(&value)}))
    }

    pub async fn delete(
        &self,
        keys: &[String],
        unlink: bool,
        timeout: Duration,
    ) -> Result<Json, DbError> {
        self.ensure_write()?;
        if keys.is_empty() {
            return Err(DbError::invalid("keys must not be empty"));
        }
        if keys.len() > 1000 {
            return Err(DbError::new(
                ErrorCode::ResourceLimitExceeded,
                "at most 1000 keys can be deleted in one call",
            ));
        }
        let mut cmd = redis::cmd(if unlink { "UNLINK" } else { "DEL" });
        for key in keys {
            cmd.arg(key);
        }
        let deleted: i64 = self.query_typed(&mut cmd, timeout).await?;
        Ok(json!({"deleted": deleted}))
    }

    pub async fn hash(
        &self,
        action: &str,
        key: &str,
        field: Option<&str>,
        text: Option<&str>,
        base64_value: Option<&str>,
        cursor: Option<&str>,
        count: Option<u64>,
        timeout: Duration,
    ) -> Result<Json, DbError> {
        self.ensure_action_write(matches!(action, "hset" | "hdel"))?;
        let reply = match action {
            "hget" => {
                let field = required(field, "field")?;
                self.query(redis::cmd("HGET").arg(key).arg(field), timeout)
                    .await?
            }
            "hset" => {
                let field = required(field, "field")?;
                let bytes = decode_payload(text, base64_value)?;
                self.query(redis::cmd("HSET").arg(key).arg(field).arg(bytes), timeout)
                    .await?
            }
            "hdel" => {
                let field = required(field, "field")?;
                self.query(redis::cmd("HDEL").arg(key).arg(field), timeout)
                    .await?
            }
            "hgetall" => self.query(redis::cmd("HGETALL").arg(key), timeout).await?,
            "hlen" => self.query(redis::cmd("HLEN").arg(key), timeout).await?,
            "hexists" => {
                let field = required(field, "field")?;
                self.query(redis::cmd("HEXISTS").arg(key).arg(field), timeout)
                    .await?
            }
            "hscan" => {
                let mut cmd = redis::cmd("HSCAN");
                cmd.arg(key).arg(cursor.unwrap_or("0"));
                if let Some(count) = count {
                    cmd.arg("COUNT").arg(count.clamp(1, 1000));
                }
                self.query(&mut cmd, timeout).await?
            }
            other => {
                return Err(DbError::invalid(format!(
                    "unsupported hash action `{other}`"
                )))
            }
        };
        Ok(json!({"action": action, "reply": encode_value(&reply)}))
    }

    pub async fn list(
        &self,
        action: &str,
        key: &str,
        text: Option<&str>,
        base64_value: Option<&str>,
        start: Option<i64>,
        stop: Option<i64>,
        timeout: Duration,
    ) -> Result<Json, DbError> {
        self.ensure_action_write(matches!(action, "lpush" | "rpush" | "lpop" | "rpop"))?;
        let reply = match action {
            "llen" => self.query(redis::cmd("LLEN").arg(key), timeout).await?,
            "lrange" => {
                let start = start.unwrap_or(0);
                let stop = stop.unwrap_or(99);
                if stop.saturating_sub(start) > 1000 {
                    return Err(DbError::new(
                        ErrorCode::ResourceLimitExceeded,
                        "lrange span is limited to 1000 elements",
                    ));
                }
                self.query(redis::cmd("LRANGE").arg(key).arg(start).arg(stop), timeout)
                    .await?
            }
            "lpush" | "rpush" => {
                let bytes = decode_payload(text, base64_value)?;
                let command = action.to_ascii_uppercase();
                self.query(redis::cmd(&command).arg(key).arg(bytes), timeout)
                    .await?
            }
            "lpop" | "rpop" => {
                let command = action.to_ascii_uppercase();
                self.query(redis::cmd(&command).arg(key), timeout).await?
            }
            other => {
                return Err(DbError::invalid(format!(
                    "unsupported list action `{other}`"
                )))
            }
        };
        Ok(json!({"action": action, "reply": encode_value(&reply)}))
    }

    pub async fn set_members(
        &self,
        action: &str,
        key: &str,
        text: Option<&str>,
        base64_value: Option<&str>,
        cursor: Option<&str>,
        count: Option<u64>,
        timeout: Duration,
    ) -> Result<Json, DbError> {
        self.ensure_action_write(matches!(action, "sadd" | "srem"))?;
        let reply = match action {
            "scard" => self.query(redis::cmd("SCARD").arg(key), timeout).await?,
            "sismember" => {
                let bytes = decode_payload(text, base64_value)?;
                self.query(redis::cmd("SISMEMBER").arg(key).arg(bytes), timeout)
                    .await?
            }
            "sadd" | "srem" => {
                let bytes = decode_payload(text, base64_value)?;
                let command = action.to_ascii_uppercase();
                self.query(redis::cmd(&command).arg(key).arg(bytes), timeout)
                    .await?
            }
            "sscan" => {
                let mut cmd = redis::cmd("SSCAN");
                cmd.arg(key).arg(cursor.unwrap_or("0"));
                if let Some(count) = count {
                    cmd.arg("COUNT").arg(count.clamp(1, 1000));
                }
                self.query(&mut cmd, timeout).await?
            }
            other => {
                return Err(DbError::invalid(format!(
                    "unsupported set action `{other}`"
                )))
            }
        };
        Ok(json!({"action": action, "reply": encode_value(&reply)}))
    }

    pub async fn sorted_set(
        &self,
        action: &str,
        key: &str,
        member_text: Option<&str>,
        member_base64: Option<&str>,
        score: Option<f64>,
        start: Option<i64>,
        stop: Option<i64>,
        timeout: Duration,
    ) -> Result<Json, DbError> {
        self.ensure_action_write(matches!(action, "zadd" | "zrem"))?;
        let reply = match action {
            "zcard" => self.query(redis::cmd("ZCARD").arg(key), timeout).await?,
            "zrange" => {
                let start = start.unwrap_or(0);
                let stop = stop.unwrap_or(99);
                self.query(
                    redis::cmd("ZRANGE")
                        .arg(key)
                        .arg(start)
                        .arg(stop)
                        .arg("WITHSCORES"),
                    timeout,
                )
                .await?
            }
            "zadd" => {
                let score = score.ok_or_else(|| DbError::invalid("zadd requires score"))?;
                let member = decode_payload(member_text, member_base64)?;
                self.query(redis::cmd("ZADD").arg(key).arg(score).arg(member), timeout)
                    .await?
            }
            "zrem" => {
                let member = decode_payload(member_text, member_base64)?;
                self.query(redis::cmd("ZREM").arg(key).arg(member), timeout)
                    .await?
            }
            other => {
                return Err(DbError::invalid(format!(
                    "unsupported sorted set action `{other}`"
                )))
            }
        };
        Ok(json!({"action": action, "reply": encode_value(&reply)}))
    }

    pub async fn info(&self, timeout: Duration) -> Result<Json, DbError> {
        let mut conn = self.manager.clone();
        let ping: String = tokio::time::timeout(timeout, redis::cmd("PING").query_async(&mut conn))
            .await
            .map_err(|_| timeout_err())?
            .map_err(query_err)?;
        let server: String = tokio::time::timeout(
            timeout,
            redis::cmd("INFO").arg("server").query_async(&mut conn),
        )
        .await
        .map_err(|_| timeout_err())?
        .map_err(query_err)?;
        let memory: String = tokio::time::timeout(
            timeout,
            redis::cmd("INFO").arg("memory").query_async(&mut conn),
        )
        .await
        .map_err(|_| timeout_err())?
        .unwrap_or_default();
        let clients: String = tokio::time::timeout(
            timeout,
            redis::cmd("INFO").arg("clients").query_async(&mut conn),
        )
        .await
        .map_err(|_| timeout_err())?
        .unwrap_or_default();
        Ok(json!({
            "ping": ping,
            "product": self.product,
            "redis_version": info_value(&server, "redis_version"),
            "valkey_version": info_value(&server, "valkey_version"),
            "os": info_value(&server, "os"),
            "tcp_port": info_value(&server, "tcp_port"),
            "role": info_value(&server, "role"),
            "used_memory_human": info_value(&memory, "used_memory_human"),
            "connected_clients": info_value(&clients, "connected_clients"),
        }))
    }

    fn ensure_write(&self) -> Result<(), DbError> {
        if self.read_only {
            Err(DbError::permission(
                "this connection is read-only; open a new connection with read_only=false",
            ))
        } else {
            Ok(())
        }
    }

    fn ensure_action_write(&self, write: bool) -> Result<(), DbError> {
        if write {
            self.ensure_write()
        } else {
            Ok(())
        }
    }

    async fn query(&self, cmd: &mut redis::Cmd, timeout: Duration) -> Result<Value, DbError> {
        let mut conn = self.manager.clone();
        tokio::time::timeout(timeout, cmd.query_async(&mut conn))
            .await
            .map_err(|_| timeout_err())?
            .map_err(query_err)
    }

    async fn query_typed<T: redis::FromRedisValue>(
        &self,
        cmd: &mut redis::Cmd,
        timeout: Duration,
    ) -> Result<T, DbError> {
        let mut conn = self.manager.clone();
        tokio::time::timeout(timeout, cmd.query_async(&mut conn))
            .await
            .map_err(|_| timeout_err())?
            .map_err(query_err)
    }

    async fn cmd_string(
        &self,
        command: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<String, DbError> {
        let mut cmd = redis::cmd(command);
        for arg in args {
            cmd.arg(*arg);
        }
        self.query_typed(&mut cmd, timeout).await
    }

    async fn cmd_i64(
        &self,
        command: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<i64, DbError> {
        let mut cmd = redis::cmd(command);
        for arg in args {
            cmd.arg(*arg);
        }
        self.query_typed(&mut cmd, timeout).await
    }

    async fn try_i64(
        &self,
        command: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<i64, DbError> {
        self.cmd_i64(command, args, timeout).await
    }
}

fn required<'a>(value: Option<&'a str>, name: &str) -> Result<&'a str, DbError> {
    value.ok_or_else(|| DbError::invalid(format!("{name} is required")))
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::BulkString(bytes) => std::str::from_utf8(bytes).ok().map(str::to_string),
        Value::SimpleString(text) => Some(text.clone()),
        Value::Int(v) => Some(v.to_string()),
        _ => None,
    }
}

fn info_value(info: &str, key: &str) -> Option<String> {
    info.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name == key {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

fn connect_err(err: redis::RedisError) -> DbError {
    DbError::new(
        ErrorCode::ConnectionFailed,
        redact_secrets(&err.to_string()),
    )
}

fn query_err(err: redis::RedisError) -> DbError {
    DbError::new(ErrorCode::QueryFailed, redact_secrets(&err.to_string()))
}

fn timeout_err() -> DbError {
    DbError::new(
        ErrorCode::QueryTimeout,
        "Redis operation exceeded the timeout; execution state may be unknown",
    )
}
