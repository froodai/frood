//! `frood`: the server and the command-line interface (docs/16_platform.md §5).

mod compose;
mod export;
mod init;
mod lifecycle;
mod memory_import;
mod modules;
mod recovery;
mod serve;
mod token;
mod wizard;

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
    /// Create frood.yaml and a secrets file in a directory. In a terminal it asks a few
    /// questions (Enter takes each default) and can start Frood right away.
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
        /// Who signs in: on this computer without a password, on a server through OIDC.
        #[arg(long)]
        email: Option<String>,
        /// A server's public https address; without it Frood runs on this computer.
        #[arg(long)]
        public_url: Option<String>,
        /// IANA time zone, e.g. Europe/Amsterdam; this computer's by default.
        #[arg(long)]
        timezone: Option<String>,
        /// Ask nothing: take every default.
        #[arg(long)]
        yes: bool,
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
    /// Logs of the server, PostgreSQL or a module (by its name in frood.yaml); all by default.
    Logs {
        service: Option<String>,
        /// Keep printing new lines.
        #[arg(long, short)]
        follow: bool,
        /// How many recent lines of each.
        #[arg(long, default_value_t = 200)]
        lines: u32,
    },
    /// Personal tokens for scripts and bots.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// Print a one-time link that adds a passkey (fingerprint, Face ID): open it on the device
    /// that will sign in.
    Passkey {
        /// Whose passkey, one of auth.allow. Default: the only address there.
        #[arg(long)]
        email: Option<String>,
    },
    /// Send the memory your assistants keep on this computer (Claude Code, Codex, OpenClaw,
    /// Hermes, a folder of Markdown) to Frood. It shows what it found and asks once.
    Import {
        /// A Frood elsewhere, e.g. https://mem.example.com; this installation by default.
        #[arg(long)]
        server: Option<String>,
        /// A token with knowledge:capture for that server.
        #[arg(long, env = "FROOD_TOKEN", hide_env_values = true)]
        token: Option<String>,
        /// Whose memory it is, when the installation lets several people in.
        #[arg(long)]
        email: Option<String>,
        /// A folder of Markdown to send too; may be given several times.
        #[arg(long)]
        folder: Vec<PathBuf>,
        /// Send without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Link your account in a channel (Telegram, …): prints a one-time code for the bot.
    Link {
        /// The channel module's name in frood.yaml, e.g. telegram.
        channel: String,
        /// Whose account it is; the user is created if new.
        #[arg(long)]
        email: String,
    },
    /// Backups: the recovery key.
    Backup {
        #[command(subcommand)]
        command: BackupCommand,
    },
    /// A new installation from a backup: frood.yaml, the secrets and the database. Run it in
    /// an empty folder (or pass --config <folder>/frood.yaml), with RESTIC_PASSWORD set.
    Restore {
        /// The restic repository: a folder on this machine, s3:…, b2:…, sftp:…, rest:….
        #[arg(long)]
        repository: String,
        /// The recovery key file (`frood backup key --out`).
        #[arg(long)]
        identity: PathBuf,
        /// The backup module's image, which has restic, e.g. ghcr.io/froodai/backup-restic:0.1.0.
        #[arg(long)]
        image: String,
        /// KEY=VALUE the repository needs, e.g. AWS_ACCESS_KEY_ID=…; repeatable.
        #[arg(long)]
        env: Vec<String>,
        /// A snapshot id. Default: the latest.
        #[arg(long)]
        snapshot: Option<String>,
        /// The database's port on this machine, when the backup's is taken here.
        #[arg(long)]
        postgres_port: Option<u16>,
        /// The server's address on this machine (e.g. 0.0.0.0:18080), when the backup's port is taken.
        #[arg(long)]
        listen: Option<String>,
    },
    /// Write all knowledge into a new folder, in a form that does not depend on the database.
    Export {
        /// The folder to create; it must not exist or be empty.
        dir: PathBuf,
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
enum BackupCommand {
    /// Make a recovery key. Its secret half stays with you, off the server; the public half
    /// goes into frood.yaml as backup.recovery_key.
    Key {
        /// Write the secret half to this file (only you can read it) instead of printing it.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the JSON Schema of frood.yaml.
    Schema,
    /// Print the sources connected from the console as frood.yaml lines, for git. Tokens stay
    /// sealed in the database: the lines name secrets to put in the secrets file instead.
    Export,
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
            email,
            public_url,
            timezone,
            yes,
        } => {
            init::run(&init::Options {
                dir,
                name,
                server: server.into(),
                build,
                postgres_port,
                force,
                email,
                public_url,
                timezone,
                yes,
            })
            .await
        }
        Command::Plan => lifecycle::plan(&paths).await,
        Command::Apply { render_only } => lifecycle::apply(&paths, render_only).await,
        Command::Serve => serve::run(&paths).await,
        Command::Status => lifecycle::status(&paths).await,
        Command::Logs {
            service,
            follow,
            lines,
        } => lifecycle::logs(&paths, service.as_deref(), follow, lines).await,
        Command::Export { dir } => lifecycle::export(&paths, &dir).await,
        Command::Backup {
            command: BackupCommand::Key { out },
        } => recovery::key(out.as_deref()),
        Command::Restore {
            repository,
            identity,
            image,
            env,
            snapshot,
            postgres_port,
            listen,
        } => {
            recovery::restore(
                &paths,
                &recovery::RestoreOptions {
                    repository,
                    identity,
                    image,
                    env,
                    snapshot,
                    postgres_port,
                    listen,
                },
            )
            .await
        }
        Command::Link { channel, email } => token::link(&paths, &channel, &email).await,
        Command::Import {
            server,
            token,
            email,
            folder,
            yes,
        } => {
            memory_import::run(
                &paths,
                &memory_import::Options {
                    server,
                    token,
                    email,
                    folders: folder,
                    yes,
                },
            )
            .await
        }
        Command::Passkey { email } => token::passkey_link(&paths, email.as_deref()).await,
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
            command: ConfigCommand::Export,
        } => lifecycle::config_export(&paths).await,
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
