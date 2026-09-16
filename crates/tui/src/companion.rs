/*
 * Licensed to the Apache Software Foundation (ASF) under one
 * or more contributor license agreements.  See the NOTICE file
 * distributed with this work for additional information
 * regarding copyright ownership.  The ASF licenses this file
 * to you under the Apache License, Version 2.0 (the
 * "License"); you may not use this file except in compliance
 * with the License.  You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing,
 * software distributed under the License is distributed on an
 * "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
 * KIND, either express or implied.  See the License for the
 * specific language governing permissions and limitations
 * under the License.
 */

//! One owned Companion process behind a nonblocking, bounded UI interface.
//!
//! This module never accesses the terminal, retries a request, or starts a Host.
//! Explicit close belongs after terminal restoration; Drop is a bounded unwind
//! fallback. A hung operating-system process cannot be proven reaped after the
//! deadline, so close reports that failure instead of waiting indefinitely.

use std::{
    ffi::{OsStr, OsString},
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc as sync_mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    runtime::Builder,
    sync::{mpsc, oneshot},
    task,
    time::{Instant, timeout, timeout_at},
};

use super::wire::{self, BUILD_ID, MAX_ECHO_BYTES, Message, WireError};

const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const HELLO_TIMEOUT: Duration = Duration::from_secs(3);
const ECHO_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);
const REAP_TIMEOUT: Duration = Duration::from_secs(2);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_GENERATION: u64 = 0;
const HELLO_GENERATION: u64 = 1;
const SHUTDOWN_GENERATION: u64 = u64::MAX;

/// A UI-visible state change. Peer text is never included in a notice.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Notice {
    Ready,
    Echoed { request_id: u32, bytes: usize },
    Disconnected { reason: String, stderr_bytes: u64 },
}

/// An echo was not queued; the caller retains its draft in every case.
#[derive(Debug, Error, PartialEq, Eq)]
pub(super) enum SendError {
    #[error("Companion handshake is not complete")]
    NotReady,
    #[error("a Companion echo is already pending")]
    Busy,
    #[error("echo exceeds the 4096-byte text or 16-KiB encoded frame limit")]
    TooLarge,
    #[error("Companion is disconnected")]
    Disconnected,
    #[error("Companion request identifiers are exhausted; reconnect explicitly")]
    RequestIdsExhausted,
}

#[derive(Debug, PartialEq, Eq)]
enum UiState {
    Starting,
    Ready,
    Pending(u32),
    Closed,
}

struct EchoRequest {
    id: u32,
    text: String,
    encoded: Vec<u8>,
}

impl EchoRequest {
    fn generation(&self) -> u64 {
        u64::from(self.id) + 1
    }
}

/// Owns exactly one worker and child; only close/Drop may wait.
pub(super) struct Companion {
    commands: mpsc::Sender<EchoRequest>,
    notices: sync_mpsc::Receiver<Notice>,
    shutdown: Option<oneshot::Sender<()>>,
    completion: sync_mpsc::Receiver<io::Result<()>>,
    worker: Option<JoinHandle<()>>,
    state: UiState,
    last_id: u32,
}

