use std::{
    io::{self, Read},
    sync::Arc,
    time::{Duration, Instant},
};

use serde::Serialize;

use super::Remote;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Upload,
    Download,
}

#[derive(Clone, Copy, Serialize)]
pub struct TransferProgress {
    pub direction: TransferDirection,
    pub transferred: u64,
    pub total: u64,
    pub bytes_per_second: u64,
}

pub type TransferObserver = Arc<dyn Fn(TransferProgress) + Send + Sync>;

impl Remote {
    /// Reports byte progress for transfers started through this instance.
    #[must_use]
    pub fn with_progress(mut self, observer: Option<TransferObserver>) -> Self {
        self.progress = observer;
        self
    }

    pub(super) fn track<R: Read>(
        &self,
        input: R,
        direction: TransferDirection,
        total: u64,
    ) -> impl Read + use<R> {
        let meter = self.progress.as_ref().map(|observer| {
            let sample = TransferProgress {
                direction,
                transferred: 0,
                total,
                bytes_per_second: 0,
            };
            if total == 0 {
                observer(sample);
            }
            Meter {
                observer: Arc::clone(observer),
                sample,
                last: None,
                last_bytes: 0,
            }
        });
        TransferReader { input, meter }
    }
}

struct TransferReader<R> {
    input: R,
    meter: Option<Meter>,
}

impl<R: Read> Read for TransferReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let Some(meter) = &mut self.meter else {
            return self.input.read(output);
        };
        if meter.sample.total > 0 && meter.last.is_none() && !output.is_empty() {
            meter.last = Some(Instant::now());
            (meter.observer)(meter.sample);
        }
        let count = self.input.read(output)?;
        if count > 0 {
            meter.advance(count as u64);
        }
        Ok(count)
    }
}

struct Meter {
    observer: TransferObserver,
    sample: TransferProgress,
    last: Option<Instant>,
    last_bytes: u64,
}

impl Meter {
    fn advance(&mut self, count: u64) {
        self.sample.transferred += count;
        let now = Instant::now();
        let elapsed = now.duration_since(self.last.unwrap_or(now));
        if elapsed < Duration::from_millis(200) && self.sample.transferred < self.sample.total {
            return;
        }
        let bytes = u128::from(self.sample.transferred - self.last_bytes);
        self.sample.bytes_per_second =
            u64::try_from(bytes * 1_000_000_000 / elapsed.as_nanos().max(1)).unwrap_or(u64::MAX);
        self.last = Some(now);
        self.last_bytes = self.sample.transferred;
        (self.observer)(self.sample);
    }
}
