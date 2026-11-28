use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    Catalog, ContainerId, FileName, Filter, FolderMove, FolderPath, Patch, Profile, RecycleFilter,
    RecycleId, Remote, Restore, Result, TransferObserver, Workspace, invalid, select_workspace,
};

use super::{Reply, Ui, json_input, json_response, open_target, read_header};
use crate::commands::{CheckoutArgs, CommitArgs, SyncArgs, SyncDeleteArgs};
use std::path::{Path, PathBuf};
use tiny_http::Request;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Session,
    Login {
        server: String,
        password: String,
    },
    Logout,
    List {
        filter: Filter,
    },
    MoveFolder {
        from: FolderPath,
        folder: FolderPath,
        name: FileName,
    },
    DeleteFolder {
        folder: FolderPath,
        recursive: bool,
    },
    Detail {
        id: ContainerId,
    },
    RecycleList {
        filter: RecycleFilter,
    },
    Purge {
        id: Option<RecycleId>,
    },
    Recycle {
        id: ContainerId,
    },
    Restore {
        id: RecycleId,
        patch: Restore,
    },
    Patch {
        id: ContainerId,
        patch: Patch,
    },
    BrowseWorkspace {
        directory: PathBuf,
    },
    SelectWorkspace {
        path: PathBuf,
    },
    Sync(SyncArgs),
    SyncDelete(SyncDeleteArgs),
    Untrack {
        path: PathBuf,
    },
    Checkout(CheckoutArgs),
    Commit(CommitArgs),
    CommitDirectory {
        directory: PathBuf,
    },
    InitBundle {
        path: PathBuf,
    },
    Open {
        path: PathBuf,
    },
}

pub fn api(state: &Ui, request: &mut Request, observer: Option<TransferObserver>) -> Result<Reply> {
    let action: Action = json_input(request)?;
    let target = read_header(request, "x-zxs-target");
    if let Action::SelectWorkspace { path } = action {
        let mut root = state
            .root
            .write()
            .map_err(|error| invalid(error.to_string()))?;
        let profile = state.targeted_profile(&root, target)?;
        *root = select_workspace(&path)?;
        return Ok(json_response(&info(state, &root, profile.as_ref())));
    }
    let workspace_root = state
        .root
        .read()
        .map_err(|error| invalid(error.to_string()))?;
    let root = workspace_root.as_path();
    let profile = match action {
        Action::Session => Profile::load()?,
        Action::Login { .. } => None,
        _ => state.targeted_profile(root, target)?,
    };
    let result = match action {
        Action::Session => Ok(info(state, root, profile.as_ref())),
        Action::Login { server, password } => login(state, root, &server, &password),
        Action::Logout => logout(state, root, profile.as_ref()),
        Action::BrowseWorkspace { directory } => {
            let workspace = Workspace::open(root)?;
            serialize(workspace.browse(&directory)?)
        }
        Action::Untrack { path } => {
            Workspace::open(root)?.untrack(&path)?;
            Ok(json!({}))
        }
        Action::Open { path } => {
            let workspace = Workspace::open(root)?;
            let path = workspace.local_path(&path)?;
            if !path.exists() {
                return Err(invalid("local path does not exist"));
            }
            open_target(&path)?;
            Ok(json!({}))
        }
        action => with_remote(
            &state.remote(profile)?.with_progress(observer),
            root,
            action,
        ),
    }?;
    Ok(json_response(&result))
}

fn login(state: &Ui, root: &Path, server: &str, password: &str) -> Result<Value> {
    let mut auth = state
        .auth
        .lock()
        .map_err(|error| invalid(error.to_string()))?;
    let connected = Profile::login(server, password)?;
    let session = info(state, root, Some(&connected.0));
    if let Some((_, previous)) = auth.replace(connected) {
        let _ = previous.logout();
    }
    Ok(session)
}

fn logout(state: &Ui, root: &Path, profile: Option<&Profile>) -> Result<Value> {
    let mut auth = state
        .auth
        .lock()
        .map_err(|error| invalid(error.to_string()))?;
    if let Some(profile) = profile {
        profile.clear()?;
    }
    if auth.as_ref().map(|(profile, _)| profile) == profile
        && let Some((_, remote)) = auth.take()
    {
        let _ = remote.logout();
    }
    Ok(info(state, root, None))
}

fn with_remote(remote: &Remote, root: &Path, action: Action) -> Result<Value> {
    match action {
        Action::InitBundle { path } => {
            let path = Workspace::open(root)?.init_bundle(&path, remote)?;
            Ok(json!({ "path": path }))
        }
        Action::Sync(args) => serialize(args.run(root, remote)?),
        Action::SyncDelete(args) => serialize(args.run(root, remote)?),
        Action::Checkout(args) => serialize(Workspace::open(root)?.checkout(remote, args.into())?),
        Action::Commit(args) => serialize(Workspace::open(root)?.commit(remote, &args.into())?),
        Action::CommitDirectory { directory } => {
            serialize(Workspace::open(root)?.commit_directory(remote, &directory)?)
        }
        Action::MoveFolder { from, folder, name } => {
            let to = FolderPath::parse(format!(
                "{}/{}",
                folder.as_str().trim_end_matches('/'),
                name.as_str()
            ))?;
            let (moved, report) = Workspace::move_folder(root, remote, &FolderMove { from, to })?;
            Ok(json!({ "folder": moved.folder, "issues": report.issues }))
        }
        Action::Patch { id, patch } => {
            let (container, report) = Workspace::patch_file(root, remote, &id, &patch)?;
            Ok(json!({ "container": container, "issues": report.issues }))
        }
        Action::Recycle { id } => serialize(remote.move_to_recycle(&id)?),
        Action::Purge { id } => serialize(remote.purge(id.as_ref())?),
        Action::DeleteFolder { folder, recursive } => {
            serialize(remote.delete_folder(&folder, recursive)?)
        }
        Action::Restore { id, patch } => {
            let (restored, report) = Workspace::restore_file(root, remote, &id, &patch)?;
            Ok(json!({ "container": restored, "issues": report.issues }))
        }
        Action::List { filter } => serialize(Catalog::open(remote, Some(root))?.list(&filter)?),
        Action::Detail { id } => serialize(Catalog::open(remote, Some(root))?.detail(&id)?),
        Action::RecycleList { filter } => {
            serialize(Catalog::open(remote, Some(root))?.recycle(&filter)?)
        }
        _ => Err(invalid("unsupported UI action")),
    }
}

fn info(state: &Ui, root: &Path, profile: Option<&Profile>) -> Value {
    json!({
        "workspace": display_path(root),
        "workspace_name": root.file_name().map(|name| name.to_string_lossy()),
        "server": profile.map(|p| &p.endpoint),
        "target": state.target(root, profile),
    })
}

fn display_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if cfg!(windows) {
        if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(drive) = value.strip_prefix(r"\\?\")
            && drive.as_bytes().get(1) == Some(&b':')
        {
            return drive.into();
        }
    }
    value.into_owned()
}

fn serialize(value: impl serde::Serialize) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}