impl Companion {
    /// Start a worker without waiting for process launch or the handshake.
    pub(super) fn spawn(node: &OsStr, script: &Path) -> io::Result<Self> {
        let (commands_tx, commands_rx) = mpsc::channel(1);
        let (notices_tx, notices_rx) = sync_mpsc::sync_channel(4);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (completion_tx, completion_rx) = sync_mpsc::sync_channel(1);
        let node = node.to_os_string();
        let script = script.to_path_buf();
        let worker = thread::Builder::new()
            .name("maka-tui-companion".to_owned())
            .spawn(move || {
                let stderr_bytes = Arc::new(AtomicU64::new(0));
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    worker_main(
                        node,
                        script,
                        commands_rx,
                        shutdown_rx,
                        &notices_tx,
                        &stderr_bytes,
                    )
                }))
                .unwrap_or_else(|_| Err(io::Error::other("Companion worker panicked")));
                let reason = match &outcome {
                    Ok(()) => "Companion closed".to_owned(),
                    Err(error) => error.to_string(),
                };
                // At most one ready/echo notice can await UI consumption. The
                // fourth slot is reserved headroom, never a reason to block I/O.
                let _ = notices_tx.try_send(Notice::Disconnected {
                    reason,
                    stderr_bytes: stderr_bytes.load(Ordering::Relaxed),
                });
                let _ = completion_tx.try_send(outcome);
            })?;
        Ok(Self {
            commands: commands_tx,
            notices: notices_rx,
            shutdown: Some(shutdown_tx),
            completion: completion_rx,
            worker: Some(worker),
            state: UiState::Starting,
            last_id: 0,
        })
    }

    /// Queue at most one echo with bounded serialization; never wait on I/O.
    pub(super) fn echo(&mut self, text: &str) -> Result<u32, SendError> {
        match self.state {
            UiState::Starting => return Err(SendError::NotReady),
            UiState::Pending(_) => return Err(SendError::Busy),
            UiState::Closed => return Err(SendError::Disconnected),
            UiState::Ready => {}
        }
        if text.len() > MAX_ECHO_BYTES {
            return Err(SendError::TooLarge);
        }
        let id = self
            .last_id
            .checked_add(1)
            .ok_or(SendError::RequestIdsExhausted)?;
        let message = Message::Echo {
            request_id: id,
            text: text.to_owned(),
        };
        let encoded = wire::encode(&message).map_err(|_| SendError::TooLarge)?;
        let Message::Echo { text, .. } = message else {
            return Err(SendError::Disconnected);
        };
        self.commands
            .try_send(EchoRequest { id, text, encoded })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => SendError::Busy,
                mpsc::error::TrySendError::Closed(_) => SendError::Disconnected,
            })?;
        self.last_id = id;
        self.state = UiState::Pending(id);
        Ok(id)
    }

    /// Consume one notice without waiting. Disconnection is reported once.
    pub(super) fn poll(&mut self) -> Option<Notice> {
        if self.state == UiState::Closed {
            return None;
        }
        let mut notice = match self.notices.try_recv() {
            Ok(notice) => notice,
            Err(sync_mpsc::TryRecvError::Empty) => return None,
            Err(sync_mpsc::TryRecvError::Disconnected) => Notice::Disconnected {
                reason: "Companion worker ended without a completion notice".to_owned(),
                stderr_bytes: 0,
            },
        };
        let ordered = match (&self.state, &notice) {
            (UiState::Starting, Notice::Ready) | (_, Notice::Disconnected { .. }) => true,
            (UiState::Pending(expected), Notice::Echoed { request_id, .. }) => {
                expected == request_id
            }
            _ => false,
        };
        if !ordered {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            notice = Notice::Disconnected {
                reason: "Companion UI notice correlation mismatch".to_owned(),
                stderr_bytes: 0,
            };
        }
        match &notice {
            Notice::Ready => self.state = UiState::Ready,
            Notice::Echoed { .. } => self.state = UiState::Ready,
            Notice::Disconnected { .. } => self.state = UiState::Closed,
        }
        Some(notice)
    }

    /// Request shutdown, then wait at most five seconds after terminal restore.
    pub(super) fn close(mut self) -> io::Result<()> {
        self.finish()
    }

    fn finish(&mut self) -> io::Result<()> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        self.state = UiState::Closed;
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let outcome = match self.completion.recv_timeout(CLOSE_TIMEOUT) {
            Ok(outcome) => outcome,
            Err(sync_mpsc::RecvTimeoutError::Timeout) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Companion close timed out; child reclamation is unconfirmed",
                ));
            }
            Err(sync_mpsc::RecvTimeoutError::Disconnected) => {
                Err(io::Error::other("Companion worker completion was lost"))
            }
        };
        // Completion is sent at the end of the worker. Never make a successful
        // notification into an unbounded join on a descheduled thread.
        if worker.is_finished() && worker.join().is_err() {
            return Err(io::Error::other("Companion worker panicked during exit"));
        }
        outcome
    }
}

