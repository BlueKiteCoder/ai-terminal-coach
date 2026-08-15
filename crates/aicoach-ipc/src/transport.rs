use std::io;

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol::DEFAULT_MAX_FRAME_LENGTH;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid JSON frame: {0}")]
    Json(#[from] serde_json::Error),
    #[error("frame exceeds the {limit}-byte limit")]
    FrameTooLarge { limit: usize },
    #[error("connection closed")]
    ConnectionClosed,
    #[error("protocol error: {0}")]
    Protocol(String),
}

pub struct NdjsonReader<R> {
    inner: R,
    max_frame_length: usize,
    buffer: Vec<u8>,
}

impl<R> NdjsonReader<R>
where
    R: AsyncBufRead + Unpin,
{
    pub fn new(inner: R) -> Self {
        Self::with_max_frame_length(inner, DEFAULT_MAX_FRAME_LENGTH)
    }

    pub fn with_max_frame_length(inner: R, max_frame_length: usize) -> Self {
        Self {
            inner,
            max_frame_length,
            buffer: Vec::new(),
        }
    }

    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<Option<T>, TransportError> {
        loop {
            self.buffer.clear();
            let mut saw_bytes = false;
            loop {
                let available = self.inner.fill_buf().await?;
                if available.is_empty() {
                    break;
                }
                saw_bytes = true;
                if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                    if self.buffer.len() + newline > self.max_frame_length {
                        return Err(TransportError::FrameTooLarge {
                            limit: self.max_frame_length,
                        });
                    }
                    self.buffer.extend_from_slice(&available[..newline]);
                    self.inner.consume(newline + 1);
                    break;
                }
                if self.buffer.len() + available.len() > self.max_frame_length {
                    return Err(TransportError::FrameTooLarge {
                        limit: self.max_frame_length,
                    });
                }
                let length = available.len();
                self.buffer.extend_from_slice(available);
                self.inner.consume(length);
            }

            if !saw_bytes && self.buffer.is_empty() {
                return Ok(None);
            }
            if self.buffer.ends_with(b"\r") {
                self.buffer.pop();
            }
            if self.buffer.is_empty() {
                continue;
            }
            return serde_json::from_slice(&self.buffer)
                .map(Some)
                .map_err(Into::into);
        }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

pub struct NdjsonWriter<W> {
    inner: W,
    max_frame_length: usize,
}

impl<W> NdjsonWriter<W>
where
    W: AsyncWrite + Unpin,
{
    pub fn new(inner: W) -> Self {
        Self::with_max_frame_length(inner, DEFAULT_MAX_FRAME_LENGTH)
    }

    pub fn with_max_frame_length(inner: W, max_frame_length: usize) -> Self {
        Self {
            inner,
            max_frame_length,
        }
    }

    pub async fn send<T: Serialize>(&mut self, value: &T) -> Result<(), TransportError> {
        let encoded = serde_json::to_vec(value)?;
        if encoded.len() > self.max_frame_length {
            return Err(TransportError::FrameTooLarge {
                limit: self.max_frame_length,
            });
        }
        self.inner.write_all(&encoded).await?;
        self.inner.write_all(b"\n").await?;
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<(), TransportError> {
        self.inner.shutdown().await.map_err(Into::into)
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use tokio::io::{BufReader, duplex};

    use super::*;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Payload {
        text: String,
    }

    #[tokio::test]
    async fn reads_and_writes_newline_containing_json_string() {
        let (left, right) = duplex(1024);
        let mut writer = NdjsonWriter::new(left);
        let mut reader = NdjsonReader::new(BufReader::new(right));
        let value = Payload {
            text: "one\ntwo".to_owned(),
        };

        writer.send(&value).await.unwrap();
        assert_eq!(reader.recv::<Payload>().await.unwrap(), Some(value));
    }

    #[tokio::test]
    async fn rejects_oversized_frames() {
        let (mut left, right) = duplex(1024);
        tokio::spawn(async move {
            left.write_all(b"123456789\n").await.unwrap();
        });
        let mut reader = NdjsonReader::with_max_frame_length(BufReader::new(right), 4);
        assert!(matches!(
            reader.recv::<Payload>().await,
            Err(TransportError::FrameTooLarge { limit: 4 })
        ));
    }
}
