mod ui;

use std::{
    io::{self, BufRead, IsTerminal, Read},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use zxs_core::{
    Checkout, CheckoutRequest, CommitRequest, ConflictChoice, ContainerId, FileName, Filter,
    FolderPath, Profile, RecycleFilter, RecycleId, Remote, Restore, Result, VersionNum, Workspace,
    init_bundle, invalid,
};

#[derive(Parser)]
#[command(about = "Work with files and versions on a ZXS server")]
struct Cli {
    #[arg(long, global = true, env = "ZXS_WORKSPACE", default_value = ".")]
    workspace: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Login {
        #[arg(default_value = "http://127.0.0.1:7738")]
        server: String,
        #[arg(long)]
        allow_public_http: bool,
    },
    Ls {
        folder: Option<FolderPath>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        q: Option<String>,
        #[arg(long)]
        cursor: Option<ContainerId>,
        #[arg(long)]
        limit: Option<u16>,
    },
    Checkout {
        id: ContainerId,
        #[arg(long)]
        version: Option<VersionNum>,
        #[arg(long)]
        to: Option<PathBuf>,
        #[arg(long)]
        backup: bool,
    },
    Status,
    Commit(CommitArgs),
    InitBundle {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    Recycle {
        id: Option<ContainerId>,
        #[arg(long)]
        cursor: Option<RecycleId>,
        #[arg(long)]
        limit: Option<u16>,
    },
    Restore {
        id: RecycleId,
        #[arg(long)]
        folder: Option<FolderPath>,
        #[arg(long)]
        name: Option<FileName>,
    },
    Ui,
}

#[derive(Args)]
struct CommitArgs {
    path: PathBuf,
    #[arg(long)]
    folder: Option<FolderPath>,
    #[arg(long)]
    name: Option<FileName>,
    #[arg(long = "tag")]
    tags: Vec<String>,
    #[arg(long, value_enum, default_value = "stop")]
    on_conflict: Resolution,
}

#[derive(Clone, ValueEnum)]
enum Resolution {
    Stop,
    Append,
    New,
    Pull,
}

impl From<CommitArgs> for CommitRequest {
    fn from(args: CommitArgs) -> Self {
        let on_conflict = match args.on_conflict {
            Resolution::Stop => ConflictChoice::Stop,
            Resolution::Append => ConflictChoice::Append,
            Resolution::New => ConflictChoice::New,
            Resolution::Pull => ConflictChoice::Pull,
        };
        Self {
            path: args.path,
            folder: args.folder,
            name: args.name,
            tags: args.tags,
            on_conflict,
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zxs-cli: {error}");
            if error.status() == 409 {
                ExitCode::from(3)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let root = cli.workspace;
    match cli.command {
        Command::Login {
            server,
            allow_public_http,
        } => {
            let (profile, _) = Profile::login(&server, allow_public_http, &password()?)?;
            emit(
                serde_json::json!({ "server": profile.endpoint, "expires_at": profile.session.expires_at }),
            )
        }
        Command::InitBundle { path } => {
            let path = if path.is_absolute() {
                path
            } else {
                root.join(path)
            };
            init_bundle(&path)
        }
        Command::Ui => ui::run(&root),
        Command::Status => emit(Workspace::open(root)?.status()?),
        command => run_remote(&root, command),
    }
}

fn run_remote(root: &std::path::Path, command: Command) -> Result<()> {
    let remote = connected()?;
    match command {
        Command::Ls {
            folder,
            tag,
            q,
            cursor,
            limit,
        } => emit(remote.list(&Filter {
            folder,
            tag,
            q,
            cursor,
            limit,
        })?),
        Command::Checkout {
            id,
            version,
            to,
            backup,
        } => emit(Workspace::open(root)?.checkout(
            &remote,
            CheckoutRequest {
                id,
                version,
                destination: to,
                choice: if backup {
                    Checkout::Backup
                } else {
                    Checkout::IfClean
                },
            },
        )?),
        Command::Commit(args) => emit(Workspace::open(root)?.commit(&remote, &args.into())?),
        Command::Recycle { id: Some(id), .. } => emit(remote.move_to_recycle(&id)?),
        Command::Recycle {
            id: None,
            cursor,
            limit,
        } => emit(remote.recycle(&RecycleFilter { cursor, limit })?),
        Command::Restore { id, folder, name } => {
            emit(remote.restore(&id, &Restore { folder, name })?)
        }
        _ => Err(invalid("command does not use a remote server")),
    }
}

fn connected() -> Result<Remote> {
    let profile = Profile::load()?.ok_or_else(|| invalid("run zxs-cli login first"))?;
    profile.remote()
}

fn emit(data: impl Serialize) -> Result<()> {
    serde_json::to_writer_pretty(io::stdout().lock(), &data)?;
    println!();
    Ok(())
}

fn password() -> Result<String> {
    if let Ok(password) = std::env::var("ZXS_PASSWORD") {
        return Ok(password);
    }
    if io::stdin().is_terminal() {
        return Err(invalid(
            "provide the password through ZXS_PASSWORD or piped standard input",
        ));
    }
    let mut password = String::new();
    io::stdin().lock().take(1027).read_line(&mut password)?;
    if password.ends_with('\n') {
        password.pop();
    }
    if password.ends_with('\r') {
        password.pop();
    }
    Ok(password)
}