impl Drop for Companion {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

struct Received {
    generation: u64,
    message: Result<Message, WireError>,
}

enum SessionEnd {
    Shutdown { pending: Option<EchoRequest> },
    // A partly written frame cannot be followed by another frame safely.
    Interrupt,
}

enum Awaited {
    Frame(Received),
    Shutdown,
}

fn worker_main(
    node: OsString,
    script: PathBuf,
    commands: mpsc::Receiver<EchoRequest>,
    shutdown: oneshot::Receiver<()>,
    notices: &sync_mpsc::SyncSender<Notice>,
    stderr_bytes: &Arc<AtomicU64>,
) -> io::Result<()> {
    let runtime = Builder::new_current_thread().enable_all().build()?;
    let mut child = {
        let _entered = runtime.enter();
        Command::new(node)
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?
    };
    // The child stays outside the unwind scope. Even a panic while initializing
    // readers or driving the session is followed by an explicit kill and wait.
    let result = catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(drive_child(
            &mut child,
            commands,
            shutdown,
            notices,
            stderr_bytes,
        ))
    }))
    .unwrap_or_else(|_| Err(io::Error::other("Companion session panicked")));
    let reclaimed = runtime.block_on(reclaim(&mut child));
    drop(child);
    runtime.shutdown_timeout(Duration::from_millis(100));
    combine(result, reclaimed)
}

async fn drive_child(
    child: &mut Child,
    commands: mpsc::Receiver<EchoRequest>,
    shutdown: oneshot::Receiver<()>,
    notices: &sync_mpsc::SyncSender<Notice>,
    stderr_bytes: &Arc<AtomicU64>,
) -> io::Result<()> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("Companion stdin pipe is missing"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Companion stdout pipe is missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Companion stderr pipe is missing"))?;
    let generation = Arc::new(AtomicU64::new(IDLE_GENERATION));
    let (frames_tx, mut frames) = mpsc::channel(1);
    let stdout_task = task::spawn(read_stdout(stdout, frames_tx, Arc::clone(&generation)));
    let mut stderr_task = task::spawn(drain_stderr(stderr, Arc::clone(stderr_bytes)));
    let result = session(
        child,
        &mut stdin,
        &mut frames,
        commands,
        shutdown,
        notices,
        &generation,
    )
    .await;
    let result = match result {
        Ok(SessionEnd::Shutdown { pending }) => {
            graceful_shutdown(child, &mut stdin, &mut frames, &generation, pending).await
        }
        Ok(SessionEnd::Interrupt) => Ok(()),
        Err(error) => Err(error),
    };
    drop(stdin);
    let reclaimed = reclaim(child).await;
    stdout_task.abort();
    let _ = stdout_task.await;
    // Drain available stderr through process exit, but descendants cannot keep
    // this owned child's close alive merely by inheriting a pipe handle.
    let stderr_result = match timeout(Duration::from_millis(100), &mut stderr_task).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(io::Error::other("Companion stderr reader panicked")),
        Err(_) => {
            stderr_task.abort();
            let _ = stderr_task.await;
            Ok(())
        }
    };
    combine(combine(result, reclaimed), stderr_result)
}

async fn read_stdout(
    mut stdout: ChildStdout,
    frames: mpsc::Sender<Received>,
    generation: Arc<AtomicU64>,
) {
    loop {
        // This read future is never recreated by a select branch. Cancelling
        // the task means the whole connection is discarded, not retried.
        let message = wire::read(&mut stdout).await;
        let failed = message.is_err();
        let received = Received {
            generation: generation.load(Ordering::Relaxed),
            message,
        };
        if frames.send(received).await.is_err() || failed {
            return;
        }
    }
}

async fn drain_stderr(mut stderr: ChildStderr, count: Arc<AtomicU64>) -> io::Result<()> {
    let mut buffer = [0_u8; 4096];
    loop {
        let bytes = stderr.read(&mut buffer).await?;
        if bytes == 0 {
            return Ok(());
        }
        let bytes = bytes as u64;
        let _ = count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_add(bytes))
        });
    }
}

