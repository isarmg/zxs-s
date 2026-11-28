use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    domain::{Blake3Hash, BundlePlan, ContentHashes, Snapshot, invalid},
    service::inspect,
    store::{
        digest,
        layout::{TempDir, check_unique_name, kind, move_new, parent, publish_staged_file, stamp},
    },
    transport::DownloadVersion,
    zip_bundle::{self, Bundle, BundleInstall},
};

use super::{
    CheckoutResult, ContainerId, EntryKind, Error, Install, InterruptedCheckout, Remote, Result,
    Revision, Track, Workspace, preflight::occupied_path,
};

pub(super) struct Publication<'a> {
    pub remote: &'a Remote,
    pub id: ContainerId,
    pub install: Install,
    pub version: &'a DownloadVersion,
    pub restored_against: Option<Revision>,
}

struct Retirement {
    source: PathBuf,
    backup: PathBuf,
    bundle: Option<BundlePlan>,
}

impl Workspace {
    pub(super) fn checkout_regular_file(&self, job: Publication<'_>) -> Result<CheckoutResult> {
        let path = &job.install.path;
        if !job.install.confirmed
            && job.install.prior_here().is_some_and(|track| {
                matches!(&track.hashes, ContentHashes::File(hash) if hash == &job.version.blake3)
            })
            && kind(path)? == Some(EntryKind::File)
        {
            let stamp = stamp(path)?;
            let hash = digest::file(path)?;
            job.install.verify_file(Some(&hash))?;
            if job.version.size.is_some_and(|size| size != stamp.size) {
                return Err(invalid("local file length does not match the selected version"));
            }
            return self.record_checkout(job, (Snapshot::File(stamp), ContentHashes::File(hash)), None);
        }
        let stage = self.checkout_stage(path, false)?;
        job.remote
            .download_version(&job.id, job.version, &stage.child("payload"))?;
        self.publish_file(job, &stage)
    }

    pub(super) fn publish_file(
        &self,
        job: Publication<'_>,
        stage: &TempDir,
    ) -> Result<CheckoutResult> {
        let path = &job.install.path;
        let hash = &job.version.blake3;
        let current = if kind(path)? == Some(EntryKind::File) {
            Some(digest::file(path)?)
        } else {
            None
        };
        job.install.verify_file(current.as_ref())?;
        let backup = if current.as_ref() == Some(hash) && job.install.source_elsewhere().is_none() {
            job.install.previous_backup(self)
        } else {
            let retirement = self.prepare_retirement(&job.install)?;
            let backup = self.start_publication(
                &job,
                retirement.as_ref().map(|entry| entry.backup.as_path()),
            )?;
            if let Some(retirement) = retirement {
                retirement.apply()?;
            }
            fs::create_dir_all(parent(path)?)?;
            let result = publish_staged_file(&stage.child("payload"), path, &stage.id()?, hash);
            report_publication(result, backup.as_deref())?;
            backup
        };
        let snapshot = Snapshot::File(stamp(path)?);
        let hashes = ContentHashes::File(hash.clone());
        self.record_checkout(job, (snapshot, hashes), backup)
    }

    pub(super) fn publish_bundle(
        &self,
        job: Publication<'_>,
        stage: &TempDir,
        mut archive: Bundle,
    ) -> Result<CheckoutResult> {
        let path = &job.install.path;
        let expanded = stage.child("expanded");
        let prior = job.install.prior_here();
        let plan = zip_bundle::plan_bundle(
            path,
            &expanded,
            &mut archive,
            prior.map(|track| &track.snapshot),
            job.install.confirmed && job.install.owns_target(),
        )?;
        if !job.install.confirmed
            && let Some(ContentHashes::Bundle(hashes)) = prior.map(|track| &track.hashes)
            && plan
                .files
                .iter()
                .any(|(member, file)| file.old.is_some() && hashes.get(member) != file.old.as_ref())
        {
            return Err(Error::CheckoutConflict);
        }
        let retirement = self.prepare_retirement(&job.install)?;
        let replacing_members = plan
            .files
            .values()
            .any(|file| file.old.is_some() && file.old != file.new);
        if replacing_members && retirement.is_some() {
            return Err(occupied_path(path));
        }
        let bundle_backup = replacing_members
            .then(|| self.layout.checkout_backup(path))
            .transpose()?;
        let backup = self.start_publication(
            &job,
            bundle_backup
                .as_deref()
                .or(retirement.as_ref().map(|entry| entry.backup.as_path())),
        )?;
        if let Some(retirement) = retirement {
            retirement.apply()?;
        }
        let result = zip_bundle::apply_bundle(
            path,
            &plan,
            BundleInstall {
                stage: &expanded,
                slot: &stage.id()?,
                backup: bundle_backup.as_deref(),
            },
        );
        let contents = report_publication(result, backup.as_deref())?;
        self.record_checkout(job, contents, backup)
    }

