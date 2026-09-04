use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use thiserror::Error;
use tokio::{
    io::{BufReader, ReadHalf, WriteHalf},
    net::UnixStream,
    sync::{Mutex, broadcast, oneshot},
    task::JoinHandle,
};

use crate::{
    protocol::{
        DEFAULT_MAX_FRAME_LENGTH, Event, Message, Request, RequestBody, RequestId, Response,
        SessionId,
    },
    transport::{NdjsonReader, NdjsonWriter, TransportError},
};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    #[error("the server sent a request to a client connection")]
    UnexpectedRequest,
}

type Pending = Arc<Mutex<HashMap<RequestId, oneshot::Sender<Response>>>>;

struct ClientInner {
    writer: Mutex<NdjsonWriter<WriteHalf<UnixStream>>>,
    pending: Pending,
    events: broadcast::Sender<Event>,
    closed: AtomicBool,
    reader_task: std::sync::Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct IpcClient {
    inner: Arc<ClientInner>,
}

impl IpcClient {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        Self::connect_with_max_frame_length(path, DEFAULT_MAX_FRAME_LENGTH).await
    }

    pub async fn connect_with_max_frame_length(
        path: impl AsRef<Path>,
        max_frame_length: usize,
    ) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(TransportError::Io)?;
        let (read, write) = tokio::io::split(stream);
        let (events, _) = broadcast::channel(256);
        let inner = Arc::new(ClientInner {
            writer: Mutex::new(NdjsonWriter::with_max_frame_length(write, max_frame_length)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            events,
            closed: AtomicBool::new(false),
            reader_task: std::sync::Mutex::new(None),
        });
        let task = tokio::spawn(read_loop(read, max_frame_length, Arc::clone(&inner)));
        *inner
            .reader_task
            .lock()
            .expect("reader task mutex poisoned") = Some(task);
        Ok(Self { inner })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    pub async fn send_request(
        &self,
        session_id: Option<SessionId>,
        body: RequestBody,
    ) -> Result<Response, ClientError> {
        self.send(Request::new(session_id, body)).await
    }

    pub async fn send(&self, request: Request) -> Result<Response, ClientError> {
        let receiver = self.dispatch(&request).await?;
        receiver.await.map_err(|_| ClientError::ConnectionClosed)
    }

    pub async fn send_timeout(
        &self,
        request: Request,
        timeout: Duration,
    ) -> Result<Response, ClientError> {
        let request_id = request.request_id;
        let receiver = self.dispatch(&request).await?;
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(ClientError::ConnectionClosed),
            Err(_) => {
                self.inner.pending.lock().await.remove(&request_id);
                Err(ClientError::Timeout(timeout))
            }
        }
    }

    async fn dispatch(
        &self,
        request: &Request,
    ) -> Result<oneshot::Receiver<Response>, ClientError> {
        if self.is_closed() {
            return Err(ClientError::ConnectionClosed);
        }
        let (sender, receiver) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .await
            .insert(request.request_id, sender);

        let send_result = self
            .inner
            .writer
            .lock()
            .await
            .send(&Message::from(request.clone()))
            .await;
        if let Err(error) = send_result {
            self.inner.pending.lock().await.remove(&request.request_id);
            return Err(error.into());
        }
        Ok(receiver)
    }

    pub async fn close(&self) -> Result<(), ClientError> {
        if !self.inner.closed.swap(true, Ordering::AcqRel) {
            self.inner.writer.lock().await.shutdown().await?;
            self.inner.pending.lock().await.clear();
        }
        if let Some(task) = self
            .inner
            .reader_task
            .lock()
            .expect("reader task mutex poisoned")
            .take()
        {
            task.abort();
        }
        Ok(())
    }
}

impl Drop for IpcClient {
    fn drop(&mut self) {
        // The reader task owns one `Arc`; abort it when only that task and the
        // last public handle remain so a forgotten `close` cannot leak a socket.
        if Arc::strong_count(&self.inner) == 2
            && let Some(task) = self
                .inner
                .reader_task
                .lock()
                .expect("reader task mutex poisoned")
                .take()
        {
            task.abort();
        }
    }
}

async fn read_loop(read: ReadHalf<UnixStream>, max_frame_length: usize, inner: Arc<ClientInner>) {
    let mut reader = NdjsonReader::with_max_frame_length(BufReader::new(read), max_frame_length);
    loop {
        match reader.recv::<Message>().await {
            Ok(Some(Message::Response { response })) => {
                if let Some(sender) = inner.pending.lock().await.remove(&response.request_id) {
                    let _ = sender.send(response);
                }
            }
            Ok(Some(Message::Event { event })) => {
                let _ = inner.events.send(event);
            }
            Ok(Some(Message::Request { .. }) | None) | Err(_) => break,
        }
    }
    inner.closed.store(true, Ordering::Release);
    inner.pending.lock().await.clear();
}