async fn session(
    child: &mut Child,
    stdin: &mut ChildStdin,
    frames: &mut mpsc::Receiver<Received>,
    mut commands: mpsc::Receiver<EchoRequest>,
    mut shutdown: oneshot::Receiver<()>,
    notices: &sync_mpsc::SyncSender<Notice>,
    generation: &AtomicU64,
) -> io::Result<SessionEnd> {
    let hello = wire::encode(&Message::Hello {
        build_id: BUILD_ID.to_owned(),
    })
    .map_err(protocol_error)?;
    if !write_or_shutdown(stdin, &hello, &mut shutdown).await? {
        return Ok(SessionEnd::Interrupt);
    }
    generation.store(HELLO_GENERATION, Ordering::Relaxed);
    match await_reply(child, frames, &mut shutdown, HELLO_TIMEOUT).await? {
        Awaited::Shutdown => return Ok(SessionEnd::Interrupt),
        Awaited::Frame(frame) => validate_ready(frame)?,
    }
    generation.store(IDLE_GENERATION, Ordering::Relaxed);
    publish(notices, Notice::Ready)?;
    let mut last_id = 0;
    loop {
        let request = tokio::select! {
            biased;
            _ = &mut shutdown => return Ok(SessionEnd::Shutdown { pending: None }),
            frame = frames.recv() => return Err(unexpected_frame(frame)),
            status = child.wait() => return Err(unexpected_exit(status)),
            request = commands.recv() => match request {
                Some(request) => request,
                None => return Ok(SessionEnd::Shutdown { pending: None }),
            },
        };
        if request.id <= last_id {
            return Err(io::Error::other("Companion request identifier was reused"));
        }
        last_id = request.id;
        if !write_or_shutdown(stdin, &request.encoded, &mut shutdown).await? {
            return Ok(SessionEnd::Interrupt);
        }
        generation.store(request.generation(), Ordering::Relaxed);
        match await_reply(child, frames, &mut shutdown, ECHO_TIMEOUT).await? {
            Awaited::Shutdown => {
                return Ok(SessionEnd::Shutdown {
                    pending: Some(request),
                });
            }
            Awaited::Frame(frame) => validate_echo(frame, &request)?,
        }
        generation.store(IDLE_GENERATION, Ordering::Relaxed);
        publish(
            notices,
            Notice::Echoed {
                request_id: request.id,
                bytes: request.text.len(),
            },
        )?;
    }
}

async fn write_or_shutdown(
    stdin: &mut ChildStdin,
    bytes: &[u8],
    shutdown: &mut oneshot::Receiver<()>,
) -> io::Result<bool> {
    tokio::select! {
        biased;
        _ = shutdown => Ok(false),
        written = timeout(WRITE_TIMEOUT, stdin.write_all(bytes)) => {
            written.map_err(|_| timed_out("Companion write timed out"))??;
            Ok(true)
        },
    }
}

async fn await_reply(
    child: &mut Child,
    frames: &mut mpsc::Receiver<Received>,
    shutdown: &mut oneshot::Receiver<()>,
    duration: Duration,
) -> io::Result<Awaited> {
    tokio::select! {
        biased;
        _ = shutdown => Ok(Awaited::Shutdown),
        frame = timeout(duration, frames.recv()) => {
            let frame = frame.map_err(|_| timed_out("Companion reply timed out"))?
                .ok_or_else(|| io::Error::other("Companion stdout reader ended"))?;
            Ok(Awaited::Frame(frame))
        },
        status = child.wait() => Err(unexpected_exit(status)),
    }
}

fn validate_ready(frame: Received) -> io::Result<()> {
    let message = frame.message.map_err(protocol_error)?;
    match message {
        Message::Ready { build_id }
            if frame.generation == HELLO_GENERATION && build_id == BUILD_ID =>
        {
            Ok(())
        }
        _ => Err(io::Error::other(
            "Companion handshake or build identity mismatch",
        )),
    }
}

fn validate_echo(frame: Received, request: &EchoRequest) -> io::Result<()> {
    let message = frame.message.map_err(protocol_error)?;
    match message {
        Message::Echoed { request_id, text }
            if frame.generation == request.generation()
                && request_id == request.id
                && text == request.text =>
        {
            Ok(())
        }
        _ => Err(io::Error::other(
            "Companion echo correlation or content mismatch",
        )),
    }
}

