use std::path::PathBuf;

use super::{Detail, Pending, Recycled, Result, StageGuard, State, db};

#[derive(Clone, Copy)]
pub(super) enum Prepared<'a> {
    None,
    Restore(&'a Detail),
    RecycleTree(&'a [Recycled]),
}

impl State {
    pub(super) fn recover(&self, rebuild: bool, indexed: bool) -> Result<()> {
        let pending = self.store.recover_changes()?;
        if !pending.is_empty() {
            self.replay(&pending)?;
        } else if rebuild {
            super::maintenance::rebuild(&self.store, &self.db, indexed)?;
        }
        self.store.clean_stages()
    }

    fn replay(&self, pending: &[Pending]) -> Result<()> {
        for transaction in pending {
            let archives = self.store.recover_archives(transaction)?;
            if let Err(error) = self.store.apply(transaction, &archives) {
                tracing::error!(tx_id = %transaction.tx_id, %error, "could not finish journaled transaction");
                return Err(error);
            }
        }
        super::maintenance::rebuild(&self.store, &self.db, false)?;
        for pending in pending {
            self.store.end(pending)?;
        }
        Ok(())
    }

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
                Prepared::None => (&[][..], None),
                Prepared::Restore(detail) => (&[][..], Some(detail)),
                Prepared::RecycleTree(archives) => (archives, None),
            };
            let mut cleanup = self.store.apply(pending, archives)?;
            self.db
                .write(|tx| db::apply_change(tx, pending, restored, archives))?;
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
