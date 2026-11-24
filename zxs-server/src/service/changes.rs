use super::{
    Admission, Change, ContainerId, ContainerPage, Detail, Error, File, Filter, OpId, Patch,
    Pending, Receipt, Record, RecycleFilter, RecycleId, RecycleInfo, RecyclePage, Recycled,
    Restore, Result, State, Version, VersionNum, fingerprint, hex, normalize_tags, now, random,
};

impl State {
    pub fn list(&self, filter: Filter) -> Result<ContainerPage> {
        self.healthy()?;
        self.db.containers(filter)
    }

    pub fn detail(&self, id: &ContainerId) -> Result<Detail> {
        self.healthy()?;
        self.db
            .detail(id)?
            .ok_or(Error::Missing("container not found"))
    }

    pub fn recycle_list(&self, filter: &RecycleFilter) -> Result<RecyclePage> {
        self.healthy()?;
        self.db.recycle(filter)
    }

    pub fn open_payload(&self, id: &ContainerId, version: VersionNum) -> Result<(Version, File)> {
        self.with_container(id, || {
            let version = self
                .db
                .version(id, version)?
                .ok_or(Error::Missing("version not found"))?;
            let file = File::open(self.store.paths.payload(id, &version))?;
            if file.metadata()?.len() != version.size {
                return Err(Error::Recovery);
            }
            Ok((version, file))
        })
    }

    pub fn patch(&self, op: OpId, id: &ContainerId, patch: Patch) -> Result<Receipt> {
        let fingerprint = fingerprint(&("patch", id, &patch))?;
        let _op = match self.admit(&op, &fingerprint)? {
            Admission::Replay(receipt) => return Ok(receipt),
            Admission::New(op) => op,
        };
        self.mutate(id, || {
            let mut container = self.container(id)?;
            if let Some(folder) = patch.folder {
                container.folder = folder;
            }
            if let Some(name) = patch.name {
                container.name = name;
            }
            if let Some(tags) = patch.tags {
                container.tags = normalize_tags(tags)?;
            }
            self.available(&container.folder, &container.name, Some(id))?;
            container.updated_at = now();
            let record = Record::new(op, fingerprint, 200, &container)?;
            self.change(Change::Patch { container }, record)
        })
    }

    pub fn recycle(&self, op: OpId, id: &ContainerId) -> Result<Receipt> {
        let fingerprint = fingerprint(&("recycle", id))?;
        let _op = match self.admit(&op, &fingerprint)? {
            Admission::Replay(receipt) => return Ok(receipt),
            Admission::New(op) => op,
        };
        self.mutate(id, || {
            let container = self.container(id)?;
            let recycle = RecycleInfo {
                recycle_id: RecycleId::parse(hex(random()?))?,
                op_id: op.clone(),
                recycled_at: now(),
            };
            let entry = Recycled { recycle, container };
            let record = Record::new(op, fingerprint, 200, &entry)?;
            self.change(Change::Recycle { entry }, record)
        })
    }

    pub fn restore(&self, op: OpId, recycle_id: RecycleId, restore: Restore) -> Result<Receipt> {
        let fingerprint = fingerprint(&("restore", &recycle_id, &restore))?;
        let _op = match self.admit(&op, &fingerprint)? {
            Admission::Replay(receipt) => return Ok(receipt),
            Admission::New(op) => op,
        };
        let entry = self
            .db
            .recycled(&recycle_id)?
            .ok_or(Error::Missing("recycle entry not found"))?;
        let id = entry.container.id.clone();
        self.mutate(&id, || {
            let entry = self
                .db
                .recycled(&recycle_id)?
                .ok_or(Error::Missing("recycle entry not found"))?;
            let mut container = entry.container;
            if let Some(folder) = restore.folder {
                container.folder = folder;
            }
            if let Some(name) = restore.name {
                container.name = name;
            }
            self.available(&container.folder, &container.name, None)?;
            container.updated_at = now();
            let record = Record::new(op, fingerprint, 200, &container)?;
            self.change(
                Change::Restore {
                    container,
                    recycle_id,
                },
                record,
            )
        })
    }

    fn change(&self, change: Change, record: Record) -> Result<Receipt> {
        let pending = Pending {
            change,
            record: Some(record.clone()),
        };
        self.transact(&pending, None)?;
        Ok(record.receipt)
    }
}
