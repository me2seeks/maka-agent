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

//! Owns one pinned TS client process. The terminal thread never waits for I/O.

use maka_tui::host_ui::{HostCommand, HostEvent};
use std::{ffi::OsStr, io, path::Path, process::Stdio, sync::mpsc, thread, time::Duration};
use tokio::{
    process::Command,
    sync::{mpsc as async_mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

pub(super) struct SessionHost {
    commands: async_mpsc::Sender<HostCommand>,
    events: async_mpsc::Receiver<HostEvent>,
    shutdown: Option<oneshot::Sender<()>>,
    completion: mpsc::Receiver<io::Result<()>>,
    closed: bool,
}

impl SessionHost {
    pub(super) fn spawn(
        node: &OsStr,
        script: &Path,
        root: &Path,
        session: &OsStr,
    ) -> io::Result<Self> {
        // App permits one unacknowledged send/answer and one stop.
        let (commands, mut requests) = async_mpsc::channel::<HostCommand>(2);
        let (notices, events) = async_mpsc::channel(8);
        let (shutdown, mut stop) = oneshot::channel();
        let (done, completion) = mpsc::sync_channel(1);
        let (node, script, root, session) = (
            node.to_owned(),
            script.to_owned(),
            root.to_owned(),
            session.to_owned(),
        );
        thread::Builder::new().name("maka-tui-host".into()).spawn(move || {
            let outcome = (|| -> io::Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                runtime.block_on(async {
                    let mut child = Command::new(node).arg(script).arg(root).arg(session)
                        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
                        .kill_on_drop(true).spawn()?;
                    let outcome = async {
                        let stdin = child.stdin.take().ok_or_else(failure)?;
                        let stdout = child.stdout.take().ok_or_else(failure)?;
                        let mut stderr = child.stderr.take().ok_or_else(failure)?;
                        // Drain without storing or rendering potentially sensitive diagnostics.
                        tokio::spawn(async move { let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await; });
                        let (mut reader, mut writer) = maka_transport::ndjson::split(tokio::io::join(stdout, stdin), CancellationToken::new());
                        let hello = tokio::select! {
                            _ = &mut stop => return Ok(()),
                            result = tokio::time::timeout(Duration::from_secs(10), reader.read()) => {
                                result.map_err(|_| failure())?.map_err(|_| failure())?.ok_or_else(failure)?
                            }
                        };
                        if !matches!(serde_json::from_value::<HostEvent>(hello), Ok(HostEvent::Hello { version: 1 })) {
                            return Err(failure());
                        }
                        let mut pending = None;
                        let mut answering: Option<String> = None;
                        let mut stopping = false;
                        let mut ready = false;
                        let mut history = false;
                        let mut deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                        let mut stop_deadline = deadline;
                        loop {
                            tokio::select! {
                                biased;
                                _ = &mut stop => {
                                    let _ = tokio::time::timeout(Duration::from_secs(1), writer.write(&HostCommand::Close)).await;
                                    break;
                                }
                                _ = tokio::time::sleep_until(deadline), if !ready || pending.is_some() || answering.is_some() => return Err(failure()),
                                _ = tokio::time::sleep_until(stop_deadline), if stopping => return Err(failure()),
                                request = requests.recv() => {
                                    let Some(request) = request else { break; };
                                    if let HostCommand::Answer { id, .. } = &request {
                                        if !ready || pending.is_some() || answering.is_some() || stopping { return Err(failure()); }
                                        answering = Some(id.clone());
                                        deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                                    }
                                    if let HostCommand::Send { revision, .. } = &request {
                                        if !ready || pending.is_some() || answering.is_some() || stopping { return Err(failure()); }
                                        pending = Some(*revision);
                                        deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                                    }
                                    if matches!(request, HostCommand::Stop) {
                                        if !ready || stopping { return Err(failure()); }
                                        stopping = true;
                                        stop_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                                    }
                                    tokio::select! {
                                        _ = &mut stop => break,
                                        result = tokio::time::timeout(Duration::from_secs(5), writer.write(&request)) => {
                                            result.map_err(|_| failure())?.map_err(|_| failure())?;
                                        }
                                    }
                                }
                                frame = reader.read() => {
                                    let frame = frame.map_err(|_| failure())?.ok_or_else(failure)?;
                                    let event: HostEvent = serde_json::from_value(frame).map_err(|_| failure())?;
                                    match &event {
                                        HostEvent::Hello { .. } => return Err(failure()),
                                        HostEvent::Ready { .. } if ready => return Err(failure()),
                                        HostEvent::Ready { .. } if history => ready = true,
                                        HostEvent::Ready { .. } => return Err(failure()),
                                        HostEvent::History { .. } if !history && !ready => history = true,
                                        HostEvent::History { .. } => return Err(failure()),
                                        HostEvent::Upsert { .. } if !ready => return Err(failure()),
                                        HostEvent::Stopped {} if stopping => stopping = false,
                                        HostEvent::Stopped {} => return Err(failure()),
                                        HostEvent::Submitted { revision, .. } => {
                                            if pending != Some(*revision) { return Err(failure()); }
                                            pending = None;
                                        }
                                        HostEvent::Failed {} => return Err(failure()),
                                        HostEvent::Answered { id, .. } => {
                                            if answering.as_ref() != Some(id) { return Err(failure()); }
                                            answering = None;
                                        }
                                        _ => {}
                                    }
                                    tokio::select! {
                                        _ = &mut stop => break,
                                        result = notices.send(event) => result.map_err(|_| failure())?,
                                    }
                                }
                            }
                        }
                        Ok(())
                    }.await;
                    // Only this owned bridge is reaped. The shared Host is never signalled.
                    if tokio::time::timeout(Duration::from_secs(2), child.wait()).await.is_err() {
                        child.start_kill()?;
                        tokio::time::timeout(Duration::from_secs(2), child.wait()).await.map_err(|_| failure())??;
                    }
                    outcome
                })
            })();
            let _ = done.send(outcome);
        })?;
        Ok(Self {
            commands,
            events,
            shutdown: Some(shutdown),
            completion,
            closed: false,
        })
    }

    pub(super) fn send(&self, command: HostCommand) -> io::Result<()> {
        self.commands.try_send(command).map_err(|_| failure())
    }

    pub(super) fn poll(&mut self) -> Option<HostEvent> {
        if self.closed {
            return None;
        }
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(async_mpsc::error::TryRecvError::Empty) => None,
            Err(async_mpsc::error::TryRecvError::Disconnected) => {
                self.closed = true;
                Some(HostEvent::Failed {})
            }
        }
    }

    pub(super) fn cancel(&mut self) {
        self.closed = true;
        if let Some(stop) = self.shutdown.take() {
            let _ = stop.send(());
        }
    }

    pub(super) fn close(mut self) -> io::Result<()> {
        self.cancel();
        self.completion
            .recv_timeout(Duration::from_secs(6))
            .map_err(|_| failure())?
    }
}

impl Drop for SessionHost {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn failure() -> io::Error {
    io::Error::other("Host 客户端连接中断；未自动重试")
}
