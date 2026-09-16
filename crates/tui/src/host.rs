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

//! Native, existing-Host-only catalog access using the shared epoch-154 crates.

use std::{fs, io::Read, path::Path, time::Duration};

use maka_protocol::{
    COMPATIBILITY_EPOCH, COMPOSITION_ID, Operation, OperationErrorCode, OperationRegistry, Outcome,
    PROTOCOL_VERSION, ProtocolError, Request,
    handshake::{ClientHello, HostHandshake, Lifecycle, decode_host_handshake},
    session::{
        SessionCatalogQueryResult, decode_session_catalog_query_input,
        decode_session_catalog_query_result,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const DEADLINE: Duration = Duration::from_secs(5);
const REGISTRATION_LIMIT: u64 = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("无法读取指定的 Host 注册文件")]
    Registration,
    #[error("Host 身份或协议不匹配；需要 epoch 154")]
    Identity,
    #[error("Host 尚未就绪或正在关闭；未发送会话查询")]
    NotReady,
    #[error("Host 连接或查询超时；未重试")]
    Timeout,
    #[error("Host 连接已关闭")]
    Closed,
    #[error("Host 传输失败")]
    Transport(#[from] maka_transport::TransportError),
    #[error("Host 响应不符合协议")]
    Protocol(#[from] ProtocolError),
    #[error("无法连接指定的本地 Host")]
    Connect(#[source] std::io::Error),
    #[error("Host 拒绝会话查询：{0:?}")]
    Rejected(OperationErrorCode),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registration {
    kind: String,
    schema_version: u64,
    root_id: String,
    host_epoch: String,
    endpoint: String,
    protocol_min: u64,
    protocol_max: u64,
    compatibility_epoch: u64,
    composition_id: String,
    composition_revision: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Catalog {
    root_id: String,
    host_epoch: String,
    page: SessionCatalogQueryResult,
}

/// Read exactly one page. No process creation, root writes, retries or commands.
/// The caller must trust the registration and endpoint; matching identities is
/// a consistency fence, not authentication of the local Host owner.
pub(super) async fn read_session_page(path: &Path) -> Result<Catalog, Error> {
    let registration = read_registration(path)?;
    tokio::time::timeout(DEADLINE, read_page(registration))
        .await
        .map_err(|_| Error::Timeout)?
}

fn read_registration(path: &Path) -> Result<Registration, Error> {
    let read = || -> Result<Registration, Error> {
        let metadata = fs::symlink_metadata(path).map_err(|_| Error::Registration)?;
        if !metadata.is_file() || metadata.len() > REGISTRATION_LIMIT {
            return Err(Error::Registration);
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path).map_err(|_| Error::Registration)?;
        if !file.metadata().map_err(|_| Error::Registration)?.is_file() {
            return Err(Error::Registration);
        }
        let mut bytes = Vec::new();
        file.take(REGISTRATION_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Registration)?;
        if bytes.len() as u64 > REGISTRATION_LIMIT {
            return Err(Error::Registration);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Registration)
    };
    let registration = read()?;
    if registration.kind != "maka-runtime-host"
        || registration.schema_version != 1
        || registration.compatibility_epoch != COMPATIBILITY_EPOCH
        || !(registration.protocol_min..=registration.protocol_max).contains(&PROTOCOL_VERSION)
        || registration.composition_id != COMPOSITION_ID
        || registration.root_id.len() != 64
        || !registration
            .root_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || registration.host_epoch.is_empty()
        || registration.host_epoch.len() > 128
        || registration.composition_revision.is_empty()
        || registration.composition_revision.len() > 128
    {
        return Err(Error::Identity);
    }
    Ok(registration)
}

#[cfg(unix)]
async fn open_stream(endpoint: &str) -> Result<tokio::net::UnixStream, Error> {
    if !Path::new(endpoint).is_absolute() {
        return Err(Error::Registration);
    }
    tokio::net::UnixStream::connect(endpoint)
        .await
        .map_err(Error::Connect)
}

#[cfg(windows)]
async fn open_stream(
    endpoint: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, Error> {
    let name = endpoint
        .strip_prefix(r"\\.\pipe\")
        .ok_or(Error::Registration)?;
    if name.is_empty() || name.contains(['\\', '/', '\0']) {
        return Err(Error::Registration);
    }
    tokio::net::windows::named_pipe::ClientOptions::new()
        .open(endpoint)
        .map_err(Error::Connect)
}

async fn read_page(registration: Registration) -> Result<Catalog, Error> {
    let stream = open_stream(&registration.endpoint).await?;
    let cancel = CancellationToken::new();
    let (mut reader, mut writer) = maka_transport::ndjson::split(stream, cancel);
    writer
        .write(&ClientHello {
            client_instance_id: format!("maka-tui-{}", uuid::Uuid::new_v4()),
            protocol_min: PROTOCOL_VERSION,
            protocol_max: PROTOCOL_VERSION,
            compatibility_epoch: COMPATIBILITY_EPOCH,
            composition_id: COMPOSITION_ID.into(),
            generation: None,
            takeover: None,
            activity_snapshot_version: None,
        })
        .await?;
    let hello = reader.read().await?.ok_or(Error::Closed)?;
    match decode_host_handshake(&hello)? {
        HostHandshake::Accepted {
            root_id,
            host_epoch,
            selected_protocol,
            compatibility_epoch,
            composition_id,
            composition_revision,
            state,
            ..
        } if root_id == registration.root_id
            && host_epoch == registration.host_epoch
            && selected_protocol == PROTOCOL_VERSION
            && compatibility_epoch == COMPATIBILITY_EPOCH
            && composition_id == COMPOSITION_ID
            && composition_revision == registration.composition_revision =>
        {
            if state != Lifecycle::Ready {
                return Err(Error::NotReady);
            }
        }
        _ => return Err(Error::Identity),
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    writer
        .write(&Request {
            request_id: request_id.clone(),
            operation: Operation::SessionCatalogQuery,
            input: json!({"kind": "list_start"}),
        })
        .await?;
    let frame = reader.read().await?.ok_or(Error::Closed)?;
    let response = maka_protocol::decode_response(&frame, &CatalogRegistry)?;
    if response.request_id != request_id || response.operation != Operation::SessionCatalogQuery {
        return Err(Error::Identity);
    }
    let page = match response.outcome {
        Outcome::Success { result } => decode_session_catalog_query_result(&result)?,
        Outcome::Failure { error } => return Err(Error::Rejected(error.code)),
    };
    if !matches!(page, SessionCatalogQueryResult::Page { .. }) {
        return Err(Error::Identity);
    }
    writer.close_after_flush().await?;
    Ok(Catalog {
        root_id: registration.root_id,
        host_epoch: registration.host_epoch,
        page,
    })
}

struct CatalogRegistry;

impl OperationRegistry for CatalogRegistry {
    fn decode_input(&self, operation: Operation, value: &Value) -> maka_protocol::Result<Value> {
        if operation != Operation::SessionCatalogQuery {
            return Err(ProtocolError::invalid("Unsupported TUI operation"));
        }
        decode_session_catalog_query_input(value)?;
        Ok(value.clone())
    }

    fn decode_output(&self, operation: Operation, value: &Value) -> maka_protocol::Result<Value> {
        if operation != Operation::SessionCatalogQuery {
            return Err(ProtocolError::invalid("Unsupported TUI operation"));
        }
        decode_session_catalog_query_result(value)?;
        Ok(value.clone())
    }

    fn error_codes(&self, operation: Operation) -> Option<&[OperationErrorCode]> {
        (operation == Operation::SessionCatalogQuery).then_some(&[
            OperationErrorCode::HostNotReady,
            OperationErrorCode::HostDraining,
            OperationErrorCode::OperationUnavailable,
            OperationErrorCode::InvalidRequest,
            OperationErrorCode::PersistenceFailed,
            OperationErrorCode::InternalFailure,
        ])
    }
}
