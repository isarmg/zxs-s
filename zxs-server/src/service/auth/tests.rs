use std::net::SocketAddr;

use serde_json::json;

use crate::{
    store::{Config, Layout, Store},
    test_support::TestDir,
};

use super::{
    Error, OpId, Record, Result, SessionEffect, SessionToken, State, db, digest, fingerprint,
    password_hash, session_key,
};

const PASSWORD: &str = "local-test-password";

fn open_state(root: &TestDir) -> Result<State> {
    let store = Store {
        paths: Layout {
            data: root.0.join("data"),
            recycle: root.0.join("recycle"),
        },
    };
    let config = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        recycle_dir: store.paths.recycle.clone(),
        password_hash: password_hash(PASSWORD)?,
        session_key: session_key()?,
    };
    store.prepare()?;
    let lock = store.lock()?;
    State::open(store, config, lock, false)
}

fn expired_session(state: &State, op: &OpId) -> Result<SessionToken> {
    let token = state.token_for(op)?;
    let expires_at = 1;
    let mut record = Record::new(
        op.clone(),
        fingerprint(&("login", &state.generation))?,
        201,
        json!({ "expires_at": expires_at }),
    )?;
    let effect = SessionEffect::Login {
        token_blake3: digest(token.as_str().as_bytes())?,
        expires_at,
        generation: state.generation.clone(),
    };
    record.session = Some(effect.clone());
    state.db.write(|tx| {
        db::put_record(tx, &record)?;
        db::put_session_effect(tx, &effect, &state.generation, 0)
    })?;
    Ok(token)
}

#[test]
fn inactive_sessions_do_not_disclose_tokens_or_other_receipts() -> Result<()> {
    let root = TestDir::create()?;
    let state = open_state(&root)?;
    let login = OpId::parse("1".repeat(32))?;
    let logout = OpId::parse("2".repeat(32))?;
    let expired = OpId::parse("3".repeat(32))?;
    let unused = OpId::parse("4".repeat(32))?;

    let receipt = state.login(login.clone(), PASSWORD)?;
    let token: SessionToken = serde_json::from_value(receipt.result["token"].clone())?;
    state.authorize(&token)?;
    assert_eq!(state.logout(logout.clone(), &token)?.status, 204);
    assert_eq!(state.logout(logout.clone(), &token)?.status, 204);

    let expired_token = expired_session(&state, &expired)?;
    for token in [&token, &expired_token] {
        assert!(matches!(state.authorize(token), Err(Error::Unauthorized)));
    }
    for op in [&login, &logout, &unused] {
        assert!(matches!(
            state.logout(op.clone(), &expired_token),
            Err(Error::Unauthorized)
        ));
    }
    for op in [login, expired] {
        assert!(state.receipt(&op)?.result.get("token").is_none());
        assert!(matches!(
            state.login(op, PASSWORD),
            Err(Error::Unauthorized)
        ));
    }
    Ok(())
}
