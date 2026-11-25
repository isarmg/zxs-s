use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use zxs_core::{
    CommitRequest, ConflictChoice, ContainerId, FileName, FolderPath, RecycleId, VersionNum,
};

#[derive(Parser)]
#[command(version, about = "Work with files and versions on a ZXS server")]
pub(crate) struct Cli {
    #[arg(long, global = true, env = "ZXS_WORKSPACE")]
    pub workspace: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[cfg(windows)]
pub(crate) fn default_workspace() -> zxs_core::Result<PathBuf> {
    use winsafe::{SHGetKnownFolderPath, co};

    let desktop = SHGetKnownFolderPath(&co::KNOWNFOLDERID::Desktop, co::KF::DEFAULT, None)
        .map_err(|error| zxs_core::invalid(format!("无法读取 Windows 桌面目录 · {error}")))?;
    Ok(PathBuf::from(desktop).join("ZXS-S-Workspace"))
}

#[derive(Subcommand)]
pub(crate) enum Command {
    Login {
        #[arg(default_value = "http://127.0.0.1:7738")]
        server: String,
    },
    Logout,
    Mkdir {
        folder: FolderPath,
    },
    MvFolder {
        from: FolderPath,
        to: FolderPath,
    },
    Rmdir {
        folder: FolderPath,
        #[arg(long)]
        recursive: bool,
    },
    Ls {
        folder: Option<FolderPath>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        q: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        limit: Option<u64>,
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
    Untrack {
        path: PathBuf,
    },
    Refresh,
    Sync,
    SyncDelete {
        path: PathBuf,
        #[arg(long)]
        expected_version: Option<VersionNum>,
    },
    Detail {
        id: ContainerId,
    },
    Patch {
        id: ContainerId,
        #[arg(long)]
        folder: Option<FolderPath>,
        #[arg(long)]
        name: Option<FileName>,
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
        #[arg(long, conflicts_with = "tags")]
        clear_tags: bool,
    },
    Revert {
        id: ContainerId,
        version: VersionNum,
        #[arg(long)]
        expected_version: Option<VersionNum>,
    },
    Commit(CommitArgs),
    InitBundle {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    Recycle {
        #[command(subcommand)]
        action: Option<RecycleCommand>,
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
pub(crate) struct CommitArgs {
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

#[derive(Subcommand)]
pub(crate) enum RecycleCommand {
    Ls {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        limit: Option<u64>,
    },
    Move {
        id: ContainerId,
    },
    Purge {
        id: Option<RecycleId>,
        #[arg(long, required_unless_present = "id", conflicts_with = "id")]
        all: bool,
    },
}
