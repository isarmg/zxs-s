use std::{
    cell::Cell,
    io::{self, Read, Seek, SeekFrom},
    rc::Rc,
};

use crate::{
    domain::{ContainerId, Version},
    transport::Remote,
};

const WINDOW_SIZE: u64 = 64 * 1024;

pub(super) struct PayloadRanges {
    remote: Remote,
    id: ContainerId,
    version: Version,
    offset: u64,
    window_start: u64,
    window: Vec<u8>,
    member_end: Rc<Cell<Option<u64>>>,
    member_stream: Option<Box<dyn Read>>,
}

impl PayloadRanges {
    pub(super) fn new(
        remote: &Remote,
        id: &ContainerId,
        version: Version,
        member_end: Rc<Cell<Option<u64>>>,
    ) -> Self {
        Self {
            remote: remote.clone(),
            id: id.clone(),
            version,
            offset: 0,
            window_start: 0,
            window: Vec::new(),
            member_end,
            member_stream: None,
        }
    }

    fn read_member(&mut self, output: &mut [u8], end: u64) -> io::Result<usize> {
        if self.offset >= end {
            return Ok(0);
        }
        let stream = if let Some(ref mut stream) = self.member_stream {
            stream
        } else {
            let response = self
                .remote
                .payload_range(&self.id, &self.version, self.offset..end)
                .map_err(io::Error::other)?;
            self.member_stream.insert(Box::new(response.into_reader()))
        };
        let count = stream.read(output)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated ZIP member",
            ));
        }
        self.offset += count as u64;
        Ok(count)
    }
}

impl Read for PayloadRanges {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.offset >= self.version.size {
            return Ok(0);
        }
        if let Some(end) = self.member_end.get() {
            return self.read_member(output, end);
        }
        let start = self.offset / WINDOW_SIZE * WINDOW_SIZE;
        if self.window_start != start || self.window.is_empty() {
            let end = start.saturating_add(WINDOW_SIZE).min(self.version.size);
            let response = self
                .remote
                .payload_range(&self.id, &self.version, start..end)
                .map_err(io::Error::other)?;
            let length = usize::try_from(end - start).map_err(io::Error::other)?;
            let mut window = std::mem::take(&mut self.window);
            window.resize(length, 0);
            response.into_reader().read_exact(&mut window)?;
            self.window = window;
            self.window_start = start;
        }
        let position = usize::try_from(self.offset - start).map_err(io::Error::other)?;
        let count = (&self.window[position..]).read(output)?;
        self.offset += count as u64;
        Ok(count)
    }
}

impl Seek for PayloadRanges {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.member_stream = None;
        self.offset = match position {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(offset) => self.offset.checked_add_signed(offset),
            SeekFrom::End(offset) => self.version.size.checked_add_signed(offset),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid ZIP offset"))?;
        Ok(self.offset)
    }
}
