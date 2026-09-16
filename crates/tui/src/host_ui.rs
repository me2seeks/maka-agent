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
use std::collections::BTreeMap;

/// A bounded, declarative interaction projected by the pinned TS client.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HostInteraction {
    /// Host-owned request identity, never an array position.
    pub id: String,
    /// Sandbox boundary, client capability, question, form, or unsupported request.
    pub kind: String,
    /// Plain display heading.
    pub title: String,
    /// Display provenance; not an authorization identity.
    pub source: String,
    /// Complete review text; the client must not silently truncate it.
    pub detail: String,
    /// Declarative fields; empty for permissions.
    pub fields: Vec<HostField>,
}

/// A form field using the existing Host field vocabulary.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostField {
    /// Stable field key used in answers.
    pub name: String,
    /// Plain field caption.
    pub label: String,
    /// Host field kind, or question for a choice with free text.
    pub kind: String,
    /// Whether omission is invalid.
    pub required: bool,
    /// Optional explanation.
    #[serde(default)]
    pub description: String,
    /// Host-provided initial value; never submitted implicitly.
    #[serde(default)]
    pub default: serde_json::Value,
    /// Choice values and captions.
    #[serde(default)]
    pub options: Vec<HostOption>,
    /// Minimum Unicode length.
    pub min_length: Option<usize>,
    /// Maximum Unicode length.
    pub max_length: Option<usize>,
    /// Optional string format checked by Host on submission.
    pub format: Option<String>,
    /// Inclusive numeric minimum.
    pub minimum: Option<f64>,
    /// Inclusive numeric maximum.
    pub maximum: Option<f64>,
    /// Minimum number of selected values.
    pub min_items: Option<usize>,
    /// Maximum number of selected values.
    pub max_items: Option<usize>,
}

/// One declared choice, with distinct wire value and display caption.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HostOption {
    /// Exact canonical answer value.
    pub value: String,
    /// Plain display caption.
    pub label: String,
}

/// Explicit decisions; never includes persistent grants in this UI slice.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InteractionAction {
    /// Allow this one permission request.
    Allow,
    /// Deny this permission request.
    Deny,
    /// Submit form or question values.
    Accept,
    /// Decline a form.
    Decline,
    /// Cancel a form or skip a question request.
    Cancel,
}

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
    /// Complete current pending-interaction projection.
    Interactions {
        /// Host-owned pending requests, bounded to sixteen.
        requests: Vec<HostInteraction>,
    },
    /// Definitive receipt for the one outstanding answer intent.
    Answered {
        /// Exact request answered by the user.
        id: String,
        /// Whether Host committed that answer.
        accepted: bool,
    },
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
    /// Answer one exact pending interaction through the Host.
    Answer {
        /// Host request identity retained from the displayed projection.
        id: String,
        /// Explicit user decision.
        action: InteractionAction,
        /// Form values keyed by declared field name only.
        values: BTreeMap<String, serde_json::Value>,
    },
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
