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

//! Narrow presentation messages from the pinned TS Host client, not Host facts.

use serde::{Deserialize, Serialize};

/// A bounded display block. Its identity is scoped to the selected session.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBlock {
    /// Stable presentation identity.
    pub id: String,
    /// Rendering category.
    pub kind: HostBlockKind,
    /// Plain heading, never terminal escape sequences.
    pub title: String,
    /// Plain body.
    pub text: String,
}

/// The four display categories understood by the initial chat view.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostBlockKind {
    /// User input.
    User,
    /// Assistant text.
    Assistant,
    /// Assistant reasoning.
    Thinking,
    /// Tool or unsupported-message summary.
    Tool,
}

/// Typed events; no arbitrary operation forwarding is exposed to the UI.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostEvent {
    /// Private protocol pairing, consumed before any UI event.
    Hello {
        /// Local bridge protocol version.
        version: u8,
    },
    /// Session bootstrap finished; sending may now be enabled.
    Ready {
        /// Exact session selected by the user.
        session_id: String,
    },
    /// Initial bounded transcript.
    History {
        /// Blocks in presentation order.
        blocks: Vec<HostBlock>,
    },
    /// Live replacement of one block, retaining reading anchors.
    Upsert {
        /// Complete current display value.
        block: HostBlock,
    },
    /// Host-owned execution status.
    State {
        /// A root Turn remains active.
        running: bool,
        /// User action is required through a capable client.
        waiting: bool,
    },
    /// A definitive submit response, not a local echo.
    Submitted {
        /// Composer revision sent with this intent.
        revision: u64,
        /// Whether admission accepted the content.
        accepted: bool,
    },
    /// The single outstanding stop request received a definitive Host response.
    Stopped {},
    /// Fixed, non-sensitive client status.
    Notice {
        /// Plain status text.
        text: String,
    },
    /// Connection ended; any unacknowledged command has an unknown outcome.
    Failed {},
}

/// Explicit user intents for the existing session only.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostCommand {
    /// Submit text exactly once.
    Send {
        /// Local revision to correlate the receipt with the retained input.
        revision: u64,
        /// Complete user input.
        text: String,
    },
    /// Stop the observed active root Turn; never close the Host.
    Stop,
    /// Close this client connection only.
    Close,
}
