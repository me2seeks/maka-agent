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

//! Bounded transcript presentation with source-based reading anchors.
//!
//! This initial in-memory slice rejects excess content explicitly. It does not
//! load Host history, evict authoritative events, or claim paged replay support.

use crate::text;

const MAX_BLOCKS: usize = 128;
const MAX_BYTES: usize = 256 * 1024;
const MAX_BLOCK_BYTES: usize = 16 * 1024;

/// Presentation kind; execution status is retained in the block's title.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// Text sent by the user.
    User,
    /// Assistant response text.
    Assistant,
    /// Reasoning governed by the global thinking display policy.
    Thinking,
    /// Tool output with an always-visible status title.
    Tool,
}

/// One stable, appendable presentation block, not a second execution ledger.
#[derive(Debug)]
pub struct Block {
    id: u64,
    kind: BlockKind,
    title: String,
    text: String,
}

impl Block {
    /// Builds a block; [`Transcript::push`] validates size, text, and identity.
    pub fn new(id: u64, kind: BlockKind, title: String, text: String) -> Self {
        Self {
            id,
            kind,
            title,
            text,
        }
    }
}

/// Recoverable input errors leave existing content and reading state intact.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TranscriptError {
    /// The demo's explicit resident content budget would be exceeded.
    #[error("Demo history limit reached; existing content is preserved")]
    Budget,
    /// An identifier is absent or already in use.
    #[error("Invalid transcript block identity")]
    Identity,
    /// Terminal controls must not be interpreted as presentation content.
    #[error("Transcript contains unsupported control characters")]
    ControlCharacter,
}

/// Source coordinates distinguish wrapped headings from wrapped body content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourcePosition {
    /// Byte offset in the heading, including its fixed-width thinking marker.
    Heading(usize),
    /// Byte offset in the body, retained even while that body is collapsed.
    Body(usize),
}

/// A source position independent of terminal rows and display width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    /// Stable block identity.
    pub block_id: u64,
    /// UTF-8 source coordinate within the heading or body.
    pub position: SourcePosition,
}

/// Whether updates follow the tail or preserve a source anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// Keep the newest output visible.
    FollowTail,
    /// Preserve the topmost source position until explicit navigation.
    Anchored(Anchor),
}

/// One fully prepared terminal row.
#[derive(Debug)]
pub struct Row {
    /// Stable content position represented by this row.
    pub anchor: Anchor,
    /// Display text with tabs expanded and no escape sequences.
    pub text: String,
    /// Used for emphasis without owning any execution facts.
    pub heading: bool,
    /// Kind of the owning block.
    pub kind: BlockKind,
}

