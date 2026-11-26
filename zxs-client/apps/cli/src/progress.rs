use std::{
    io::{self, IsTerminal, Write},
    sync::{Arc, Mutex},
};

use zxs_core::{TransferDirection, TransferObserver, TransferProgress};

#[derive(Default)]
pub struct Terminal(Arc<Mutex<bool>>);

impl Terminal {
    pub fn observer(&self) -> Option<TransferObserver> {
        if !io::stderr().is_terminal() {
            return None;
        }
        let active = Arc::clone(&self.0);
        Some(Arc::new(move |progress| {
            if let Ok(mut active) = active.lock() {
                display(progress, &mut active);
            }
        }))
    }

    pub fn finish(&self) {
        if let Ok(mut active) = self.0.lock()
            && *active
        {
            let _ = writeln!(io::stderr());
            *active = false;
        }
    }
}

fn display(sample: TransferProgress, active: &mut bool) {
    let mut stderr = io::stderr().lock();
    if *active && sample.transferred == 0 {
        let _ = writeln!(stderr);
    }
    let direction = match sample.direction {
        TransferDirection::Upload => "上传",
        TransferDirection::Download => "下载",
    };
    let complete = sample.transferred >= sample.total;
    let percent = if complete {
        100
    } else {
        u128::from(sample.transferred) * 100 / u128::from(sample.total)
    };
    let filled = usize::try_from(percent / 5).unwrap_or(20);
    let rate = rate(sample.bytes_per_second);
    let _ = write!(
        stderr,
        "\r{direction} [{}{}] {percent:3}%  {rate:>14}",
        "=".repeat(filled),
        " ".repeat(20 - filled)
    );
    if complete {
        let _ = writeln!(stderr);
    }
    let _ = stderr.flush();
    *active = !complete;
}

fn rate(bytes: u64) -> String {
    let mut scale = 1;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB"] {
        if bytes < scale * 1024 {
            break;
        }
        scale *= 1024;
        unit = next;
    }
    format!(
        "{}.{} {unit}/s",
        bytes / scale,
        (bytes % scale) * 10 / scale
    )
}
