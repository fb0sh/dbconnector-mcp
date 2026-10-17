use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{DbError, ErrorCode};
use crate::target::{DatabaseKind, ParsedTarget};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkPolicy {
    Open,
    Restricted { hosts: Vec<String> },
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub allow_writes: bool,
    pub allow_destructive: bool,
    pub network: NetworkPolicy,
    pub require_db_tls: bool,
    pub sqlite_root: Option<PathBuf>,
    pub max_connections: usize,
    pub sql_pool_max: u32,
    pub global_concurrency: usize,
    pub per_connection_concurrency: usize,
    pub query_timeout: Duration,
    pub max_rows: u64,
    pub default_rows: u64,
    pub max_result_bytes: usize,
    pub idle_ttl: Duration,
    pub txn_idle_ttl: Duration,
    pub txn_max_lifetime: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            allow_writes: false,
            allow_destructive: false,
            network: NetworkPolicy::Open,
            require_db_tls: false,
            sqlite_root: None,
            max_connections: 16,
            sql_pool_max: 3,
            global_concurrency: 8,
            per_connection_concurrency: 4,
            query_timeout: Duration::from_secs(10),
            max_rows: 1000,
            default_rows: 100,
            max_result_bytes: 256 * 1024,
            idle_ttl: Duration::from_secs(30 * 60),
            txn_idle_ttl: Duration::from_secs(60),
            txn_max_lifetime: Duration::from_secs(5 * 60),
        }
    }
}

impl ServerConfig {
    pub fn ensure_write(&self, connection_read_only: bool) -> Result<(), DbError> {
        if !self.allow_writes {
            return Err(DbError::permission(
                "writes are disabled; start the server with --allow-writes and open the connection with read_only=false",
            ));
        }
        if connection_read_only {
            return Err(DbError::permission(
                "this connection is read-only; open a new connection with read_only=false",
            ));
        }
        Ok(())
    }

    pub fn ensure_destructive(&self, allow_destructive: bool) -> Result<(), DbError> {
        if !self.allow_destructive || !allow_destructive {
            return Err(DbError::permission(
                "destructive operation requires server flag --allow-destructive and tool argument allow_destructive=true",
            ));
        }
        Ok(())
    }

    /// Check network and SQLite path policy. Does not open a socket.
    pub fn authorize_target(&self, target: &ParsedTarget) -> Result<(), DbError> {
        if target.kind == DatabaseKind::Sqlite {
            return self.authorize_sqlite(target);
        }
        if self.require_db_tls && !target.tls_strict {
            return Err(DbError::permission(
                "server was started with --require-db-tls; the database URL must request verified TLS (postgres sslmode=verify-full, mysql ssl-mode=VERIFY_IDENTITY, mongodb tls=true, or rediss://)",
            ));
        }
        match &self.network {
            NetworkPolicy::Open => Ok(()),
            NetworkPolicy::Restricted { hosts } => self.authorize_host(target, hosts),
        }
    }

