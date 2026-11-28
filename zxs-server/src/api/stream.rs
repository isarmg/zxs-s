use std::{
    fs::File,
    future::poll_fn,
    io::{self, Read, Seek, SeekFrom},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum::body::{Body, Bytes, HttpBody};
use futures_core::Stream;
use tokio::sync::{
    OwnedSemaphorePermit,
    mpsc::{self, Receiver, Sender},
};

use super::{BUFFER, Result, invalid};

pub async fn feed(mut body: Body, sender: Sender<Vec<u8>>, size: u64) -> Result<()> {
    let mut buffer = Vec::new();
    let capacity = usize::try_from(size).unwrap_or(BUFFER).clamp(1, BUFFER);
    while let Some(frame) = tokio::select! {
        biased;
        frame = poll_fn(|context| Pin::new(&mut body).poll_frame(context)) => frame,
        () = sender.closed() => return Ok(()),
    } {
        let frame = frame.map_err(|error| invalid(format!("upload body: {error}")))?;
        if let Ok(bytes) = frame.into_data()
            && !send_blocks(&mut buffer, &sender, &bytes, capacity).await
        {
            return Ok(());
        }
    }
    if !buffer.is_empty() {
        let _ = sender.send(buffer).await;
    }
    Ok(())
}

async fn send_blocks(
    buffer: &mut Vec<u8>,
    sender: &Sender<Vec<u8>>,
    mut bytes: &[u8],
    capacity: usize,
) -> bool {
    while !bytes.is_empty() {
        if buffer.capacity() == 0 {
            buffer.reserve_exact(capacity);
        }
        let count = bytes.len().min(BUFFER - buffer.len());
        let (chunk, remaining) = bytes.split_at(count);
        buffer.extend_from_slice(chunk);
        bytes = remaining;
        if buffer.len() == BUFFER && sender.send(std::mem::take(buffer)).await.is_err() {
            return false;
        }
    }
    true
}

type Chunk = io::Result<Bytes>;

struct Download {
    receiver: Receiver<Chunk>,
    _permit: Arc<OwnedSemaphorePermit>,
}

impl Stream for Download {
    type Item = Chunk;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(context)
    }
}

pub fn download(mut file: File, start: u64, remaining: u64, permit: OwnedSemaphorePermit) -> Body {
    let (sender, receiver) = mpsc::channel(1);
    let permit = Arc::new(permit);
    let download = Download {
        receiver,
        _permit: Arc::clone(&permit),
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = file
            .seek(SeekFrom::Start(start))
            .and_then(|_| send_file(file, remaining, &sender));
        if let Err(error) = result {
            let _ = sender.blocking_send(Err(error));
        }
    });
    Body::from_stream(download)
}

fn send_file(mut file: File, mut remaining: u64, sender: &Sender<Chunk>) -> io::Result<()> {
    while remaining > 0 {
        let size = usize::try_from(remaining).unwrap_or(BUFFER).min(BUFFER);
        let mut chunk = vec![0; size];
        file.read_exact(&mut chunk)?;
        if sender.blocking_send(Ok(Bytes::from(chunk))).is_err() {
            break;
        }
        remaining -= size as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{future::Future, time::Duration};

    use crate::domain::Error;

    use super::{BUFFER, Body, Chunk, Context, Pin, Poll, Result, Stream, feed, mpsc, poll_fn};

    struct PausedUpload;

    impl Stream for PausedUpload {
        type Item = Chunk;

        fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn upload_stops_waiting_for_body_after_storage_receiver_closes() {
        let (sender, receiver) = mpsc::channel(1);
        let pump = feed(Body::from_stream(PausedUpload), sender, 1);
        tokio::pin!(pump);
        poll_fn(|context| {
            assert!(pump.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(receiver);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), pump).await,
            Ok(Ok(()))
        ));
    }

    #[tokio::test]
    async fn small_upload_uses_a_small_buffer() -> Result<()> {
        const CONTENTS: [u8; 4096] = [7; 4096];
        let (sender, mut receiver) = mpsc::channel(1);
        feed(
            Body::from(CONTENTS.as_slice()),
            sender,
            CONTENTS.len() as u64,
        )
        .await?;
        let chunk = receiver.recv().await.ok_or(Error::Recovery)?;
        assert_eq!(chunk, CONTENTS);
        assert!(chunk.capacity() < BUFFER);
        Ok(())
    }
}