/// Owns wrapping, global thinking policy, follow state, and semantic anchoring.
#[derive(Debug)]
pub struct Transcript {
    blocks: Vec<Block>,
    bytes: usize,
    rows: Vec<Row>,
    width: u16,
    height: u16,
    thinking: bool,
    reading: Reading,
    unread: usize,
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

impl Transcript {
    /// Creates an empty bounded reader. Limits are documented in the demo README.
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            bytes: 0,
            rows: Vec::new(),
            width: 80,
            height: 20,
            thinking: false,
            reading: Reading::FollowTail,
            unread: 0,
        }
    }

    /// Adds a block without moving an anchored reader.
    ///
    /// # Errors
    /// Rejects duplicate IDs, terminal controls, or resident budget overflow.
    pub fn push(&mut self, block: Block) -> Result<(), TranscriptError> {
        if self.blocks.iter().any(|existing| existing.id == block.id) {
            return Err(TranscriptError::Identity);
        }
        validate_text(&block.text)?;
        validate_text(&block.title)?;
        if block.title.contains(['\n', '\t']) || block.title.len() > 256 {
            return Err(TranscriptError::ControlCharacter);
        }
        let size = block.text.len().saturating_add(block.title.len());
        if self.blocks.len() >= MAX_BLOCKS
            || block.text.len() > MAX_BLOCK_BYTES
            || self.bytes.saturating_add(size) > MAX_BYTES
        {
            return Err(TranscriptError::Budget);
        }
        self.bytes += size;
        self.blocks.push(block);
        self.changed();
        Ok(())
    }

    /// Replaces a stable display block without changing the reader's follow policy.
    ///
    /// # Errors
    /// Rejects unknown IDs, terminal controls, or resident budget overflow.
    pub fn replace(&mut self, block: Block) -> Result<(), TranscriptError> {
        let Some(index) = self
            .blocks
            .iter()
            .position(|existing| existing.id == block.id)
        else {
            return self.push(block);
        };
        validate_text(&block.text)?;
        validate_text(&block.title)?;
        if block.title.contains(['\n', '\t']) || block.title.len() > 256 {
            return Err(TranscriptError::ControlCharacter);
        }
        let old = &self.blocks[index];
        let bytes =
            self.bytes - old.text.len() - old.title.len() + block.text.len() + block.title.len();
        if block.text.len() > MAX_BLOCK_BYTES || bytes > MAX_BYTES {
            return Err(TranscriptError::Budget);
        }
        if old.text == block.text && old.title == block.title && old.kind == block.kind {
            return Ok(());
        }
        self.bytes = bytes;
        self.blocks[index] = block;
        self.changed();
        Ok(())
    }

    /// Appends a streaming chunk atomically; rejected chunks do not change data.
    ///
    /// # Errors
    /// Rejects unknown IDs, terminal controls, or resident budget overflow.
    pub fn append(&mut self, id: u64, chunk: &str) -> Result<(), TranscriptError> {
        validate_text(chunk)?;
        let block = self
            .blocks
            .iter_mut()
            .find(|block| block.id == id)
            .ok_or(TranscriptError::Identity)?;
        if block.text.len().saturating_add(chunk.len()) > MAX_BLOCK_BYTES
            || self.bytes.saturating_add(chunk.len()) > MAX_BYTES
        {
            return Err(TranscriptError::Budget);
        }
        if chunk.is_empty() {
            return Ok(());
        }
        block.text.push_str(chunk);
        self.bytes += chunk.len();
        self.changed();
        Ok(())
    }

    /// Updates geometry without changing follow policy or semantic position.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.height = height;
        if self.width != width.max(1) {
            self.width = width.max(1);
            self.reflow();
        }
    }

    /// Changes the policy for all existing and future thinking blocks at once.
    pub fn toggle_thinking(&mut self) {
        self.thinking = !self.thinking;
        self.reflow();
    }

    /// Whether thinking bodies are currently expanded.
    pub fn thinking_expanded(&self) -> bool {
        self.thinking
    }

    /// Current source anchor or explicit follow state.
    pub fn reading(&self) -> Reading {
        self.reading
    }

    /// Number of content updates observed while reading history, not messages.
    pub fn unread_updates(&self) -> usize {
        self.unread
    }

    /// Explicitly returns to live output and clears its unread-update indicator.
    pub fn follow_latest(&mut self) {
        self.reading = Reading::FollowTail;
        self.unread = 0;
    }

    /// Scrolls without affecting Composer focus. Reaching the end while reading
    /// does not opt back into live following; use [`Self::follow_latest`].
    pub fn scroll(&mut self, delta: isize) {
        if self.rows.is_empty() || self.height == 0 {
            return;
        }
        let top = self.top();
        let maximum = self.rows.len().saturating_sub(usize::from(self.height));
        let target = top.saturating_add_signed(delta).min(maximum);
        if delta == 0 || (self.reading == Reading::FollowTail && target == top && delta > 0) {
            return;
        }
        if let Some(row) = self.rows.get(target) {
            self.reading = Reading::Anchored(row.anchor);
        }
    }

    /// Complete visible rows. No caller needs to reconcile row indices.
    pub fn visible(&self) -> &[Row] {
        let top = self.top();
        let end = top
            .saturating_add(usize::from(self.height))
            .min(self.rows.len());
        &self.rows[top..end]
    }

    fn top(&self) -> usize {
        match self.reading {
            Reading::FollowTail => self.rows.len().saturating_sub(usize::from(self.height)),
            Reading::Anchored(anchor) => {
                let mut heading = None;
                let mut found = None;
                for (index, row) in self.rows.iter().enumerate() {
                    if row.anchor.block_id != anchor.block_id {
                        continue;
                    }
                    heading.get_or_insert(index);
                    match (anchor.position, row.anchor.position) {
                        (SourcePosition::Heading(wanted), SourcePosition::Heading(offset))
                        | (SourcePosition::Body(wanted), SourcePosition::Body(offset))
                            if offset <= wanted =>
                        {
                            found = Some(index)
                        }
                        _ => {}
                    }
                }
                found.or(heading).unwrap_or(0)
            }
        }
    }

    fn changed(&mut self) {
        if matches!(self.reading, Reading::Anchored(_)) {
            self.unread = self.unread.saturating_add(1);
        }
        self.reflow();
    }

    fn reflow(&mut self) {
        let mut rows = Vec::new();
        for block in &self.blocks {
            let prefix = if block.kind == BlockKind::Thinking {
                if self.thinking { "[-] " } else { "[+] " }
            } else {
                ""
            };
            for line in text::wrap(&format!("{prefix}{}", block.title), self.width) {
                rows.push(Row {
                    anchor: Anchor {
                        block_id: block.id,
                        position: SourcePosition::Heading(line.start),
                    },
                    text: line.text,
                    heading: true,
                    kind: block.kind,
                });
            }
            if block.kind != BlockKind::Thinking || self.thinking {
                for line in text::wrap(&block.text, self.width) {
                    rows.push(Row {
                        anchor: Anchor {
                            block_id: block.id,
                            position: SourcePosition::Body(line.start),
                        },
                        text: line.text,
                        heading: false,
                        kind: block.kind,
                    });
                }
            }
        }
        // Readers never observe a partially rebuilt layout, including folds.
        self.rows = rows;
    }
}

