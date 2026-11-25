mod commands;
mod ui;

use std::{
    io::{self, BufRead, IsTerminal, Read},
    process::ExitCode,
};

use clap::Parser;
use commands::{Cli, Command, RecycleCommand};
use serde::Serialize;
use zxs_core::{
    Catalog, Checkout, CheckoutRequest, FileState, Filter, FolderMove, LocalPath, METADATA_LIMIT,
    Patch, Profile, RecycleFilter, Remote, Restore, Result, Workspace, invalid,
};

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
    let root = workspace_path(cli.workspace)?;
    match cli.command {
        Command::Login { server } => {
            let (profile, remote) = Profile::login(&server, &password()?)?;
            close_session(&remote);
            emit(serde_json::json!({ "server": profile.endpoint }))
        }
        Command::Ui => ui::run(&root),
        Command::Status => emit_status(&Workspace::open(root)?),
        Command::Untrack { path } => Workspace::open(root)?.untrack(&path),
        command => run_remote(&root, command),
    }
}

fn workspace_path(path: Option<std::path::PathBuf>) -> Result<std::path::PathBuf> {
    if let Some(path) = path {
        return Ok(path);
    }
    #[cfg(windows)]
    return commands::default_workspace();
    #[cfg(not(windows))]
    Ok(std::env::current_dir()?)
}

fn run_remote(root: &std::path::Path, command: Command) -> Result<()> {
    let profile = Profile::load()?.ok_or_else(|| invalid("run zxs-cli login first"))?;
    if matches!(command, Command::Logout) {
        return profile.clear();
    }
    let remote = profile.remote()?;
    let result = execute_remote(root, &remote, command);
    close_session(&remote);
    result
}

fn close_session(remote: &Remote) {
    if let Err(error) = remote.logout() {
        eprintln!("zxs-cli: 无法结束本次命令的会话 · {error}");
    }
}

