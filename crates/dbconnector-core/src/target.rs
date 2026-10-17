use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use url::Url;

use crate::error::{DbError, ErrorCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseKind {
    Postgres,
    Mysql,
    Sqlite,
    Mongodb,
    Redis,
}

impl DatabaseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Sqlite => "sqlite",
            Self::Mongodb => "mongodb",
            Self::Redis => "redis",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParsedTarget {
    pub kind: DatabaseKind,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    /// True when the URL asks for TLS with certificate and hostname verification.
    pub tls_strict: bool,
    /// True when the URL asks for any TLS, including modes that do not verify.
    pub tls_requested: bool,
    pub sqlite_memory: bool,
    pub sqlite_path: Option<PathBuf>,
}

pub fn parse_target(database_url: &str) -> Result<ParsedTarget, DbError> {
    let trimmed = database_url.trim();
    if trimmed.is_empty() {
        return Err(DbError::invalid("database_url is empty"));
    }
    if trimmed.starts_with("sqlite:") {
        return parse_sqlite(trimmed);
    }
    let url = Url::parse(trimmed)
        .map_err(|err| DbError::invalid(format!("database_url is not a valid URL: {err}")))?;
    let scheme = url.scheme().to_ascii_lowercase();
    let kind = match scheme.as_str() {
        "postgres" | "postgresql" => DatabaseKind::Postgres,
        "mysql" | "mariadb" => DatabaseKind::Mysql,
        "mongodb" | "mongodb+srv" => DatabaseKind::Mongodb,
        "redis" | "rediss" => DatabaseKind::Redis,
        _ => {
            return Err(DbError::new(
                ErrorCode::UnsupportedEngine,
                format!("unsupported URL scheme `{scheme}`"),
            ))
        }
    };
    let host = url.host_str().map(|h| h.to_string());
    if kind != DatabaseKind::Sqlite && host.is_none() {
        return Err(DbError::invalid("database URL is missing a host"));
    }
    let database = match kind {
        DatabaseKind::Redis => {
            let path = url.path().trim_matches('/');
            if path.is_empty() {
                None
            } else {
                Some(path.to_string())
            }
        }
        _ => {
            let path = url.path().trim_start_matches('/');
            if path.is_empty() {
                None
            } else {
                Some(path.to_string())
            }
        }
    };
    let (tls_requested, tls_strict) = tls_flags(kind, &scheme, &url);
    Ok(ParsedTarget {
        kind,
        host,
        port: url.port(),
        database,
        tls_strict,
        tls_requested,
        sqlite_memory: false,
        sqlite_path: None,
    })
}

fn tls_flags(kind: DatabaseKind, scheme: &str, url: &Url) -> (bool, bool) {
    let params: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
        .collect();
    let get = |name: &str| {
        params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.to_ascii_lowercase())
    };
    match kind {
        DatabaseKind::Postgres => {
            let mode = get("sslmode").unwrap_or_default();
            let requested = matches!(mode.as_str(), "require" | "verify-ca" | "verify-full");
            let strict = mode == "verify-full";
            (requested, strict)
        }
        DatabaseKind::Mysql => {
            let mode = get("ssl-mode")
                .or_else(|| get("sslmode"))
                .unwrap_or_default()
                .replace('-', "_");
            let requested = matches!(
                mode.as_str(),
                "required" | "verify_ca" | "verify_identity" | "preferred"
            );
            let strict = mode == "verify_identity";
            (requested, strict)
        }
        DatabaseKind::Mongodb => {
            if scheme == "mongodb+srv" {
                let insecure = get("tlsinsecure").as_deref() == Some("true")
                    || get("tlsallowinvalidcertificates").as_deref() == Some("true");
                return (!insecure, !insecure);
            }
            let tls = get("tls").or_else(|| get("ssl"));
            let enabled = matches!(tls.as_deref(), Some("true") | Some("1"));
            let insecure = get("tlsinsecure").as_deref() == Some("true")
                || get("tlsallowinvalidcertificates").as_deref() == Some("true");
            (enabled, enabled && !insecure)
        }
        DatabaseKind::Redis => {
            let insecure = get("insecure").as_deref() == Some("true");
            if scheme == "rediss" {
                (true, !insecure)
            } else {
                (false, false)
            }
        }
        DatabaseKind::Sqlite => (false, false),
    }
}

fn parse_sqlite(raw: &str) -> Result<ParsedTarget, DbError> {
    let rest = raw.trim_start_matches("sqlite:");
    let (memory, path) = if rest == ":memory:" || rest == "//:memory:" || rest == "///:memory:" {
        (true, None)
    } else if let Some(path) = rest.strip_prefix("//") {
        if path.is_empty() {
            return Err(DbError::invalid("SQLite URL is missing a path"));
        }
        // sqlite:///abs and sqlite://relative. Three slashes means absolute.
        if let Some(abs) = path.strip_prefix('/') {
            if abs.is_empty() {
                return Err(DbError::invalid("SQLite URL is missing a path"));
            }
            (false, Some(PathBuf::from(format!("/{abs}"))))
        } else {
            (false, Some(PathBuf::from(path)))
        }
    } else if rest.is_empty() {
        return Err(DbError::invalid("SQLite URL is missing a path"));
    } else {
        (false, Some(PathBuf::from(rest)))
    };
    Ok(ParsedTarget {
        kind: DatabaseKind::Sqlite,
        host: None,
        port: None,
        database: path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
        tls_strict: false,
        tls_requested: false,
        sqlite_memory: memory,
        sqlite_path: path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_schemes() {
        assert_eq!(
            parse_target("postgresql://localhost:5432/app")
                .unwrap()
                .kind,
            DatabaseKind::Postgres
        );
        assert_eq!(
            parse_target("mysql://10.0.0.8:3306/app").unwrap().kind,
            DatabaseKind::Mysql
        );
        assert_eq!(
            parse_target("mongodb+srv://cluster.example/app")
                .unwrap()
                .kind,
            DatabaseKind::Mongodb
        );
        assert!(
            parse_target("mongodb+srv://cluster.example/app")
                .unwrap()
                .tls_strict
        );
        assert_eq!(
            parse_target("rediss://cache.example:6380/0").unwrap().kind,
            DatabaseKind::Redis
        );
        assert!(parse_target("rediss://cache.example/0").unwrap().tls_strict);
        let sqlite = parse_target("sqlite:///tmp/app.db").unwrap();
        assert_eq!(sqlite.kind, DatabaseKind::Sqlite);
        assert_eq!(sqlite.sqlite_path.unwrap(), PathBuf::from("/tmp/app.db"));
    }

    #[test]
    fn rejects_unknown_scheme() {
        let err = parse_target("oracle://localhost/app").unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedEngine);
    }

    #[test]
    fn does_not_retain_password_in_target_debug() {
        let target = parse_target("postgres://app:s3cret@localhost/app").unwrap();
        let rendered = format!("{target:?}");
        assert!(!rendered.contains("s3cret"));
    }
}
