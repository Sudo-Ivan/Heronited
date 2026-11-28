//! Progress reporting and cancellation, plus readers that count bytes.

use std::io::Read;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub sent: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continue {
    Go,
    Cancel,
}

impl Continue {
    pub fn cancelled(self) -> bool {
        matches!(self, Continue::Cancel)
    }
}

/// Reports every `step` bytes read through it (and once more at EOF).
pub struct CountingReader<'a, R: Read> {
    inner: R,
    sent: u64,
    total: u64,
    progress: &'a mut dyn FnMut(Progress) -> Continue,
    since_last: u64,
    step: u64,
    base: u64,
    pub(crate) failed: bool,
}

impl<'a, R: Read> CountingReader<'a, R> {
    pub fn new(inner: R, total: u64, progress: &'a mut dyn FnMut(Progress) -> Continue) -> Self {
        CountingReader { inner, sent: 0, total, progress, since_last: 0, step: 256 * 1024, base: 0, failed: false }
    }

    /// Reports `base + sent`: one part of a bigger transfer.
    pub fn with_base(inner: R, total: u64, base: u64, progress: &'a mut dyn FnMut(Progress) -> Continue) -> Self {
        let mut r = Self::new(inner, total, progress);
        r.base = base;
        r
    }

    pub fn sent(&self) -> u64 {
        self.sent
    }
}

impl<R: Read> Read for CountingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.sent += n as u64;
        self.since_last += n as u64;
        if self.since_last >= self.step || n == 0 {
            self.since_last = 0;
            if (self.progress)(Progress { sent: self.base + self.sent, total: self.total }).cancelled() {
                self.failed = true;
                return Err(std::io::Error::other("cancelled"));
            }
        }
        Ok(n)
    }
}

/// A reader chain: prefix bytes, then the file, then suffix bytes. The
/// hand-built multipart body streams this way with an exact total length.
pub struct ChainReader<R: Read> {
    prefix: std::io::Cursor<Vec<u8>>,
    file: R,
    suffix: std::io::Cursor<Vec<u8>>,
    stage: u8,
}

impl<R: Read> ChainReader<R> {
    pub fn new(prefix: Vec<u8>, file: R, suffix: Vec<u8>) -> Self {
        ChainReader { prefix: std::io::Cursor::new(prefix), file, suffix: std::io::Cursor::new(suffix), stage: 0 }
    }
}

impl<R: Read> Read for ChainReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let n = match self.stage {
                0 => self.prefix.read(buf)?,
                1 => self.file.read(buf)?,
                _ => self.suffix.read(buf)?,
            };
            if n > 0 {
                return Ok(n);
            }
            if self.stage >= 2 {
                return Ok(0);
            }
            self.stage += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn the_chain_reads_everything_in_order() {
        let file = std::io::Cursor::new(b"MIDDLE".to_vec());
        let mut chain = ChainReader::new(b"PRE-".to_vec(), file, b"-POST".to_vec());
        let mut all = String::new();
        chain.read_to_string(&mut all).unwrap();
        assert_eq!(all, "PRE-MIDDLE-POST");
    }

    #[test]
    fn counting_reports_and_cancels() {
        let data = vec![0u8; 1024 * 1024];
        let mut calls = 0;
        let mut progress = |_: Progress| {
            calls += 1;
            if calls >= 2 {
                Continue::Cancel
            } else {
                Continue::Go
            }
        };
        let mut r = CountingReader::new(std::io::Cursor::new(data), 1024 * 1024, &mut progress);
        let mut buf = vec![0u8; 512 * 1024];
        let _ = r.read(&mut buf);
        let _ = r.read(&mut buf);
        let err = r.read(&mut buf);
        assert!(err.is_err());
        assert!(r.failed);
    }
}
