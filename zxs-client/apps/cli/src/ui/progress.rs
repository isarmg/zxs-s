use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use zxs_core::{Result, TransferObserver, TransferProgress, invalid};

use super::{Reply, json_response};

#[derive(Default)]
pub(super) struct Transfers(Mutex<HashMap<String, Job>>);

#[derive(Clone, Default)]
pub(super) struct Job(Arc<Mutex<Snapshot>>);

#[derive(Default)]
struct Snapshot {
    sample: Option<TransferProgress>,
    finished: Option<Instant>,
}

impl Transfers {
    pub(super) fn start(&self, id: Option<&str>) -> Result<Option<Job>> {
        let Some(id) = id else { return Ok(None) };
        if id.len() != 36
            || !id
                .bytes()
                .all(|byte| byte == b'-' || byte.is_ascii_hexdigit())
        {
            return Err(invalid("invalid transfer ID"));
        }
        let mut jobs = self.0.lock().map_err(|error| invalid(error.to_string()))?;
        jobs.retain(|_, job| job.recent());
        let job = Job::default();
        jobs.insert(id.to_owned(), job.clone());
        Ok(Some(job))
    }

    pub(super) fn read(&self, id: &str) -> Result<Reply> {
        let job = self
            .0
            .lock()
            .map_err(|error| invalid(error.to_string()))?
            .get(id)
            .cloned();
        let sample = job.map_or(Ok(None), |job| job.sample())?;
        Ok(json_response(&serde_json::to_value(sample)?))
    }
}

impl Job {
    pub(super) fn observer(&self) -> TransferObserver {
        let state = Arc::clone(&self.0);
        Arc::new(move |sample| {
            if let Ok(mut state) = state.lock() {
                state.sample = Some(sample);
            }
        })
    }

    pub(super) fn finish(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.finished = Some(Instant::now());
        }
    }

    fn recent(&self) -> bool {
        self.0.lock().is_ok_and(|state| {
            state
                .finished
                .is_none_or(|at| at.elapsed() < Duration::from_secs(60))
        })
    }

    fn sample(&self) -> Result<Option<TransferProgress>> {
        let state = self.0.lock().map_err(|error| invalid(error.to_string()))?;
        Ok(state.sample)
    }
}
