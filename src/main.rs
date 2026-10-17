use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use dbconnector_core::{NetworkPolicy, ServerConfig};
use dbconnector_mcp::serve;

#[derive(Debug, Parser)]
#[command(
    name = "dbconnector",
    version,
    about = "stdio-only MCP server for PostgreSQL, MySQL/MariaDB, SQLite, MongoDB, and Redis/Valkey"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the MCP server on stdin/stdout. Logs go to stderr.
    Serve(ServeArgs),
}

#[derive(Debug, Parser)]
struct ServeArgs {
    /// Allow SQL, MongoDB, and Redis write tools. Connections stay read-only unless open_connection sets read_only=false.
    #[arg(long)]
    allow_writes: bool,
    /// Allow destructive operations when a tool also passes allow_destructive=true.
    #[arg(long)]
    allow_destructive: bool,
    /// Host policy. `open` (default) allows any host the operating system can reach. `restricted` allows only --allow-host entries.
    #[arg(long, default_value = "open")]
    network_policy: String,
    /// Host or IP allowed when --network-policy restricted is set. Repeat the flag for more than one host.
    #[arg(long = "allow-host")]
    allow_host: Vec<String>,
    /// Require verified TLS for TCP database URLs. SQLite is unchanged.
    #[arg(long)]
    require_db_tls: bool,
    /// Restrict SQLite files to this directory.
    #[arg(long)]
    sqlite_root: Option<PathBuf>,
    /// Maximum open connection entries.
    #[arg(long, default_value_t = 16)]
    max_connections: usize,
    /// Maximum physical connections in each SQL pool.
    #[arg(long, default_value_t = 3)]
    sql_pool_max: u32,
    /// Default query timeout in milliseconds.
    #[arg(long, default_value_t = 10_000)]
    query_timeout_ms: u64,
    /// Maximum rows returned by a read tool.
    #[arg(long, default_value_t = 1000)]
    max_rows: u64,
    /// Default row limit when a tool does not set one.
    #[arg(long, default_value_t = 100)]
    default_rows: u64,
    /// Maximum serialized result size in bytes.
    #[arg(long, default_value_t = 262_144)]
    max_result_bytes: usize,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Serve(args) => match build_config(args) {
            Ok(config) => {
                if let Err(err) = serve(config).await {
                    tracing::error!(error = %err, "MCP server stopped");
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(message) => {
                eprintln!("{message}");
                ExitCode::from(2)
            }
        },
    }
}

fn build_config(args: ServeArgs) -> Result<ServerConfig, String> {
    let network = match args.network_policy.as_str() {
        "open" => {
            if !args.allow_host.is_empty() {
                return Err("--allow-host is only used with --network-policy restricted".into());
            }
            NetworkPolicy::Open
        }
        "restricted" => NetworkPolicy::Restricted {
            hosts: args.allow_host,
        },
        other => return Err(format!("unknown --network-policy `{other}`")),
    };
    if args.max_connections == 0
        || args.sql_pool_max == 0
        || args.max_rows == 0
        || args.default_rows == 0
    {
        return Err("connection, pool, and row limits must be greater than zero".into());
    }
    if args.default_rows > args.max_rows {
        return Err("--default-rows cannot exceed --max-rows".into());
    }
    Ok(ServerConfig {
        allow_writes: args.allow_writes,
        allow_destructive: args.allow_destructive,
        network,
        require_db_tls: args.require_db_tls,
        sqlite_root: args.sqlite_root,
        max_connections: args.max_connections,
        sql_pool_max: args.sql_pool_max,
        query_timeout: Duration::from_millis(args.query_timeout_ms),
        max_rows: args.max_rows,
        default_rows: args.default_rows,
        max_result_bytes: args.max_result_bytes,
        ..ServerConfig::default()
    })
}

#[cfg(test)]
mod feature_tools {
    use dbconnector_core::ServerConfig;
    use dbconnector_mcp::DbServer;

    #[tokio::test]
    async fn registered_tools_follow_crate_features() {
        let server = DbServer::new(ServerConfig::default());
        let names = server.tool_names();
        for required in [
            "open_connection",
            "close_connection",
            "list_connections",
            "get_connection_info",
            "get_capabilities",
        ] {
            assert!(names.iter().any(|name| name == required), "{names:?}");
        }
        assert_eq!(
            names.iter().any(|name| name.starts_with("sql_")),
            cfg!(feature = "sql")
        );
        assert_eq!(
            names.iter().any(|name| name.starts_with("mongo_")),
            cfg!(feature = "mongo")
        );
        assert_eq!(
            names.iter().any(|name| name.starts_with("redis_")),
            cfg!(feature = "redis")
        );
    }
}
