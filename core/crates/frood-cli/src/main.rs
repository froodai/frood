//! `frood`: the server and the command-line interface (docs/16_platform.md §5).

mod compose;
mod init;
mod lifecycle;
mod modules;
mod serve;
mod token;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "frood",
    version,
    about = "Frood: self-hosted memory and assistant platform"
)]
struct Cli {
    /// Path to frood.yaml.
    #[arg(
        long,
        global = true,
        default_value = "frood.yaml",
        env = "FROOD_CONFIG"
    )]
    config: PathBuf,
    /// Secrets file. Default: .frood/secrets.env next to the config.
    #[arg(long, global = true, env = "FROOD_SECRETS")]
    secrets: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create frood.yaml and a secrets file in a directory.
    Init {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Installation name: lowercase letters, digits and dashes.
        #[arg(long, default_value = "personal")]
        name: String,
        /// Where the server runs.
        #[arg(long, value_enum, default_value_t = Placement::Container)]
        server: Placement,
        /// Build the server image from this directory instead of pulling it.
        #[arg(long)]
        build: Option<String>,
        /// Host port for PostgreSQL (bound to 127.0.0.1).
        #[arg(long, default_value_t = 5432)]
        postgres_port: u16,
        /// Overwrite an existing frood.yaml. Existing secrets are always kept.
        #[arg(long)]
        force: bool,
    },
    /// Check the config and show what `apply` would do.
    Plan,
    /// Start services, run migrations and record the applied config revision.
    Apply {
        /// Only write .frood/deploy/compose.yaml; start nothing.
        #[arg(long)]
        render_only: bool,
    },
    /// Run the server.
    Serve,
    /// Show server health and the applied config revision.
    Status,
    /// Personal tokens for scripts and bots.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// Config utilities.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Exit 0 if the URL answers 200. Used by container health checks.
    #[command(hide = true)]
    Probe { url: String },
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Create a token and print it (once) to stdout.
    Create {
        /// Whose token it is; the user is created if new.
        #[arg(long)]
        email: String,
        /// What it is for, e.g. "iPhone shortcut".
        #[arg(long)]
        label: String,
        /// Comma-separated scopes. Default: all but admin.
        #[arg(long, value_delimiter = ',')]
        scopes: Vec<String>,
        /// Expire after this many days. Default: never.
        #[arg(long)]
        expires_days: Option<u32>,
    },
    /// List a user's personal tokens.
    List {
        #[arg(long)]
        email: String,
    },
    /// Revoke a token by id.
    Revoke {
        #[arg(long)]
        email: String,
        id: uuid::Uuid,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the JSON Schema of frood.yaml.
    Schema,
}

#[derive(Clone, Copy, ValueEnum)]
enum Placement {
    Container,
    Host,
}

impl From<Placement> for frood_config::ServerPlacement {
    fn from(value: Placement) -> Self {
        match value {
            Placement::Container => Self::Container,
            Placement::Host => Self::Host,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let paths = lifecycle::Paths::new(cli.config, cli.secrets);
    let result = match cli.command {
        Command::Init {
            dir,
            name,
            server,
            build,
            postgres_port,
            force,
        } => init::run(&init::Options {
            dir,
            name,
            server: server.into(),
            build,
            postgres_port,
            force,
        }),
        Command::Plan => lifecycle::plan(&paths).await,
        Command::Apply { render_only } => lifecycle::apply(&paths, render_only).await,
        Command::Serve => serve::run(&paths).await,
        Command::Status => lifecycle::status(&paths).await,
        Command::Token { command } => match command {
            TokenCommand::Create {
                email,
                label,
                scopes,
                expires_days,
            } => {
                let scopes = if scopes.is_empty() {
                    token::default_scopes()
                } else {
                    scopes
                };
                token::create(&paths, &email, &label, &scopes, expires_days).await
            }
            TokenCommand::List { email } => token::list(&paths, &email).await,
            TokenCommand::Revoke { email, id } => token::revoke(&paths, &email, id).await,
        },
        Command::Config {
            command: ConfigCommand::Schema,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&frood_config::json_schema()).unwrap_or_default()
            );
            Ok(())
        }
        Command::Probe { url } => lifecycle::probe(&url).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
