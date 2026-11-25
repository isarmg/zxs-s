use std::{
    fs::File,
    future::poll_fn,
    io::{self, Read, Seek, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
};

use axum::body::{Body, Bytes, HttpBody};
use futures_core::Stream;
use tokio::sync::{
    OwnedSemaphorePermit,
    mpsc::{self, Receiver, Sender},
};

use super::{BUFFER, Result, invalid};

pub async fn feed(mut body: Body, sender: Sender<Vec<u8>>) -> Result<()> {
    let mut buffer = Vec::new();
    while let Some(frame) = poll_fn(|context| Pin::new(&mut body).poll_frame(context)).await {
        let frame = frame.map_err(|error| invalid(format!("upload body: {error}")))?;
        if let Ok(bytes) = frame.into_data()
            && !send_blocks(&mut buffer, &sender, &bytes).await
        {
            return Ok(());
        }
    }
    if !buffer.is_empty() {
        let _ = sender.send(buffer).await;
    }
    Ok(())
}

async fn send_blocks(buffer: &mut Vec<u8>, sender: &Sender<Vec<u8>>, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        if buffer.capacity() == 0 {
            buffer.reserve_exact(BUFFER);
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

struct Download(Receiver<Chunk>);

impl Stream for Download {
    type Item = Chunk;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(context)
    }
}

pub fn download(mut file: File, start: u64, remaining: u64, permit: OwnedSemaphorePermit) -> Body {
    let (sender, receiver) = mpsc::channel(1);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = file
            .seek(SeekFrom::Start(start))
            .and_then(|_| send_file(file, remaining, &sender));
        if let Err(error) = result {
            let _ = sender.blocking_send(Err(error));
        }
    });
    Body::from_stream(Download(receiver))
}

fn send_file(file: File, mut remaining: u64, sender: &Sender<Chunk>) -> io::Result<()> {
    let mut input = file.take(remaining);
    while remaining > 0 {
        let size = usize::try_from(remaining.min(BUFFER as u64)).map_err(io::Error::other)?;
        let mut chunk = vec![0; size];
        input.read_exact(&mut chunk)?;
        if sender.blocking_send(Ok(Bytes::from(chunk))).is_err() {
            break;
        }
        remaining -= size as u64;
    }
    Ok(())
}
