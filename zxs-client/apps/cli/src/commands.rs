use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zxs_core::{
    Checkout, CheckoutRequest, CommitRequest, CommitTarget, ConflictChoice, ContainerId, FileName,
    FolderPath, RecycleId, Remote, Result, SyncReport, VersionNum, Workspace,
};

#[derive(Parser)]
#[command(version, about = "Work with files and versions on a ZXS server")]
pub(crate) struct Cli {
    #[arg(long, global = true)]
    pub workspace: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[cfg(windows)]
pub(crate) fn default_workspace() -> zxs_core::Result<PathBuf> {
    use winsafe::{SHGetKnownFolderPath, co};

    let desktop = SHGetKnownFolderPath(&co::KNOWNFOLDERID::Desktop, co::KF::DEFAULT, None)
        .map_err(|error| zxs_core::invalid(format!("无法读取 Windows 桌面目录 · {error}")))?;
    let workspace = PathBuf::from(desktop).join("ZXS-S-Workspace");
    std::fs::create_dir_all(&workspace)?;
    Ok(workspace)
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
    Checkout(CheckoutArgs),
    Status,
    Untrack {
        path: PathBuf,
    },
    Refresh,
    Sync(SyncArgs),
    SyncDelete(SyncDeleteArgs),
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
        folder: FolderPath,
        #[arg(long)]
        name: Option<FileName>,
    },
    Ui,
}

#[derive(Args, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckoutArgs {
    id: ContainerId,
    #[arg(long)]
    version: Option<VersionNum>,
    #[arg(long)]
    to: Option<PathBuf>,
    #[arg(long)]
    #[serde(skip)]
    backup: bool,
    #[arg(skip = Checkout::IfClean)]
    choice: Checkout,
}

impl From<CheckoutArgs> for CheckoutRequest {
    fn from(args: CheckoutArgs) -> Self {
        Self {
            id: args.id,
            version: args.version,
            destination: args.to,
            choice: if args.backup {
                Checkout::Backup
            } else {
                args.choice
            },
        }
    }
}

#[derive(Args, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommitArgs {
    path: PathBuf,
    #[arg(skip)]
    expected_container: Option<CommitTarget>,
    #[arg(long)]
    #[serde(skip)]
    folder: Option<FolderPath>,
    #[arg(long)]
    name: Option<FileName>,
    #[arg(long = "tag")]
    #[serde(skip)]
    tags: Vec<String>,
    #[arg(long, value_enum, default_value = "stop")]
    #[serde(rename = "choice")]
    on_conflict: Resolution,
}

#[derive(Clone, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum Resolution {
    Stop,
    Append,
    New,
    Pull,
}

#[derive(Args, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SyncArgs {
    #[arg(long, help = "Also check out all files in this server directory")]
    folder: Option<FolderPath>,
}

impl SyncArgs {
    pub fn run(&self, root: &Path, remote: &Remote) -> Result<SyncReport> {
        let workspace = Workspace::open(root)?;
        match &self.folder {
            Some(folder) => workspace.sync_directory(remote, folder),
            None => workspace.refresh(remote),
        }
    }
}

#[derive(Args, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SyncDeleteArgs {
    path: PathBuf,
    #[arg(long)]
    expected_version: Option<VersionNum>,
    #[arg(skip)]
    expected_container: Option<ContainerId>,
}

impl SyncDeleteArgs {
    pub fn run(&self, root: &Path, remote: &Remote) -> Result<impl Serialize> {
        let expected = (self.expected_container.as_ref(), self.expected_version);
        Workspace::open(root)?.sync_delete(remote, &self.path, expected)
    }
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
            expected_container: args.expected_container,
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
