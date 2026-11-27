use std::net::SocketAddr;

use super::{Error, Result, State, password_hash};
use crate::{store::Config, test_support::TestDir};

const PASSWORD: &str = "local-test-password";

#[test]
fn restart_revokes_sessions() -> Result<()> {
    let root = TestDir::create()?;
    let store = root.store()?;
    let config = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        recycle_dir: store.paths.recycle.clone(),
        password_hash: password_hash(PASSWORD)?,
        data_disks: store.paths.data_disks.clone(),
        recycle_disks: store.paths.recycle_disks.clone(),
    };
    store.save_config(&config)?;
    let state = State::open(store.clone(), config.clone(), store.lock()?, true)?;
    let first = state.login(PASSWORD)?;
    let second = state.login(PASSWORD)?;
    assert_ne!(first.token, second.token);
    state.authorize(&first.token)?;
    state.logout(&first.token)?;
    assert!(matches!(
        state.logout(&first.token),
        Err(Error::Unauthorized)
    ));
    state.authorize(&second.token)?;
    drop(state);
    let state = State::open(store.clone(), config, store.lock()?, true)?;
    assert!(matches!(
        state.authorize(&second.token),
        Err(Error::Unauthorized)
    ));
    state.login(PASSWORD)?;
    Ok(())
}
