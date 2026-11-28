mod commands;
mod progress;
mod ui;

use std::{
    io::{self, BufRead, IsTerminal, Read},
    process::ExitCode,
};

use clap::Parser;
use commands::{Cli, Command, RecycleCommand};
use serde::Serialize;
use zxs_core::{
    Catalog, FileState, Filter, Folder, FolderMove, LocalPath, METADATA_LIMIT, Patch, Profile,
    RecycleFilter, Remote, Restore, Result, Workspace, configured_workspace, invalid,
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
        Command::Untrack { path } => Workspace::open(root)?.untrack(&path, None),
        command => run_remote(&root, command),
    }
}

fn workspace_path(path: Option<std::path::PathBuf>) -> Result<std::path::PathBuf> {
    if let Some(path) = path {
        return Ok(path);
    }
    if let Some(path) = configured_workspace()? {
        return Ok(path);
    }
    if let Some(path) = std::env::var_os("ZXS_WORKSPACE") {
        return Ok(path.into());
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
    let progress = progress::Terminal::default();
    let remote = profile.remote()?.with_progress(progress.observer());
    let result = execute_remote(root, &remote, command);
    progress.finish();
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
        Command::InitBundle { path } => emit(Workspace::open(root)?.init_bundle(&path, remote)?),
        Command::Sync(args) => {
            let report = args.run(root, remote)?;
            emit(&report)?;
            sync_outcome(&report)
        }
        Command::SyncDelete(args) => emit(args.run(root, remote)?),

        Command::Refresh => {
            let workspace = Workspace::open(root)?;
            let report = workspace.refresh(remote)?;
            warn_sync_issues(&report);
            emit_status(&workspace)?;
            sync_outcome(&report)
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
            ..Filter::default()
        })?),
        Command::Checkout(args) => {
            let workspace = Workspace::open(root)?;
            let result = workspace.checkout(remote, args.into())?;
            warn_bundle_conflicts(&workspace);
            emit(result)
        }
        Command::Commit(args) => {
            let workspace = Workspace::open(root)?;
            let result = workspace.commit(remote, &args.into())?;
            warn_bundle_conflicts(&workspace);
            emit(result)
        }
        command => run_container_command(root, remote, command),
    }
}

fn run_container_command(root: &std::path::Path, remote: &Remote, command: Command) -> Result<()> {
    match command {
        Command::Mkdir { folder } => emit(remote.create_folder(&Folder { folder })?.0),
        Command::MvFolder { from, to } => {
            let (moved, report) = Workspace::move_folder(root, remote, &FolderMove { from, to })?;
            warn_sync_issues(&report);
            emit(moved)
        }
        Command::Rmdir { folder, recursive } => emit(remote.delete_folder(&folder, recursive)?),
        Command::Detail { id } => emit(Catalog::open(remote, Some(root))?.detail(&id)?),
        Command::Patch {
            id,
            folder,
            name,
            tags,
            clear_tags,
        } => {
            let tags = if clear_tags { Some(Vec::new()) } else { tags };
            let (updated, report) =
                Workspace::patch_file(root, remote, &id, &Patch { folder, name, tags })?;
            warn_sync_issues(&report);
            emit(updated)
        }
        Command::Recycle { action } => match action.unwrap_or(RecycleCommand::Ls {
            cursor: None,
            limit: None,
        }) {
            RecycleCommand::Move { id } => emit(remote.recycle_file(&id, None)?),
            RecycleCommand::Purge { id, .. } => emit(remote.purge(id.as_ref())?),
            RecycleCommand::Ls { cursor, limit } => {
                emit(Catalog::open(remote, Some(root))?.recycle(&RecycleFilter { cursor, limit })?)
            }
        },
        Command::Restore { id, folder, name } => {
            let (restored, report) =
                Workspace::restore_file(root, remote, &id, &Restore { folder, name })?;
            warn_sync_issues(&report);
            emit(restored)
        }
        _ => Err(invalid("command does not use a remote server")),
    }
}

fn warn_sync_issues(report: &zxs_core::SyncReport) {
    for issue in &report.issues {
        eprintln!("zxs-cli: {}: {}", issue.path, issue.reason);
    }
}

fn sync_outcome(report: &zxs_core::SyncReport) -> Result<()> {
    if report.issues.is_empty() {
        Ok(())
    } else {
        Err(invalid(format!("同步有 {} 项失败", report.issues.len())))
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
            eprintln!("zxs-cli: {} 的远端容器当前不可用", entry.path.as_str());
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
