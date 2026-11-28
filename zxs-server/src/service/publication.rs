use std::path::PathBuf;

use super::{Db, Detail, Pending, Recycled, Result, StageGuard, State, Store, db};

#[derive(Clone, Copy)]
pub(super) enum Prepared<'a> {
    None,
    Restore(&'a Detail),
    RecycleTree(&'a [Recycled]),
}

pub(super) fn recover(store: &Store, db: &Db, rebuild: bool, indexed: bool) -> Result<()> {
    let pending = store.pending_changes()?;
    for transaction in &pending {
        if let Err(error) = store.apply(transaction, None) {
            tracing::error!(tx_id = %transaction.tx_id, %error, "could not finish journaled transaction");
            return Err(error);
        }
    }
    if rebuild || !pending.is_empty() {
        super::maintenance::rebuild(store, db, indexed && pending.is_empty())?;
    }
    for transaction in &pending {
        store.end(transaction)?;
    }
    store.clean_stages()
}

impl State {
    pub(super) fn transact(
        &self,
        pending: &Pending,
        stages: &mut [StageGuard],
        prepared: Prepared<'_>,
    ) -> Result<Vec<PathBuf>> {
        let mut metadata = self.store.prepare_metadata(pending)?;
        let recorded = self.store.start(pending);
        if recorded.is_ok() || !matches!(self.store.journal(pending).try_exists(), Ok(false)) {
            for stage in stages.iter_mut().chain(metadata.iter_mut()) {
                stage.journaled();
            }
            self.mark_on_error(recorded)?;
        } else {
            recorded?;
        }
        self.mark_on_error((|| {
            let (archives, restored) = match prepared {
                Prepared::None => (None, None),
                Prepared::Restore(detail) => (None, Some(detail)),
                Prepared::RecycleTree(archives) => (Some(archives), None),
            };
            let mut cleanup = self.store.apply(pending, archives)?;
            self.db.write(|tx| {
                db::apply_change(tx, pending, restored, archives.unwrap_or_default())
            })?;
            self.store.end(pending)?;
            cleanup.extend(
                stages
                    .iter()
                    .chain(metadata.iter())
                    .map(|stage| stage.path.clone()),
            );
            Ok(cleanup)
        })())
    }
}
