use super::{Pending, Recycled, Result, StageGuard, State, db};

impl State {
    pub(super) fn recover(&self, rebuild: bool) -> Result<()> {
        let pending = self.store.recover_changes()?;
        for transaction in &pending {
            self.store.apply(transaction)?;
            if !rebuild {
                self.apply_index(transaction, &self.store.archived_entries(transaction)?)?;
            }
        }
        if rebuild {
            self.rebuild()?;
        }
        for pending in &pending {
            self.store.end(pending)?;
        }
        self.store.clean_stages()
    }

    pub(super) fn transact(&self, pending: &Pending, stages: &mut [StageGuard]) -> Result<()> {
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
            self.store.apply(pending)?;
            self.apply_index(pending, &self.store.archived_entries(pending)?)?;
            self.store.end(pending)
        })())
    }

    pub(super) fn complete(&self, pending: &Pending) {
        if let Err(error) = self.store.finish(pending) {
            tracing::warn!(tx_id = %pending.tx_id, %error, "temporary storage cleanup deferred");
        }
    }

    fn apply_index(&self, pending: &Pending, archived: &[Recycled]) -> Result<()> {
        let restored = if let super::Change::Restore { container, .. } = &pending.change {
            Some(
                self.store
                    .snapshot(&self.store.paths.container(container))?,
            )
        } else {
            None
        };
        self.db
            .write(|tx| db::apply_change(tx, pending, restored.as_ref(), archived))
    }
}
