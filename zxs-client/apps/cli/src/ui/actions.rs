use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    Checkout, CheckoutRequest, CommitRequest, ConflictChoice, ContainerId, FileName, Filter,
    FolderPath, Patch, Profile, RecycleFilter, RecycleId, Restore, Result, VersionNum, Workspace,
    init_bundle, invalid,
};

use super::{Reply, Ui, json_input, json_response, open_target};
use std::path::PathBuf;
use tiny_http::Request;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Session,
    Login {
        server: String,
        password: String,
        allow_public_http: bool,
    },
    Logout,
    List {
        filter: Filter,
    },
    Detail {
        id: ContainerId,
    },
    RecycleList {
        filter: RecycleFilter,
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
    Status,
    Checkout {
        id: ContainerId,
        version: Option<VersionNum>,
        to: Option<PathBuf>,
        choice: Checkout,
    },
    Commit {
        path: PathBuf,
        folder: Option<FolderPath>,
        name: Option<FileName>,
        tags: Vec<String>,
        choice: ConflictChoice,
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
    let result = match action {
        Action::Session => session(state),
        Action::Login {
            server,
            password,
            allow_public_http,
        } => {
            let mut auth = state
                .auth
                .lock()
                .map_err(|error| invalid(error.to_string()))?;
            *auth = Some(Profile::login(&server, allow_public_http, &password)?);
            let profile = &auth.as_ref().ok_or_else(|| invalid("connection failed"))?.0;
            Ok(info(state, Some(profile)))
        }
        Action::Logout => {
            let mut auth = state
                .auth
                .lock()
                .map_err(|error| invalid(error.to_string()))?;
            if let Some((profile, remote)) = auth.as_ref() {
                profile.logout(remote)?;
            } else if let Some(profile) = Profile::load()? {
                profile.logout(&profile.remote()?)?;
            }
            *auth = None;
            Ok(info(state, None))
        }
        Action::Status => {
            let workspace = Workspace::open(&state.root)?;
            if let Ok(remote) = state.remote() {
                workspace.refresh(&remote)?;
            }
            serialize(workspace.status()?)
        }
        Action::Open { path } => {
            let workspace = Workspace::open(&state.root)?;
            let path = workspace.local_path(&path)?;
            if !path.exists() {
                return Err(invalid("local path does not exist"));
            }
            open_target(&path)?;
            Ok(json!({}))
        }
        Action::InitBundle { path } => {
            let workspace = Workspace::open(&state.root)?;
            init_bundle(&workspace.local_path(&path)?)?;
            Ok(json!({}))
        }
        action => with_remote(state, action),
    }?;
    Ok(json_response(&result))
}

fn with_remote(state: &Ui, action: Action) -> Result<Value> {
    let remote = state.remote()?;
    match action {
        Action::List { filter } => serialize(remote.list(&filter)?),
        Action::Detail { id } => serialize(remote.detail(&id)?),
        Action::RecycleList { filter } => serialize(remote.recycle(&filter)?),
        Action::Recycle { id } => serialize(remote.move_to_recycle(&id)?),
        Action::Restore { id, patch } => serialize(remote.restore(&id, &patch)?),
        Action::Patch { id, patch } => serialize(remote.patch(&id, &patch)?),
        Action::Revert {
            id,
            expected,
            version,
        } => serialize(remote.revert(&id, expected, version)?),
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
            serialize(Workspace::open(&state.root)?.checkout(&remote, request)?)
        }
        Action::Commit {
            path,
            folder,
            name,
            tags,
            choice,
        } => {
            let request = CommitRequest {
                path,
                folder,
                name,
                tags,
                on_conflict: choice,
            };
            serialize(Workspace::open(&state.root)?.commit(&remote, &request)?)
        }
        _ => Err(invalid("unsupported UI action")),
    }
}

fn session(state: &Ui) -> Result<Value> {
    let profile = Profile::load()?;
    Ok(info(state, profile.as_ref()))
}

fn info(state: &Ui, profile: Option<&Profile>) -> Value {
    json!({ "workspace": state.root, "server": profile.map(|p| &p.endpoint),
        "expires_at": profile.map(|p| p.session.expires_at) })
}

fn serialize(value: impl serde::Serialize) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}