    fn prepare_retirement(&self, install: &Install) -> Result<Option<Retirement>> {
        let elsewhere = install
            .source_elsewhere()
            .map(|path| kind(path).map(|entry| (path, entry)))
            .transpose()?;
        let (source, entry) = if let Some((source, Some(entry))) = elsewhere {
            check_unique_name(source)?;
            if kind(&install.path)? == Some(EntryKind::File) {
                return Err(occupied_path(&install.path));
            }
            (source, entry)
        } else {
            let Some(entry) = kind(&install.path)? else {
                return Ok(None);
            };
            if entry == EntryKind::Directory && install.kind == EntryKind::Directory {
                return Ok(None);
            }
            if !install.owns_target() {
                return Err(occupied_path(&install.path));
            }
            (&*install.path, entry)
        };
        if !install.confirmed
            && let Some(track) = &install.previous
            && (source != install.path || entry != install.kind)
            && !inspect::clean_contents(&self.layout, track)?
        {
            return Err(Error::CheckoutConflict);
        }
        let bundle = if entry == EntryKind::Directory {
            Some(zip_bundle::plan_retirement(source)?)
        } else {
            None
        };
        let storage = if bundle.is_some() {
            source
        } else {
            parent(source)?
        };
        Ok(Some(Retirement {
            source: source.into(),
            backup: self.layout.checkout_backup(storage)?,
            bundle,
        }))
    }

    fn start_publication(
        &self,
        job: &Publication<'_>,
        retaining: Option<&Path>,
    ) -> Result<Option<PathBuf>> {
        let backup = retaining
            .map(Path::to_path_buf)
            .or_else(|| job.install.previous_backup(self));
        let relative = backup
            .as_ref()
            .and_then(|path| path.strip_prefix(&self.layout.root).ok())
            .map(Path::to_path_buf);
        self.db.start_checkout(&InterruptedCheckout {
            path: job.install.local.clone(),
            container_id: job.id.clone(),
            version: job.version.number,
            endpoint: job.remote.endpoint().into(),
            kind: job.install.kind,
            mirrored: Some(job.install.mirrored),
            payload_hash: Some(job.version.blake3.clone()),
            restored_against: job.restored_against.clone(),
            backup: relative,
        })?;
        Ok(backup)
    }

    fn record_checkout(
        &self,
        job: Publication<'_>,
        (snapshot, hashes): (Snapshot, ContentHashes),
        backup: Option<PathBuf>,
    ) -> Result<CheckoutResult> {
        let bundle = matches!(snapshot, Snapshot::Bundle(_));
        let restored_from = job.restored_against.as_ref().map(|_| job.version.number);
        let (version, blake3) = job.restored_against.map_or_else(
            || {
                (
                    job.version.number,
                    bundle.then(|| job.version.blake3.clone()),
                )
            },
            |base| (base.version, base.blake3),
        );
        self.db.save_checkout(
            &Track {
                path: job.install.local.clone(),
                endpoint: job.remote.endpoint().into(),
                container_id: job.id,
                version,
                remote_blake3: blake3,
                restored_from,
                snapshot,
                hashes,
                remote_missing: false,
                mirrored: job.install.mirrored,
            },
            job.install.previous.as_ref().map(|entry| &entry.path),
        )?;
        Ok(CheckoutResult {
            path: job.install.path,
            local: job.install.local,
            version: job.version.number,
            backup,
            bundle,
            restored_from,
        })
    }
}

impl Install {
    fn verify_file(&self, current: Option<&Blake3Hash>) -> Result<()> {
        let Some(current) = current else {
            return Ok(());
        };
        if self.confirmed && self.owns_target() {
            return Ok(());
        }
        if let Some(track) = self.prior_here() {
            if matches!(&track.hashes, ContentHashes::File(hash) if hash == current) {
                return Ok(());
            }
            return Err(Error::CheckoutConflict);
        }
        Err(occupied_path(&self.path))
    }
}

fn report_publication<T>(result: Result<T>, backup: Option<&Path>) -> Result<T> {
    result.map_err(|error| match backup {
        Some(path) if path.exists() => {
            Error::LocalConflict(format!("{error}。原件保留于 {}", path.display()))
        }
        _ => error,
    })
}

impl Retirement {
    fn apply(self) -> Result<()> {
        if let Some(plan) = self.bundle {
            zip_bundle::retain_members(&self.source, &plan, Some(&self.backup))?;
            zip_bundle::remove_empty(&self.source)
        } else {
            move_new(&self.source, &self.backup)
        }
    }
}
