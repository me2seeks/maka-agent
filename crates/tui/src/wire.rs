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

//! Bounded, versioned frames for the private M0 Companion experiment.

use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};

pub(super) const MAX_FRAME_BYTES: usize = 16 * 1024;
pub(super) const MAX_ECHO_BYTES: usize = 4096;
// This pairs the two source prototypes; it is not a released artifact hash.
pub(super) const BUILD_ID: &str = "maka-tui-m0-ipc-1";
const MAGIC: [u8; 4] = *b"MKUI";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 8;
const MAX_BUILD_ID_BYTES: usize = 128;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Message {
    Hello { build_id: String },
    Ready { build_id: String },
    Echo { request_id: u32, text: String },
    Echoed { request_id: u32, text: String },
    Shutdown {},
    Bye {},
}

#[derive(Debug, Error)]
pub(super) enum WireError {
    #[error("Companion pipe I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid Companion frame magic")]
    Magic,
    #[error("unsupported Companion protocol version")]
    Version,
    #[error("Companion frame exceeds its byte budget or is empty")]
    FrameLength,
    // Do not render a serde diagnostic containing untrusted peer text in the TTY.
    #[error("invalid Companion JSON message")]
    Json(#[source] serde_json::Error),
    #[error("invalid Companion build identifier")]
    BuildId,
    #[error("Companion request identifiers must be nonzero")]
    RequestId,
    #[error("Companion echo exceeds its byte budget")]
    EchoLength,
}

impl Message {
    fn validate(&self) -> Result<(), WireError> {
        match self {
            Self::Hello { build_id } | Self::Ready { build_id }
                if build_id.is_empty() || build_id.len() > MAX_BUILD_ID_BYTES =>
            {
                Err(WireError::BuildId)
            }
            Self::Echo { request_id: 0, .. } | Self::Echoed { request_id: 0, .. } => {
                Err(WireError::RequestId)
            }
            Self::Echo { text, .. } | Self::Echoed { text, .. } if text.len() > MAX_ECHO_BYTES => {
                Err(WireError::EchoLength)
            }
            _ => Ok(()),
        }
    }
}

fn payload_length(header: [u8; HEADER_BYTES]) -> Result<usize, WireError> {
    if header[..4] != MAGIC {
        return Err(WireError::Magic);
    }
    if u16::from_be_bytes([header[4], header[5]]) != VERSION {
        return Err(WireError::Version);
    }
    let length = usize::from(u16::from_be_bytes([header[6], header[7]]));
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(WireError::FrameLength);
    }
    Ok(length)
}

fn decode_payload(payload: &[u8]) -> Result<Message, WireError> {
    let message: Message = serde_json::from_slice(payload).map_err(WireError::Json)?;
    message.validate()?;
    Ok(message)
}

/// Read one frame. Keep this future alive until completion or discard the pipe.
pub(super) async fn read(reader: &mut (impl AsyncRead + Unpin)) -> Result<Message, WireError> {
    let mut header = [0; HEADER_BYTES];
    reader.read_exact(&mut header).await?;
    // No payload allocation occurs until magic, version, and size are admitted.
    let length = payload_length(header)?;
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).await?;
    decode_payload(&payload)
}

/// Encode within the same bound enforced by the reader, including JSON escaping.
pub(super) fn encode(message: &Message) -> Result<Vec<u8>, WireError> {
    message.validate()?;
    let mut payload = BoundedPayload(Vec::new());
    if let Err(error) = serde_json::to_writer(&mut payload, message) {
        return if error.is_io() {
            Err(WireError::FrameLength)
        } else {
            Err(WireError::Json(error))
        };
    }
    let length = u16::try_from(payload.0.len()).map_err(|_| WireError::FrameLength)?;
    let mut frame = Vec::with_capacity(HEADER_BYTES + payload.0.len());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&VERSION.to_be_bytes());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload.0);
    Ok(frame)
}

struct BoundedPayload(Vec<u8>);