async fn graceful_shutdown(
    child: &mut Child,
    stdin: &mut ChildStdin,
    frames: &mut mpsc::Receiver<Received>,
    generation: &AtomicU64,
    mut pending: Option<EchoRequest>,
) -> io::Result<()> {
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    let shutdown = wire::encode(&Message::Shutdown {}).map_err(protocol_error)?;
    timeout_at(deadline, stdin.write_all(&shutdown))
        .await
        .map_err(|_| timed_out("Companion shutdown write timed out"))??;
    generation.store(SHUTDOWN_GENERATION, Ordering::Relaxed);
    loop {
        let received = timeout_at(deadline, frames.recv())
            .await
            .map_err(|_| timed_out("Companion shutdown acknowledgement timed out"))?
            .ok_or_else(|| io::Error::other("Companion ended without shutdown acknowledgement"))?;
        match received.message.map_err(protocol_error)? {
            Message::Bye {} if received.generation == SHUTDOWN_GENERATION && pending.is_none() => {
                break;
            }
            Message::Echoed { request_id, text } => {
                let expected = pending
                    .take()
                    .ok_or_else(|| io::Error::other("unexpected Companion echo during shutdown"))?;
                if request_id != expected.id
                    || text != expected.text
                    || (received.generation != expected.generation()
                        && received.generation != SHUTDOWN_GENERATION)
                {
                    return Err(io::Error::other("Companion shutdown echo mismatch"));
                }
            }
            _ => {
                return Err(io::Error::other(
                    "invalid Companion shutdown acknowledgement",
                ));
            }
        }
    }
    let status = timeout_at(deadline, child.wait())
        .await
        .map_err(|_| timed_out("Companion did not exit after shutdown acknowledgement"))??;
    if !status.success() {
        return Err(io::Error::other(
            "Companion exited unsuccessfully during shutdown",
        ));
    }
    Ok(())
}

async fn reclaim(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let kill = child.start_kill();
    let wait = timeout(REAP_TIMEOUT, child.wait())
        .await
        .map_err(|_| timed_out("Companion child reap timed out"))
        .and_then(|result| result.map(|_| ()));
    combine(kill, wait)
}

fn publish(notices: &sync_mpsc::SyncSender<Notice>, notice: Notice) -> io::Result<()> {
    notices
        .try_send(notice)
        .map_err(|_| io::Error::other("Companion UI notice channel is unavailable"))
}

fn protocol_error(error: WireError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn timed_out(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, message)
}

fn unexpected_frame(frame: Option<Received>) -> io::Error {
    match frame {
        Some(Received {
            message: Err(error),
            ..
        }) => protocol_error(error),
        Some(_) => io::Error::other("unsolicited or duplicate Companion message"),
        None => io::Error::other("Companion stdout reader ended"),
    }
}

fn unexpected_exit(status: io::Result<std::process::ExitStatus>) -> io::Error {
    match status {
        Ok(_) => io::Error::other("Companion exited unexpectedly"),
        Err(error) => error,
    }
}

