use std::path::PathBuf;

use crate::domain::RecyclePlan;

use super::{Detail, Pending, Result, StageGuard, State, db};

#[derive(Clone, Copy)]
pub(super) enum Prepared<'a> {
    None,
    Restore(&'a Detail),
    RecycleTree(&'a [RecyclePlan]),
}

impl State {
    pub(super) fn recover(&self, rebuild: bool, indexed: bool) -> Result<()> {
        let pending = self.store.recover_changes()?;
        for transaction in &pending {
            let archives = self.store.recover_archives(transaction)?;
            self.store.apply(transaction, &archives)?;
            if indexed {
                self.apply_index(transaction, &archives, None)?;
            }
        }
        if rebuild {
            if indexed {
                self.store.check_indexed_paths()?;
            }
            super::maintenance::rebuild(&self.store, &self.db)?;
        }
        for pending in &pending {
            self.store.end(pending)?;
        }
        self.store.clean_stages()
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
            self.apply_index(pending, archives, restored)?;
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

    fn apply_index(
        &self,
        pending: &Pending,
        archived: &[RecyclePlan],
        restored: Option<&Detail>,
    ) -> Result<()> {
        let recovered =
            if let (None, super::Change::Restore { container, .. }) = (restored, &pending.change) {
                Some(
                    self.store
                        .snapshot(&self.store.paths.container(container))?,
                )
            } else {
                None
            };
        self.db
            .write(|tx| db::apply_change(tx, pending, restored.or(recovered.as_ref()), archived))
    }
}