impl Write for BoundedPayload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("frame byte budget exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_bytes(bytes: &[u8]) -> Result<Message, WireError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut reader = bytes;
        runtime.block_on(read(&mut reader))
    }

    fn raw_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::from(MAGIC);
        frame.extend_from_slice(&VERSION.to_be_bytes());
        frame.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn frame_round_trips_multibyte_text_without_normalization() {
        let message = Message::Echo {
            request_id: 7,
            text: "中文 e\u{301} 👩‍💻\n\t".to_owned(),
        };
        assert_eq!(read_bytes(&encode(&message).unwrap()).unwrap(), message);
    }

    #[test]
    fn reader_rejects_oversized_header_without_reading_a_payload() {
        let mut frame = Vec::from(MAGIC);
        frame.extend_from_slice(&VERSION.to_be_bytes());
        frame.extend_from_slice(&u16::MAX.to_be_bytes());
        assert!(matches!(read_bytes(&frame), Err(WireError::FrameLength)));
    }

    #[test]
    fn reader_rejects_empty_payload_before_json_decoding() {
        assert!(matches!(
            read_bytes(&raw_frame(b"")),
            Err(WireError::FrameLength)
        ));
    }

    #[test]
    fn reader_rejects_invalid_magic() {
        let mut frame = raw_frame(br#"{"type":"bye"}"#);
        frame[0] = b'X';
        assert!(matches!(read_bytes(&frame), Err(WireError::Magic)));
    }

    #[test]
    fn reader_rejects_unknown_version() {
        let mut frame = raw_frame(br#"{"type":"bye"}"#);
        frame[5] = 2;
        assert!(matches!(read_bytes(&frame), Err(WireError::Version)));
    }

    #[test]
    fn reader_reports_truncated_header_as_unexpected_eof() {
        assert!(
            matches!(read_bytes(b"MKU"), Err(WireError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof)
        );
    }

    #[test]
    fn reader_reports_truncated_payload_as_unexpected_eof() {
        let mut frame = raw_frame(br#"{"type":"bye"}"#);
        frame.pop();
        assert!(
            matches!(read_bytes(&frame), Err(WireError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof)
        );
    }

    #[test]
    fn reader_rejects_non_utf8_payload() {
        assert!(matches!(
            read_bytes(&raw_frame(b"\xff")),
            Err(WireError::Json(_))
        ));
    }

    #[test]
    fn reader_rejects_unknown_fields_on_empty_variant() {
        assert!(matches!(
            read_bytes(&raw_frame(br#"{"type":"bye","extra":true}"#)),
            Err(WireError::Json(_))
        ));
    }

    #[test]
    fn reader_rejects_unknown_fields_on_data_variant() {
        assert!(matches!(
            read_bytes(&raw_frame(
                br#"{"type":"ready","build_id":"x","extra":true}"#
            )),
            Err(WireError::Json(_))
        ));
    }

    #[test]
    fn reader_rejects_unknown_message_type() {
        assert!(matches!(
            read_bytes(&raw_frame(br#"{"type":"execute"}"#)),
            Err(WireError::Json(_))
        ));
    }

    #[test]
    fn reader_rejects_duplicate_required_fields() {
        assert!(matches!(
            read_bytes(&raw_frame(
                br#"{"type":"ready","build_id":"x","build_id":"y"}"#
            )),
            Err(WireError::Json(_))
        ));
    }

    #[test]
    fn reader_admits_exactly_the_payload_budget() {
        let mut payload = Vec::from(br#"{"type":"bye"}"#);
        payload.resize(MAX_FRAME_BYTES, b' ');
        assert_eq!(read_bytes(&raw_frame(&payload)).unwrap(), Message::Bye {});
    }

    #[test]
    fn one_read_does_not_consume_the_following_frame() {
        let first = Message::Ready {
            build_id: BUILD_ID.to_owned(),
        };
        let mut bytes = encode(&first).unwrap();
        bytes.extend_from_slice(&encode(&Message::Bye {}).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut reader = bytes.as_slice();
        runtime.block_on(read(&mut reader)).unwrap();
        assert_eq!(
            runtime.block_on(read(&mut reader)).unwrap(),
            Message::Bye {}
        );
    }

    #[test]
    fn reader_rejects_zero_request_identifier() {
        assert!(matches!(
            read_bytes(&raw_frame(br#"{"type":"echoed","request_id":0,"text":""}"#)),
            Err(WireError::RequestId)
        ));
    }

    #[test]
    fn echo_budget_counts_utf8_bytes() {
        let message = Message::Echo {
            request_id: 1,
            text: "中".repeat(MAX_ECHO_BYTES / 3 + 1),
        };
        assert!(matches!(encode(&message), Err(WireError::EchoLength)));
    }

    #[test]
    fn encoder_bounds_json_escape_expansion() {
        let message = Message::Echo {
            request_id: 1,
            text: "\0".repeat(MAX_ECHO_BYTES),
        };
        assert!(matches!(encode(&message), Err(WireError::FrameLength)));
    }

    #[test]
    fn invalid_json_diagnostic_does_not_echo_peer_control_sequences() {
        let error = read_bytes(&raw_frame(br#"{"type":"\u001b[31m"}"#)).unwrap_err();
        assert_eq!(error.to_string(), "invalid Companion JSON message");
    }
}