fn combine(first: io::Result<()>, second: io::Result<()>) -> io::Result<()> {
    match (first, second) {
        (Ok(()), result) | (result, Ok(())) => result,
        (Err(first), Err(second)) => Err(io::Error::new(
            first.kind(),
            format!("{first}; cleanup also failed: {second}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: u32, text: &str) -> EchoRequest {
        EchoRequest {
            id,
            text: text.to_owned(),
            encoded: Vec::new(),
        }
    }

    #[test]
    fn ready_requires_the_matching_build_identifier() {
        let received = Received {
            generation: HELLO_GENERATION,
            message: Ok(Message::Ready {
                build_id: "other-build".to_owned(),
            }),
        };
        assert!(validate_ready(received).is_err());
    }

    #[test]
    fn ready_cannot_arrive_before_hello_was_written() {
        let received = Received {
            generation: IDLE_GENERATION,
            message: Ok(Message::Ready {
                build_id: BUILD_ID.to_owned(),
            }),
        };
        assert!(validate_ready(received).is_err());
    }

    #[test]
    fn echo_requires_byte_exact_text_not_unicode_equivalence() {
        let request = request(1, "e\u{301}");
        let received = Received {
            generation: request.generation(),
            message: Ok(Message::Echoed {
                request_id: 1,
                text: "é".to_owned(),
            }),
        };
        assert!(validate_echo(received, &request).is_err());
    }

    #[test]
    fn echo_requires_the_current_request_identifier() {
        let request = request(2, "draft");
        let received = Received {
            generation: request.generation(),
            message: Ok(Message::Echoed {
                request_id: 1,
                text: "draft".to_owned(),
            }),
        };
        assert!(validate_echo(received, &request).is_err());
    }

    #[test]
    fn queued_reply_from_an_old_generation_is_not_accepted() {
        let old_generation = request(1, "draft").generation();
        let request = request(2, "draft");
        let received = Received {
            generation: old_generation,
            message: Ok(Message::Echoed {
                request_id: 2,
                text: "draft".to_owned(),
            }),
        };
        assert!(validate_echo(received, &request).is_err());
    }

    #[test]
    fn matching_reply_is_accepted_for_the_exact_generation() {
        let request = request(7, "中文 👩‍💻");
        let received = Received {
            generation: request.generation(),
            message: Ok(Message::Echoed {
                request_id: 7,
                text: request.text.clone(),
            }),
        };
        assert!(validate_echo(received, &request).is_ok());
    }

    #[test]
    fn combined_errors_preserve_operation_and_cleanup_failures() {
        let result = combine(
            Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame")),
            Err(io::Error::other("reap failed")),
        );
        assert_eq!(
            result.unwrap_err().to_string(),
            "bad frame; cleanup also failed: reap failed"
        );
    }

    #[test]
    fn ui_rejects_requests_until_ready_without_using_a_worker() {
        let (commands, _commands_rx) = mpsc::channel(1);
        let (_notices_tx, notices) = sync_mpsc::sync_channel(4);
        let (_completion_tx, completion) = sync_mpsc::sync_channel(1);
        let mut companion = Companion {
            commands,
            notices,
            shutdown: None,
            completion,
            worker: None,
            state: UiState::Starting,
            last_id: 0,
        };
        assert_eq!(companion.echo("draft"), Err(SendError::NotReady));
    }

    #[test]
    fn ui_keeps_one_outstanding_request_even_before_worker_consumes_it() {
        let (commands, _commands_rx) = mpsc::channel(1);
        let (_notices_tx, notices) = sync_mpsc::sync_channel(4);
        let (_completion_tx, completion) = sync_mpsc::sync_channel(1);
        let mut companion = Companion {
            commands,
            notices,
            shutdown: None,
            completion,
            worker: None,
            state: UiState::Ready,
            last_id: 0,
        };
        companion.echo("first").unwrap();
        assert_eq!(companion.echo("second"), Err(SendError::Busy));
    }

    #[test]
    fn request_identifier_exhaustion_never_wraps_or_reuses_zero() {
        let (commands, _commands_rx) = mpsc::channel(1);
        let (_notices_tx, notices) = sync_mpsc::sync_channel(4);
        let (_completion_tx, completion) = sync_mpsc::sync_channel(1);
        let mut companion = Companion {
            commands,
            notices,
            shutdown: None,
            completion,
            worker: None,
            state: UiState::Ready,
            last_id: u32::MAX,
        };
        assert_eq!(companion.echo("draft"), Err(SendError::RequestIdsExhausted));
    }

    #[test]
    fn oversized_echo_does_not_use_a_request_id_or_queue_slot() {
        let (commands, mut commands_rx) = mpsc::channel(1);
        let (_notices_tx, notices) = sync_mpsc::sync_channel(4);
        let (_completion_tx, completion) = sync_mpsc::sync_channel(1);
        let mut companion = Companion {
            commands,
            notices,
            shutdown: None,
            completion,
            worker: None,
            state: UiState::Ready,
            last_id: 0,
        };
        assert_eq!(
            companion.echo(&"x".repeat(MAX_ECHO_BYTES + 1)),
            Err(SendError::TooLarge)
        );
        assert_eq!(companion.echo("small").unwrap(), 1);
        assert_eq!(commands_rx.try_recv().unwrap().text, "small");
    }

    #[test]
    fn stale_ui_notice_disconnects_instead_of_making_the_draft_sendable() {
        let (commands, _commands_rx) = mpsc::channel(1);
        let (notices_tx, notices) = sync_mpsc::sync_channel(4);
        let (_completion_tx, completion) = sync_mpsc::sync_channel(1);
        let mut companion = Companion {
            commands,
            notices,
            shutdown: None,
            completion,
            worker: None,
            state: UiState::Pending(2),
            last_id: 2,
        };
        notices_tx
            .try_send(Notice::Echoed {
                request_id: 1,
                bytes: 4,
            })
            .unwrap();
        assert!(matches!(
            companion.poll(),
            Some(Notice::Disconnected { .. })
        ));
        assert_eq!(companion.echo("draft"), Err(SendError::Disconnected));
    }
}
