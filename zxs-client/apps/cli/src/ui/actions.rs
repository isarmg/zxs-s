use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    Catalog, Checkout, CheckoutRequest, CommitRequest, ConflictChoice, ContainerId, FileName,
    Filter, FolderMove, FolderPath, Patch, Profile, RecycleFilter, RecycleId, Restore, Result,
    VersionNum, Workspace, invalid, move_workspace,
};

use super::{Reply, Ui, json_input, json_response, open_target};
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
    CreateFolder {
        folder: FolderPath,
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
    Revert {
        id: ContainerId,
        expected: VersionNum,
        version: VersionNum,
    },
    BrowseWorkspace {
        directory: PathBuf,
    },
    MoveWorkspace {
        parent: PathBuf,
        name: FileName,
    },
    Sync,
    Refresh,
    SyncDelete {
        path: PathBuf,
        expected_version: Option<VersionNum>,
    },
    Untrack {
        path: PathBuf,
    },
    Checkout {
        id: ContainerId,
        version: Option<VersionNum>,
        to: Option<PathBuf>,
        choice: Checkout,
    },
    Commit {
        path: PathBuf,
        name: Option<FileName>,
        choice: ConflictChoice,
    },
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

pub fn api(state: &Ui, request: &mut Request) -> Result<Reply> {
    let action: Action = json_input(request)?;
    if let Action::MoveWorkspace { parent, name } = action {
        let profile = Profile::load()?;
        let mut root = state
            .root
            .write()
            .map_err(|error| invalid(error.to_string()))?;
        let moved = move_workspace(&root, &parent, &name)?;
        *root = moved.path;
        return Ok(json_response(&json!({
            "workspace": info(&root, profile.as_ref()),
            "warnings": moved.warnings,
        })));
    }
    let workspace_root = state
        .root
        .read()
        .map_err(|error| invalid(error.to_string()))?;
    let root = workspace_root.as_path();
    let result = match action {
        Action::Session => session(root),
        Action::Login { server, password } => {
            let mut auth = state
                .auth
                .lock()
                .map_err(|error| invalid(error.to_string()))?;
            let connected = Profile::login(&server, &password)?;
            if let Some((_, previous)) = auth.replace(connected) {
                let _ = previous.logout();
            }
            let profile = &auth.as_ref().ok_or_else(|| invalid("connection failed"))?.0;
            Ok(info(root, Some(profile)))
        }
        Action::Logout => {
            let mut auth = state
                .auth
                .lock()
                .map_err(|error| invalid(error.to_string()))?;
            if let Some((profile, remote)) = auth.take() {
                profile.logout(&remote)?;
            } else if let Some(profile) = Profile::load()? {
                profile.clear()?;
            }
            Ok(info(root, None))
        }
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
        Action::InitBundle { path } => {
            let workspace = Workspace::open(root)?;
            let path = workspace.init_bundle(&path, &state.remote()?)?;
            Ok(json!({ "path": path }))
        }
        action => with_remote(state, root, action),
    }?;
    Ok(json_response(&result))
}

fn with_remote(state: &Ui, root: &Path, action: Action) -> Result<Value> {
    let remote = state.remote()?;
    match action {
        Action::Sync => serialize(Workspace::open(root)?.refresh(&remote)?),
        Action::Refresh => serialize(Workspace::open(root)?.refresh_tracked_paths(&remote)?),
        Action::SyncDelete {
            path,
            expected_version,
        } => serialize(Workspace::open(root)?.sync_delete(&remote, &path, expected_version)?),
        Action::Checkout {
            id,
            version,
            to,
            choice,
        } => {
            let request = CheckoutRequest {
                id,
                version,
                destination: to,
                choice,
            };
            serialize(Workspace::open(root)?.checkout(&remote, request)?)
        }
        Action::Commit { path, name, choice } => {
            let request = CommitRequest {
                path,
                folder: None,
                name,
                tags: Vec::new(),
                on_conflict: choice,
            };
            serialize(Workspace::open(root)?.commit(&remote, &request)?)
        }
        Action::CommitDirectory { directory } => {
            serialize(Workspace::open(root)?.commit_directory(&remote, &directory)?)
        }
        Action::MoveFolder { from, folder, name } => {
            let to = FolderPath::parse(format!(
                "{}/{}",
                folder.as_str().trim_end_matches('/'),
                name.as_str()
            ))?;
            let (moved, report) = Workspace::move_folder(root, &remote, &FolderMove { from, to })?;
            Ok(json!({ "folder": moved.folder, "issues": report.issues }))
        }
        Action::Patch { id, patch } => {
            let (container, report) = Workspace::patch_file(root, &remote, &id, &patch)?;
            Ok(json!({ "container": container, "issues": report.issues }))
        }
        Action::Recycle { id } => serialize(remote.move_to_recycle(&id)?),
        Action::Purge { id } => serialize(remote.purge(id.as_ref())?),
        action => catalog_action(&Catalog::open(&remote, Some(root))?, action),
    }
}

fn catalog_action(catalog: &Catalog<'_>, action: Action) -> Result<Value> {
    match action {
        Action::List { filter } => serialize(catalog.list(&filter)?),
        Action::CreateFolder { folder } => serialize(catalog.create_folder(folder)?),
        Action::DeleteFolder { folder, recursive } => {
            serialize(catalog.delete_folder(&folder, recursive)?)
        }
        Action::Detail { id } => serialize(catalog.detail(&id)?),
        Action::Revert {
            id,
            expected,
            version,
        } => serialize(catalog.revert(&id, expected, version)?),
        Action::RecycleList { filter } => serialize(catalog.recycle(&filter)?),
        Action::Restore { id, patch } => serialize(catalog.restore(&id, &patch)?),
        _ => Err(invalid("unsupported UI action")),
    }
}

fn session(root: &Path) -> Result<Value> {
    let profile = Profile::load()?;
    Ok(info(root, profile.as_ref()))
}

fn info(root: &Path, profile: Option<&Profile>) -> Value {
    json!({
        "workspace": display_path(root),
        "workspace_name": root.file_name().map(|name| name.to_string_lossy()),
        "workspace_parent": root.parent().map(display_path),
        "server": profile.map(|p| &p.endpoint),
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
