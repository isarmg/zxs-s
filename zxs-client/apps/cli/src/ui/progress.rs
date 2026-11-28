use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use zxs_core::{Result, TransferObserver, TransferProgress, invalid};

use super::{Reply, json_response};

#[derive(Default)]
pub(super) struct Transfers(Arc<Mutex<HashMap<String, Snapshot>>>);

#[derive(Default)]
struct Snapshot {
    sample: Option<TransferProgress>,
    finished: Option<Instant>,
}

impl Transfers {
    pub(super) fn start(&self, id: Option<&str>) -> Result<Option<TransferObserver>> {
        let Some(id) = id else { return Ok(None) };
        if id.len() != 36
            || !id
                .bytes()
                .all(|byte| byte == b'-' || byte.is_ascii_hexdigit())
        {
            return Err(invalid("invalid transfer ID"));
        }
        let id = id.to_owned();
        let mut jobs = self.0.lock().map_err(|error| invalid(error.to_string()))?;
        jobs.retain(|_, job| {
            job.finished
                .is_none_or(|at| at.elapsed() < Duration::from_secs(60))
        });
        jobs.insert(id.clone(), Snapshot::default());
        let jobs = Arc::clone(&self.0);
        Ok(Some(Arc::new(move |sample| {
            if let Ok(mut jobs) = jobs.lock()
                && let Some(job) = jobs.get_mut(&id)
            {
                job.sample = Some(sample);
            }
        })))
    }

    pub(super) fn read(&self, id: &str) -> Result<Reply> {
        let jobs = self.0.lock().map_err(|error| invalid(error.to_string()))?;
        let sample = jobs.get(id).and_then(|job| job.sample);
        Ok(json_response(&serde_json::to_value(sample)?))
    }

    pub(super) fn finish(&self, id: Option<&str>) {
        if let Some(id) = id
            && let Ok(mut jobs) = self.0.lock()
            && let Some(job) = jobs.get_mut(id)
        {
            job.finished = Some(Instant::now());
        }
    }
}