fn validate_text(text: &str) -> Result<(), TranscriptError> {
    if text
        .chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        Err(TranscriptError::ControlCharacter)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader() -> Transcript {
        let mut reader = Transcript::new();
        reader.resize(20, 5);
        for id in 1..=5 {
            reader
                .push(Block::new(
                    id,
                    BlockKind::Thinking,
                    format!("Thinking {id}"),
                    "旧内容 e\u{301} 👩‍👩‍👧‍👦\n".repeat(12),
                ))
                .unwrap();
        }
        reader.toggle_thinking();
        reader.scroll(-35);
        reader
    }

    #[test]
    fn streaming_and_resize_preserve_the_source_anchor() {
        let mut reader = reader();
        let anchor = reader.reading();
        reader.append(5, "more text").unwrap();
        reader.resize(9, 3);
        reader.resize(30, 7);
        assert_eq!(reader.reading(), anchor);
        assert_eq!(reader.unread_updates(), 1);
    }

    #[test]
    fn folding_keeps_hidden_content_anchor_and_returns_to_it() {
        let mut reader = reader();
        let anchor = reader.reading();
        reader.toggle_thinking();
        assert!(reader.visible()[0].heading);
        reader
            .push(Block::new(
                6,
                BlockKind::Thinking,
                "future".into(),
                "hidden".into(),
            ))
            .unwrap();
        assert!(!reader.rows.iter().any(|row| row.text == "hidden"));
        reader.toggle_thinking();
        assert_eq!(reader.reading(), anchor);
        assert!(!reader.visible()[0].heading);
    }

    #[test]
    fn budget_failure_is_atomic() {
        let mut reader = reader();
        let rows = reader.rows.len();
        assert_eq!(
            reader.append(5, &"x".repeat(MAX_BLOCK_BYTES)),
            Err(TranscriptError::Budget)
        );
        assert_eq!(reader.rows.len(), rows);
        assert_eq!(reader.unread_updates(), 0);
    }

    #[test]
    fn explicit_latest_restores_following() {
        let mut reader = reader();
        reader.append(5, "tail").unwrap();
        reader.follow_latest();
        assert_eq!(
            (reader.reading(), reader.unread_updates()),
            (Reading::FollowTail, 0)
        );
    }

    #[test]
    fn each_wrapped_heading_row_can_be_scrolled_and_fold_returns_to_heading_start() {
        let mut reader = Transcript::new();
        reader.resize(24, 2);
        reader
            .push(Block::new(
                1,
                BlockKind::Thinking,
                "Thinking 1 · completed fixture".into(),
                "body\n".repeat(8),
            ))
            .unwrap();
        reader.toggle_thinking();
        reader.scroll(isize::MIN);
        let first = reader.visible()[0].text.clone();
        reader.scroll(1);
        assert_ne!(reader.visible()[0].text, first);
        assert!(reader.visible()[0].heading);
        reader.scroll(2);
        assert!(!reader.visible()[0].heading);
        reader.toggle_thinking();
        assert!(reader.visible()[0].text.starts_with("[+] Thinking 1"));
    }

    #[test]
    fn scrolling_to_the_end_does_not_reenable_live_follow() {
        let mut reader = reader();
        reader.scroll(isize::MAX);
        let anchor = reader.reading();
        assert!(matches!(anchor, Reading::Anchored(_)));
        reader.append(5, "new output\n".repeat(8).as_str()).unwrap();
        assert_eq!(reader.reading(), anchor);
        assert_eq!(reader.unread_updates(), 1);
    }

    #[test]
    fn escape_sequences_are_rejected_before_drawing() {
        let mut reader = Transcript::new();
        assert_eq!(
            reader.push(Block::new(
                1,
                BlockKind::Tool,
                "output".into(),
                "\u{1b}]52;c;secret\u{7}".into()
            )),
            Err(TranscriptError::ControlCharacter)
        );
        assert!(reader.visible().is_empty());
    }
}
