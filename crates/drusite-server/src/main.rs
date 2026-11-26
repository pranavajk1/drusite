// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `drusite-server` command.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use drusite_server::app::{self, AppState};
use drusite_server::config::{Config, LoginMethod};
use drusite_server::crypto::LocalMasterKeys;
use drusite_server::db::Db;
use drusite_server::keys::{KeySource, Keyring};
use drusite_server::policy::Policy;
use drusite_server::storage::BlobStore;
use drusite_server::{account, telemetry};

#[derive(Debug, Parser)]
#[command(name = "drusite-server", version, about = "The Drusite sync server")]
struct Cli {
    /// The config file. Every value can also come from a DRUSITE__SECTION__KEY variable.
    #[arg(long, short, env = "DRUSITE_CONFIG", global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the server (the default).
    Serve,
    /// Apply pending database migrations, then exit.
    Migrate,
    /// Load and check the configuration, then exit.
    CheckConfig,
    /// Manage accounts from the server.
    #[command(subcommand)]
    Account(AccountCommand),
    /// Master keys for the env and file key sources.
    #[command(subcommand)]
    Keys(KeysCommand),
}

#[derive(Debug, Subcommand)]
enum AccountCommand {
    /// Print a one-time sign-in link for an account that has lost its login methods.
    Recover {
        email: String,
        /// How long the link works.
        #[arg(long, default_value_t = 30)]
        ttl_minutes: i64,
    },
    /// Create an account and print its first sign-in link.
    Create {
        email: String,
        #[arg(long, default_value = "")]
        name: String,
        #[arg(long, default_value_t = 24 * 60)]
        ttl_minutes: i64,
    },
}

#[derive(Debug, Subcommand)]
enum KeysCommand {
    /// Print a new master key. Put it first in the key source to rotate; keep the old one after
    /// it until every workspace key has been re-wrapped.
    Generate,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("drusite-server: can't start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("drusite-server: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn load_config(cli: &Cli) -> anyhow::Result<Config> {
    Ok(Config::load(cli.config.as_deref(), std::env::vars())?)
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command.as_ref().unwrap_or(&Command::Serve) {
        Command::Keys(KeysCommand::Generate) => {
            println!("{}", LocalMasterKeys::generate_entry()?);
            Ok(())
        }
        Command::CheckConfig => {
            let config = load_config(&cli)?;
            KeySource::from_config(&config.keys)?;
            Policy::load()?;
            println!("config OK. Login methods:");
            for method in config.auth.enabled_methods() {
                match method {
                    LoginMethod::Oidc { id, display_name } => {
                        println!("  oidc {id} ({display_name})");
                    }
                    other => println!("  {other:?}"),
                }
            }
            Ok(())
        }
        Command::Migrate => {
            let config = load_config(&cli)?;
            let db = Db::connect(&config.database).await?;
            db.preflight(config.database.allow_rls_bypass).await?;
            db.migrate().await?;
            println!("migrations applied");
            Ok(())
        }
        Command::Account(command) => {
            let config = load_config(&cli)?;
            let db = Db::connect(&config.database).await?;
            if !db.migrations_current().await.unwrap_or(false) {
                anyhow::bail!("the database isn't migrated; run `drusite-server migrate` first");
            }
            let (user, token, ttl) = match command {
                AccountCommand::Recover { email, ttl_minutes } => {
                    let ttl = time::Duration::minutes(*ttl_minutes);
                    let (user, token) = account::recover(&db, email, ttl).await?;
                    (user, token, ttl)
                }
                AccountCommand::Create {
                    email,
                    name,
                    ttl_minutes,
                } => {
                    let ttl = time::Duration::minutes(*ttl_minutes);
                    let (user, token) = account::create(&db, email, name, ttl).await?;
                    (user, token, ttl)
                }
            };
            println!("account {user}");
            println!(
                "one-time sign-in link, valid for {} minutes (give it only to the account's owner):",
                ttl.whole_minutes()
            );
            println!("{}", account::recovery_link(&config.public_url, &token));
            Ok(())
        }
        Command::Serve => serve(load_config(&cli)?).await,
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let _telemetry = telemetry::init(&config.telemetry)?;
    let methods = config.auth.enabled_methods();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        ?methods,
        "starting drusite-server"
    );

    let policy = Policy::load()?;
    let db = Db::connect(&config.database)
        .await
        .context("connecting to PostgreSQL")?;
    db.preflight(config.database.allow_rls_bypass).await?;
    if config.database.migrate_on_start {
        db.migrate().await.context("migrating the database")?;
    } else if !db.migrations_current().await? {
        anyhow::bail!(
            "the database has pending migrations and migrate_on_start is off; \
             run `drusite-server migrate`"
        );
    }
    let source = KeySource::from_config(&config.keys)?;
    source.check().await.context("checking the key source")?;
    let blobs = BlobStore::from_config(&config.storage)?;
    blobs
        .check_writable()
        .await
        .context("checking the object store can be written")?;

    let listen = config.server.listen;
    let state = AppState::new(config, db, blobs, Keyring::new(source), policy);
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("listening on {listen}"))?;
    tracing::info!(%listen, "ready");
    axum::serve(listener, app::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("stopped");
    Ok(())
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
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