fn execute_remote(root: &std::path::Path, remote: &Remote, command: Command) -> Result<()> {
    match command {
        Command::InitBundle { path } => Workspace::open(root)?
            .init_bundle(&path, remote)
            .map(|_| ()),
        Command::Sync => emit(Workspace::open(root)?.refresh(remote)?),
        Command::SyncDelete {
            path,
            expected_version,
        } => emit(Workspace::open(root)?.sync_delete(remote, &path, expected_version)?),

        Command::Refresh => {
            let workspace = Workspace::open(root)?;
            warn_sync_issues(workspace.refresh(remote));
            emit_status(&workspace)
        }
        Command::Ls {
            folder,
            tag,
            q,
            cursor,
            limit,
        } => emit(Catalog::open(remote, Some(root))?.list(&Filter {
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
        } => checkout(
            root,
            remote,
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
        ),
        Command::Commit(args) => {
            let workspace = Workspace::open(root)?;
            let result = workspace.commit(remote, &args.into())?;
            warn_bundle_conflicts(&workspace);
            emit(result)
        }
        command => run_container_command(root, remote, command),
    }
}

fn checkout(root: &std::path::Path, remote: &Remote, request: CheckoutRequest) -> Result<()> {
    let workspace = Workspace::open(root)?;
    let result = workspace.checkout(remote, request)?;
    warn_bundle_conflicts(&workspace);
    emit(result)
}

fn run_container_command(root: &std::path::Path, remote: &Remote, command: Command) -> Result<()> {
    match command {
        Command::Mkdir { folder } => {
            emit(Catalog::open(remote, Some(root))?.create_folder(folder)?)
        }
        Command::MvFolder { from, to } => {
            let moved = Catalog::open(remote, Some(root))?.move_folder(&FolderMove { from, to })?;
            warn_sync_issues(Workspace::refresh_existing(root, remote));
            emit(moved)
        }
        Command::Rmdir { folder, recursive } => {
            emit(Catalog::open(remote, Some(root))?.delete_folder(&folder, recursive)?)
        }
        Command::Detail { id } => emit(Catalog::open(remote, Some(root))?.detail(&id)?),
        Command::Patch {
            id,
            folder,
            name,
            tags,
            clear_tags,
        } => {
            let tags = if clear_tags { Some(Vec::new()) } else { tags };
            let moved = folder.is_some() || name.is_some();
            let updated =
                Catalog::open(remote, Some(root))?.patch(&id, &Patch { folder, name, tags })?;
            if moved {
                warn_sync_issues(Workspace::refresh_existing(root, remote));
            }
            emit(updated)
        }
        Command::Revert {
            id,
            version,
            expected_version,
        } => {
            let expected = match expected_version {
                Some(version) => version,
                None => remote.detail(&id)?.container.latest_version,
            };
            emit(Catalog::open(remote, Some(root))?.revert(&id, expected, version)?)
        }
        Command::Recycle { action } => match action.unwrap_or(RecycleCommand::Ls {
            cursor: None,
            limit: None,
        }) {
            RecycleCommand::Move { id } => emit(remote.move_to_recycle(&id)?),
            RecycleCommand::Purge { id, .. } => emit(remote.purge(id.as_ref())?),
            RecycleCommand::Ls { cursor, limit } => {
                emit(Catalog::open(remote, Some(root))?.recycle(&RecycleFilter { cursor, limit })?)
            }
        },
        Command::Restore { id, folder, name } => {
            emit(Catalog::open(remote, Some(root))?.restore(&id, &Restore { folder, name })?)
        }
        _ => Err(invalid("command does not use a remote server")),
    }
}

fn warn_sync_issues(result: Result<zxs_core::SyncReport>) {
    match result {
        Ok(report) => {
            for issue in report.issues {
                eprintln!("zxs-cli: {}: {}", issue.path, issue.reason);
            }
        }
        Err(error) => eprintln!("zxs-cli: 刷新本地工作区遇到错误 {error}"),
    }
}

fn warn_bundle_conflicts(workspace: &Workspace) {
    match workspace.bundle_warnings() {
        Ok(paths) => warn_invalid_manifests(paths.iter().map(LocalPath::as_str)),
        Err(error) => eprintln!("zxs-cli: 检查 .zxsignore 时遇到错误 {error}"),
    }
}

fn warn_invalid_manifests<'a>(paths: impl IntoIterator<Item = &'a str>) {
    for path in paths {
        let reason = if path.contains('/') {
            "目录与已有文件容器重叠"
        } else {
            "目录包须在数据盘的子目录内创建"
        };
        eprintln!("zxs-cli: 已忽略 {path}/.zxsignore，{reason}");
    }
}

fn emit_status(workspace: &Workspace) -> Result<()> {
    let statuses = workspace.status()?;
    warn_invalid_manifests(statuses.bundle_warnings.iter().map(LocalPath::as_str));
    for entry in &statuses.entries {
        if entry.remote_missing {
            eprintln!(
                "zxs-cli: {} 的远端容器已不存在。可以解除追踪或从回收站还原",
                entry.path.as_str()
            );
        } else if matches!(entry.state, FileState::Missing) {
            eprintln!(
                "zxs-cli: {} 已删除或移走。可以重新检出，或用 sync-delete 将原容器移入回收站",
                entry.path.as_str()
            );
        }
    }
    for path in &statuses.symlink_warnings {
        eprintln!("zxs-cli: 已跳过符号链接 {path}");
    }
    if statuses.truncated {
        eprintln!("zxs-cli: 已显示前 1000 条新增项。请整理剩余文件或初始化目录工作区");
    }
    emit(statuses)
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
    if io::stdin()
        .lock()
        .take(METADATA_LIMIT as u64 + 3)
        .read_line(&mut password)?
        == 0
    {
        return Err(invalid("provide the password on standard input"));
    }
    if password.ends_with('\n') {
        password.pop();
    }
    if password.ends_with('\r') {
        password.pop();
    }
    if password.len() > METADATA_LIMIT {
        return Err(invalid("password metadata exceeds 1 MiB"));
    }
    Ok(password)
}