    fn authorize_sqlite(&self, target: &ParsedTarget) -> Result<(), DbError> {
        let Some(root) = &self.sqlite_root else {
            return Ok(());
        };
        if target.sqlite_memory {
            return Err(DbError::permission(
                "in-memory SQLite is outside --sqlite-root",
            ));
        }
        let Some(path) = &target.sqlite_path else {
            return Err(DbError::invalid("SQLite URL is missing a file path"));
        };
        let root = canonicalize_existing_dir(root)?;
        let candidate = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()
                .map_err(|e| DbError::invalid(format!("cannot resolve working directory: {e}")))?
                .join(path)
        };
        let parent = candidate.parent().unwrap_or(Path::new("."));
        let parent_canon = if parent.exists() {
            parent.canonicalize().map_err(|e| {
                DbError::permission(format!("cannot canonicalize SQLite directory: {e}"))
            })?
        } else {
            return Err(DbError::permission(
                "SQLite parent directory does not exist under --sqlite-root",
            ));
        };
        if !parent_canon.starts_with(&root) {
            return Err(DbError::permission("SQLite path is outside --sqlite-root"));
        }
        Ok(())
    }

    fn authorize_host(&self, target: &ParsedTarget, hosts: &[String]) -> Result<(), DbError> {
        let Some(host) = target.host.as_deref() else {
            return Err(DbError::permission(
                "restricted network policy rejected a connection without a host",
            ));
        };
        if hosts.is_empty() {
            return Err(DbError::permission(
                "restricted network policy has no --allow-host entries",
            ));
        }
        let host_ok = hosts.iter().any(|allowed| host_matches(host, allowed));
        if !host_ok {
            return Err(DbError::permission(format!(
                "host `{host}` is not in the restricted allow list"
            )));
        }
        // Resolve before connect so a name that does not resolve fails closed.
        // Explicit hostnames in the allow list authorize that name; IP entries
        // authorize that address. Resolved addresses are checked when the allow
        // list contains any IP so a name cannot point at an unlisted address
        // while an IP-only policy is in effect.
        if host.parse::<IpAddr>().is_err() {
            let resolved = resolve_host(host)?;
            let allow_ips: Vec<IpAddr> = hosts
                .iter()
                .filter_map(|h| h.parse::<IpAddr>().ok())
                .collect();
            if !allow_ips.is_empty() {
                let all_listed = resolved.iter().all(|ip| allow_ips.contains(ip));
                if !all_listed {
                    return Err(DbError::permission(format!(
                        "resolved addresses for `{host}` are not all present in --allow-host"
                    )));
                }
            }
        }
        Ok(())
    }
}

fn host_matches(host: &str, allowed: &str) -> bool {
    host.eq_ignore_ascii_case(allowed.trim())
}

fn resolve_host(host: &str) -> Result<Vec<IpAddr>, DbError> {
    let port = 1;
    match std::net::ToSocketAddrs::to_socket_addrs(&(host, port)) {
        Ok(iter) => {
            let ips: Vec<IpAddr> = iter.map(|addr| addr.ip()).collect();
            if ips.is_empty() {
                Err(DbError::permission(format!(
                    "host `{host}` did not resolve to an address"
                )))
            } else {
                Ok(ips)
            }
        }
        Err(err) => Err(DbError::new(
            ErrorCode::PermissionDenied,
            format!("failed to resolve host `{host}`: {err}"),
        )),
    }
}

fn canonicalize_existing_dir(path: &Path) -> Result<PathBuf, DbError> {
    if !path.exists() {
        return Err(DbError::invalid("--sqlite-root does not exist"));
    }
    path.canonicalize()
        .map_err(|e| DbError::invalid(format!("cannot canonicalize --sqlite-root: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::parse_target;

    #[test]
    fn open_policy_allows_remote_host_without_allow_list() {
        let cfg = ServerConfig::default();
        let target = parse_target("postgres://db.example:5432/app").unwrap();
        cfg.authorize_target(&target).unwrap();
    }

    #[test]
    fn restricted_policy_rejects_unlisted_host() {
        let cfg = ServerConfig {
            network: NetworkPolicy::Restricted {
                hosts: vec!["127.0.0.1".into()],
            },
            ..ServerConfig::default()
        };
        let target = parse_target("postgres://10.1.1.1:5432/app").unwrap();
        let err = cfg.authorize_target(&target).unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
    }

    #[test]
    fn require_tls_rejects_plaintext_postgres() {
        let cfg = ServerConfig {
            require_db_tls: true,
            ..ServerConfig::default()
        };
        let target = parse_target("postgres://localhost/app").unwrap();
        assert_eq!(
            cfg.authorize_target(&target).unwrap_err().code,
            ErrorCode::PermissionDenied
        );
    }

    #[test]
    fn require_tls_accepts_verify_full() {
        let cfg = ServerConfig {
            require_db_tls: true,
            ..ServerConfig::default()
        };
        let target = parse_target("postgres://localhost/app?sslmode=verify-full").unwrap();
        cfg.authorize_target(&target).unwrap();
    }

    #[test]
    fn writes_require_server_flag_and_writable_connection() {
        let cfg = ServerConfig::default();
        assert!(cfg.ensure_write(false).is_err());
        let cfg = ServerConfig {
            allow_writes: true,
            ..ServerConfig::default()
        };
        assert!(cfg.ensure_write(true).is_err());
        cfg.ensure_write(false).unwrap();
    }
}
