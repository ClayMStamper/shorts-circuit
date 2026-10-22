//! `shorts-runner`: the cloud runner process and its maintenance commands.
//!
//! ```text
//! shorts-runner serve                  run the sync API and the worker (the default)
//! shorts-runner migrate                apply database migrations and exit
//! shorts-runner token create <name>    register a desktop installation; prints its token once
//! shorts-runner token list             list installations
//! shorts-runner token revoke <id>      revoke an installation's token
//! shorts-runner generate-key           print a new random master key
//! shorts-runner rotate-keys            re-wrap every credential under the current master key
//! ```

use std::process::ExitCode;
use std::str::FromStr;
use std::sync::Arc;

use shorts_runner::{MediaDir, PgStore, Runner, RunnerConfig, RunnerSetup, Vault};
use shorts_scheduler::{SystemClock, WorkerConfig};
use sqlx::postgres::PgConnectOptions;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

fn usage() -> ExitCode {
    eprintln!(
        "usage: shorts-runner [serve | migrate | token create <name> | token list | \
         token revoke <id> | generate-key | rotate-keys]"
    );
    ExitCode::from(2)
}

async fn open(config: &RunnerConfig) -> Result<PgStore, String> {
    let vault = Vault::from_env().map_err(|e| e.to_string())?;
    let options = PgConnectOptions::from_str(&config.database_url)
        .map_err(|_| "DATABASE_URL is not a PostgreSQL connection string".to_owned())?;
    PgStore::connect(options, Arc::new(vault))
        .await
        .map_err(|e| e.to_string())
}

async fn run(args: &[String]) -> Result<(), String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if words == ["generate-key"] {
        println!("{}", Vault::generate_key());
        return Ok(());
    }
    let config = RunnerConfig::from_env()?;
    let store = open(&config).await?;
    let now = chrono::Utc::now();
    match words.as_slice() {
        [] | ["serve"] => {
            let media = MediaDir::new(config.data_dir.clone());
            let runner = Runner::new(store, media, RunnerSetup::from_env(), Arc::new(SystemClock));
            let listener = tokio::net::TcpListener::bind(config.bind)
                .await
                .map_err(|e| format!("cannot listen on {}: {e}", config.bind))?;
            info!(bind = %config.bind, data_dir = %config.data_dir.display(), "cloud runner listening");
            let shutdown = CancellationToken::new();
            let stop = shutdown.clone();
            tokio::spawn(async move {
                shutdown_signal().await;
                info!("shutting down; running jobs are released with their checkpoints");
                stop.cancel();
            });
            runner
                .serve(listener, WorkerConfig::default(), shutdown)
                .await
                .map_err(|e| e.to_string())
        }
        ["migrate"] => {
            println!("database is up to date");
            Ok(())
        }
        ["token", "create", name @ ..] if !name.is_empty() => {
            let (installation, token) = store
                .create_installation(&name.join(" "), now)
                .await
                .map_err(|e| e.to_string())?;
            println!("installation {} ({})", installation.id, installation.name);
            println!("token (shown once; paste it into the desktop app): {token}");
            Ok(())
        }
        ["token", "list"] => {
            for i in store.installations().await.map_err(|e| e.to_string())? {
                let state = match i.revoked_at {
                    Some(at) => format!("revoked {}", at.format("%Y-%m-%d %H:%M UTC")),
                    None => match i.last_seen_at {
                        Some(at) => format!("last seen {}", at.format("%Y-%m-%d %H:%M UTC")),
                        None => "never used".to_owned(),
                    },
                };
                println!("{}  {}  ({state})", i.id, i.name);
            }
            Ok(())
        }
        ["token", "revoke", id] => {
            let id = id
                .parse()
                .map_err(|_| "not an installation id".to_owned())?;
            store
                .revoke_installation(id, now)
                .await
                .map_err(|e| e.to_string())?;
            println!("revoked {id}");
            Ok(())
        }
        ["rotate-keys"] => {
            let count = store
                .rewrap_credentials()
                .await
                .map_err(|e| e.to_string())?;
            let in_use = store
                .credential_key_ids()
                .await
                .map_err(|e| e.to_string())?;
            println!("re-wrapped {count} credentials");
            println!("master keys in use: {}", in_use.join(", "));
            println!("current master key: {}", store.vault().active_key_id());
            Ok(())
        }
        _ => Err(String::new()),
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.is_empty() => usage(),
        Err(e) => {
            error!("{e}");
            eprintln!("shorts-runner: {e}");
            ExitCode::FAILURE
        }
    }
}
