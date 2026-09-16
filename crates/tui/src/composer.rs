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

//! Bounded, grapheme-aware draft editing independent of terminal and Host IO.
//!
//! Each text edit is one transaction. Sending a draft and deciding when to clear
//! it belong to the caller; this module never submits or clears a draft itself.

use std::collections::VecDeque;
use std::ops::Range;

use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::text::{self, columns};

const MAX_HISTORY_ENTRIES: usize = 128;
const MAX_HISTORY_BYTES: usize = 4 * 1024 * 1024;
const TAB_COLUMNS: usize = 4;

/// One atomic edit or cursor movement in a [`Composer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Insert a complete input event, including a paste, as one undo transaction.
    /// CRLF becomes LF; LF and tab are allowed, and other controls are rejected.
    Insert(String),
    /// Move left over one extended grapheme cluster, including a line break.
    Left,
    /// Move right over one extended grapheme cluster, including a line break.
    Right,
    /// Remove the complete grapheme immediately before the cursor.
    Backspace,
    /// Remove the complete grapheme immediately after the cursor.
    Delete,
    /// Move to the beginning of the current logical line.
    Home,
    /// Move to the end of the current logical line, before its LF if present.
    End,
    /// Move to the preceding logical line, preserving the desired display column.
    Up,
    /// Move to the next logical line, preserving the desired display column.
    Down,
    /// Restore the text and cursor from before the most recent retained edit.
    Undo,
    /// Restore the text and cursor from after the most recently undone edit.
    Redo,
}

/// A rejected edit. Rejection leaves the draft, cursor, revision and history intact.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EditError {
    /// The normalized input would exceed the configured UTF-8 byte limit.
    #[error("draft would exceed the {max_bytes}-byte limit")]
    TooLarge {
        /// Maximum permitted UTF-8 bytes in the entire draft.
        max_bytes: usize,
    },
    /// Input contains a Unicode control other than LF or tab after CRLF normalization.
    #[error("input contains an unsupported control character: {character:?}")]
    InvalidControl {
        /// First unsupported control, including a standalone CR or ESC.
        character: char,
    },
    /// No further text revision can be represented without reusing an identifier.
    #[error("draft revision counter is exhausted")]
    RevisionExhausted,
}

/// A UTF-8 draft whose cursor always lies on an extended grapheme boundary.
///
/// Text is limited by the byte budget passed to [`Self::new`]. Undo and redo
/// together retain at most 128 transactions and 4 MiB of snapshot text. Oldest
/// transactions are discarded first. An edit whose before/after snapshots alone
/// exceed 4 MiB succeeds but clears history, so undo cannot cross that edit.
///
/// [`Edit::Up`] and [`Edit::Down`] use logical LF-separated lines, Unicode display
/// widths, and tab stops every four columns. [`Self::move_visual`] instead follows
/// visual rows at an explicit viewport width. A column inside a wide grapheme
/// selects its leading edge. Repeated vertical moves preserve the desired column
/// across shorter lines within the same navigation mode and viewport width.
///
/// Edits and navigation scan the bounded draft; text transactions retain complete
/// snapshots rather than an unbounded sequence of per-character operations.
#[derive(Debug)]
pub struct Composer {
    text: String,
    cursor: usize,
    revision: u64,
    max_bytes: usize,
    desired_column: Option<DesiredColumn>,
    history: VecDeque<Change>,
    history_position: usize,
    history_bytes: usize,
}

impl Composer {
    /// Create an empty draft. A zero byte limit permits navigation but no text.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            revision: 0,
            max_bytes,
            desired_column: None,
            history: VecDeque::new(),
            history_position: 0,
            history_bytes: 0,
        }
    }

    /// Borrow the entire draft, with line breaks represented by LF.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Return the cursor's UTF-8 byte offset, always on a grapheme boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Return the text revision, starting at zero and advancing for text edits,
    /// undo and redo. Cursor movement and rejected or empty edits do not advance it.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Return the zero-based logical line and display column of the cursor.
    /// Tabs use four-column stops, matching vertical navigation.
    pub fn line_column(&self) -> (usize, usize) {
        let prefix = &self.text[..self.cursor];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let start = self.line_start(self.cursor);
        let column = columns(&self.text[start..self.cursor]);
        (line, column)
    }

    /// Move by one wrapped visual row using the same layout as draft rendering.
    ///
    /// `width` is the editable area's column count, excluding borders. Zero uses
    /// a one-column fallback. Full logical-line ends include an empty visual row
    /// for the insertion cursor, without adding text. Soft-wrap boundaries belong
    /// to the following row, so movement never selects an off-screen column.
    ///
    /// Repeated moves preserve the desired column across shorter rows. Changing
    /// width or switching from logical navigation resets that goal to the current
    /// visual column. Graphemes too wide for the viewport follow the renderer's
    /// one-cell replacement; the source text remains intact.
    ///
    /// Returns whether the cursor changed. The first and last visual rows clamp
    /// movement; draft text, revision and undo history are never changed.
    pub fn move_visual(&mut self, direction: VerticalDirection, width: u16) -> bool {
        let width = width.max(1);
        let layout = text::composer_layout(&self.text, self.cursor, width);
        let target_row = match direction {
            VerticalDirection::Up => layout.cursor_row.checked_sub(1),
            VerticalDirection::Down => layout.cursor_row.checked_add(1),
        };
        let column = match self.desired_column {
            Some(DesiredColumn::Visual {
                width: previous_width,
                column,
            }) if previous_width == width => column,
            _ => usize::from(layout.cursor_column),
        };
        self.desired_column = Some(DesiredColumn::Visual { width, column });
        let Some(cursor) = target_row.and_then(|row| layout.cursor_on_row(row, column)) else {
            return false;
        };
        let changed = self.cursor != cursor;
        self.cursor = cursor;
        changed
    }

    /// Apply an input event atomically, returning whether text or cursor changed.
    ///
    /// Insertions, including multiline pastes, form one undo transaction each.
    /// Navigation never adds history. An out-of-range movement or deletion is a
    /// successful no-op. After text joins graphemes, insertion snaps the cursor
    /// forward and deletion snaps it backward to a valid grapheme boundary.
    ///
    /// # Errors
    /// Returns [`EditError`] if input includes an unsupported control, the draft
    /// would exceed its byte limit, or the text revision is exhausted. No portion
    /// of rejected input is inserted, and no state is changed.
    pub fn update(&mut self, edit: Edit) -> Result<bool, EditError> {
        match edit {
            Edit::Insert(input) => self.insert(input),
            Edit::Backspace => {
                let start = previous_boundary(&self.text, self.cursor);
                self.delete(start..self.cursor)
            }
            Edit::Delete => {
                let end = next_boundary(&self.text, self.cursor);
                self.delete(self.cursor..end)
            }
            Edit::Left => Ok(self.move_cursor(previous_boundary(&self.text, self.cursor))),
            Edit::Right => Ok(self.move_cursor(next_boundary(&self.text, self.cursor))),
            Edit::Home => Ok(self.move_cursor(self.line_start(self.cursor))),
            Edit::End => Ok(self.move_cursor(self.line_end(self.cursor))),
            Edit::Up => Ok(self.move_vertical(VerticalDirection::Up)),
            Edit::Down => Ok(self.move_vertical(VerticalDirection::Down)),
            Edit::Undo => self.undo(),
            Edit::Redo => self.redo(),
        }
    }

    fn insert(&mut self, input: String) -> Result<bool, EditError> {
        let input = if input.contains('\r') {
            input.replace("\r\n", "\n")
        } else {
            input
        };
        if let Some(character) = input
            .chars()
            .find(|character| character.is_control() && !matches!(character, '\n' | '\t'))
        {
            return Err(EditError::InvalidControl { character });
        }
        if input.is_empty() {
            return Ok(false);
        }
        let new_len = self.text.len().checked_add(input.len());
        let Some(new_len) = new_len.filter(|length| *length <= self.max_bytes) else {
            return Err(EditError::TooLarge {
                max_bytes: self.max_bytes,
            });
        };
        let mut text = String::with_capacity(new_len);
        text.push_str(&self.text[..self.cursor]);
        text.push_str(&input);
        text.push_str(&self.text[self.cursor..]);
        // Inserting a joiner or base character can merge with the following
        // grapheme, so the byte immediately after input is not always a boundary.
        let cursor = boundary_at_or_after(&text, self.cursor + input.len());
        self.commit(text, cursor)
    }

    fn delete(&mut self, range: Range<usize>) -> Result<bool, EditError> {
        if range.is_empty() {
            return Ok(false);
        }
        let mut text = self.text.clone();
        text.replace_range(range.clone(), "");
        let cursor = boundary_at_or_before(&text, range.start);
        self.commit(text, cursor)
    }

    fn commit(&mut self, text: String, cursor: usize) -> Result<bool, EditError> {
        let revision = self.next_revision()?;
        let bytes = self.text.len().checked_add(text.len());
        if let Some(bytes) = bytes.filter(|bytes| *bytes <= MAX_HISTORY_BYTES) {
            while self.history.len() > self.history_position {
                if let Some(change) = self.history.pop_back() {
                    self.history_bytes -= change.bytes;
                }
            }
            self.history.push_back(Change {
                before: Snapshot::new(&self.text, self.cursor),
                after: Snapshot::new(&text, cursor),
                bytes,
            });
            self.history_bytes += bytes;
            self.history_position += 1;
            while self.history.len() > MAX_HISTORY_ENTRIES || self.history_bytes > MAX_HISTORY_BYTES
            {
                if let Some(change) = self.history.pop_front() {
                    self.history_bytes -= change.bytes;
                    self.history_position -= 1;
                }
            }
        } else {
            // Crossing a change without its complete before/after pair would
            // restore an unrelated draft, so oversized changes sever history.
            self.history.clear();
            self.history_position = 0;
            self.history_bytes = 0;
        }
        self.text = text;
        self.cursor = cursor;
        self.revision = revision;
        self.desired_column = None;
        Ok(true)
    }

    fn undo(&mut self) -> Result<bool, EditError> {
        let Some(position) = self.history_position.checked_sub(1) else {
            return Ok(false);
        };
        let Some(change) = self.history.get(position) else {
            return Ok(false);
        };
        let revision = self.next_revision()?;
        self.text = change.before.text.to_string();
        self.cursor = change.before.cursor;
        self.revision = revision;
        self.history_position = position;
        self.desired_column = None;
        Ok(true)
    }

    fn redo(&mut self) -> Result<bool, EditError> {
        let Some(change) = self.history.get(self.history_position) else {
            return Ok(false);
        };
        let revision = self.next_revision()?;
        self.text = change.after.text.to_string();
        self.cursor = change.after.cursor;
        self.revision = revision;
        self.history_position += 1;
        self.desired_column = None;
        Ok(true)
    }

    fn next_revision(&self) -> Result<u64, EditError> {
        self.revision
            .checked_add(1)
            .ok_or(EditError::RevisionExhausted)
    }

    fn move_cursor(&mut self, cursor: usize) -> bool {
        let changed = self.cursor != cursor;
        self.cursor = cursor;
        self.desired_column = None;
        changed
    }

    fn move_vertical(&mut self, direction: VerticalDirection) -> bool {
        let start = self.line_start(self.cursor);
        let end = self.line_end(self.cursor);
        let column = match self.desired_column {
            Some(DesiredColumn::Logical(column)) => column,
            _ => columns(&self.text[start..self.cursor]),
        };
        self.desired_column = Some(DesiredColumn::Logical(column));
        let target = match direction {
            VerticalDirection::Up if start > 0 => {
                let previous_end = start - 1;
                self.line_start(previous_end)..previous_end
            }
            VerticalDirection::Down if end < self.text.len() => {
                let next_start = end + 1;
                next_start..self.line_end(next_start)
            }
            _ => return false,
        };
        let cursor = target.start + cursor_for_column(&self.text[target], column);
        let changed = self.cursor != cursor;
        self.cursor = cursor;
        changed
    }

    fn line_start(&self, cursor: usize) -> usize {
        self.text[..cursor].rfind('\n').map_or(0, |index| index + 1)
    }

    fn line_end(&self, cursor: usize) -> usize {
        self.text[cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| cursor + index)
    }
}

#[derive(Debug)]
struct Snapshot {
    text: Box<str>,
    cursor: usize,
}

impl Snapshot {
    fn new(text: &str, cursor: usize) -> Self {
        Self {
            text: text.into(),
            cursor,
        }
    }
}

#[derive(Debug)]
struct Change {
    before: Snapshot,
    after: Snapshot,
    bytes: usize,
}

/// Direction of a visual-row movement in [`Composer::move_visual`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalDirection {
    /// Move toward the first visual row.
    Up,
    /// Move toward the last visual row.
    Down,
}

#[derive(Debug, Clone, Copy)]
enum DesiredColumn {
    Logical(usize),
    Visual { width: u16, column: usize },
}

fn previous_boundary(text: &str, cursor: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|index| *index < cursor)
        .last()
        .unwrap_or(0)
}

fn next_boundary(text: &str, cursor: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|index| *index > cursor)
        .unwrap_or(text.len())
}

fn boundary_at_or_before(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|index| *index <= cursor)
        .last()
        .unwrap_or(0)
}

fn boundary_at_or_after(text: &str, cursor: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|index| *index >= cursor)
        .unwrap_or(text.len())
}

fn grapheme_width(grapheme: &str, column: usize) -> usize {
    if grapheme == "\t" {
        TAB_COLUMNS - column % TAB_COLUMNS
    } else {
        UnicodeWidthStr::width(grapheme)
    }
}

fn cursor_for_column(line: &str, desired_column: usize) -> usize {
    let mut column: usize = 0;
    let mut cursor = 0;
    for (index, grapheme) in line.grapheme_indices(true) {
        let next_column = column.saturating_add(grapheme_width(grapheme, column));
        if next_column > desired_column {
            break;
        }
        column = next_column;
        cursor = index + grapheme.len();
    }
    cursor
}

#[cfg(test)]
mod tests {
    use super::{Composer, Edit, EditError, VerticalDirection};
    use unicode_segmentation::UnicodeSegmentation;

    #[test]
    fn empty_draft_ignores_navigation_deletion_and_history() {
        let mut composer = Composer::new(0);
        for edit in [
            Edit::Left,
            Edit::Right,
            Edit::Home,
            Edit::End,
            Edit::Up,
            Edit::Down,
            Edit::Backspace,
            Edit::Delete,
            Edit::Undo,
            Edit::Redo,
            Edit::Insert(String::new()),
        ] {
            assert_eq!(composer.update(edit), Ok(false));
        }
        assert_eq!(
            (composer.text(), composer.cursor(), composer.revision()),
            ("", 0, 0)
        );
    }

    #[test]
    fn a_multiline_paste_is_one_undo_transaction() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("first".into())).unwrap();
        composer
            .update(Edit::Insert("\r\nsecond\t行\r\n".into()))
            .unwrap();
        assert_eq!(composer.text(), "first\nsecond\t行\n");
        composer.update(Edit::Undo).unwrap();
        assert_eq!((composer.text(), composer.cursor()), ("first", 5));
        composer.update(Edit::Redo).unwrap();
        assert_eq!(composer.text(), "first\nsecond\t行\n");
    }

    #[test]
    fn unsupported_controls_reject_the_entire_paste_and_preserve_redo() {
        for character in ['\0', '\u{7}', '\r', '\u{1b}', '\u{7f}', '\u{85}', '\u{9b}'] {
            let mut composer = Composer::new(100);
            composer.update(Edit::Insert("draft".into())).unwrap();
            composer.update(Edit::Insert("!".into())).unwrap();
            composer.update(Edit::Undo).unwrap();
            let revision = composer.revision();
            assert_eq!(
                composer.update(Edit::Insert(format!("prefix{character}suffix"))),
                Err(EditError::InvalidControl { character })
            );
            assert_eq!(
                (composer.text(), composer.cursor(), composer.revision()),
                ("draft", 5, revision)
            );
            composer.update(Edit::Redo).unwrap();
            assert_eq!(composer.text(), "draft!");
        }
    }

    #[test]
    fn byte_limit_counts_normalized_utf8_and_never_truncates() {
        let mut composer = Composer::new(4);
        composer.update(Edit::Insert("界\r\n".into())).unwrap();
        assert_eq!(composer.text(), "界\n");
        assert_eq!(
            composer.update(Edit::Insert("é".into())),
            Err(EditError::TooLarge { max_bytes: 4 })
        );
        assert_eq!(
            (composer.text(), composer.cursor(), composer.revision()),
            ("界\n", 4, 1)
        );
        composer.update(Edit::Undo).unwrap();
        assert_eq!(composer.text(), "");
    }

    #[test]
    fn movement_and_backspace_keep_combining_marks_and_zwj_emoji_intact() {
        let mut composer = Composer::new(100);
        composer
            .update(Edit::Insert("e\u{301}👩‍👩‍👧‍👦界".into()))
            .unwrap();
        composer.update(Edit::Left).unwrap();
        assert_eq!(&composer.text()[composer.cursor()..], "界");
        composer.update(Edit::Backspace).unwrap();
        assert_eq!((composer.text(), composer.cursor()), ("e\u{301}界", 3));
        composer.update(Edit::Backspace).unwrap();
        assert_eq!((composer.text(), composer.cursor()), ("界", 0));
    }

    #[test]
    fn delete_removes_an_entire_flag_without_splitting_regional_indicators() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("🇺🇸🇨🇦".into())).unwrap();
        composer.update(Edit::Home).unwrap();
        composer.update(Edit::Right).unwrap();
        assert_eq!(composer.cursor(), "🇺🇸".len());
        composer.update(Edit::Delete).unwrap();
        assert_eq!((composer.text(), composer.cursor()), ("🇺🇸", "🇺🇸".len()));
    }

    #[test]
    fn inserting_a_joiner_snaps_to_the_end_of_the_merged_emoji() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("👩👧".into())).unwrap();
        composer.update(Edit::Left).unwrap();
        composer.update(Edit::Insert("\u{200d}".into())).unwrap();
        assert_eq!((composer.text(), composer.cursor()), ("👩‍👧", "👩‍👧".len()));
        composer.update(Edit::Backspace).unwrap();
        assert_eq!((composer.text(), composer.cursor()), ("", 0));
    }

    #[test]
    fn inserting_before_a_combining_mark_preserves_a_grapheme_boundary() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("\u{301}".into())).unwrap();
        composer.update(Edit::Home).unwrap();
        composer.update(Edit::Insert("e".into())).unwrap();
        assert_eq!((composer.text(), composer.cursor()), ("e\u{301}", 3));
    }

    #[test]
    fn deleting_a_newline_that_joins_graphemes_snaps_backward() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("e\n\u{301}".into())).unwrap();
        composer.update(Edit::Home).unwrap();
        composer.update(Edit::Backspace).unwrap();
        assert_eq!((composer.text(), composer.cursor()), ("e\u{301}", 0));
    }

    #[test]
    fn undo_and_redo_restore_transaction_cursors_and_advance_revision() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("ab".into())).unwrap();
        composer.update(Edit::Left).unwrap();
        composer.update(Edit::Insert("X".into())).unwrap();
        composer.update(Edit::Left).unwrap();
        composer.update(Edit::Undo).unwrap();
        assert_eq!(
            (composer.text(), composer.cursor(), composer.revision()),
            ("ab", 1, 3)
        );
        composer.update(Edit::Redo).unwrap();
        assert_eq!(
            (composer.text(), composer.cursor(), composer.revision()),
            ("aXb", 2, 4)
        );
    }

    #[test]
    fn a_new_edit_after_undo_discards_only_the_redo_branch() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("a".into())).unwrap();
        composer.update(Edit::Insert("b".into())).unwrap();
        composer.update(Edit::Undo).unwrap();
        composer.update(Edit::Insert("c".into())).unwrap();
        assert_eq!(composer.update(Edit::Redo), Ok(false));
        composer.update(Edit::Undo).unwrap();
        assert_eq!(composer.text(), "a");
        composer.update(Edit::Undo).unwrap();
        assert_eq!(composer.text(), "");
    }

    #[test]
    fn an_empty_insert_and_cursor_movement_preserve_redo() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("a".into())).unwrap();
        composer.update(Edit::Insert("b".into())).unwrap();
        composer.update(Edit::Undo).unwrap();
        composer.update(Edit::Insert(String::new())).unwrap();
        composer.update(Edit::Left).unwrap();
        assert_eq!(composer.revision(), 3);
        composer.update(Edit::Redo).unwrap();
        assert_eq!((composer.text(), composer.cursor()), ("ab", 2));
    }

    #[test]
    fn home_end_and_vertical_navigation_handle_empty_and_trailing_lines() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("abc\n\n界\n".into())).unwrap();
        assert_eq!(composer.line_column(), (3, 0));
        composer.update(Edit::Up).unwrap();
        composer.update(Edit::End).unwrap();
        assert_eq!(composer.line_column(), (2, 2));
        composer.update(Edit::Up).unwrap();
        assert_eq!(composer.line_column(), (1, 0));
        composer.update(Edit::Up).unwrap();
        assert_eq!(composer.line_column(), (0, 2));
        composer.update(Edit::Home).unwrap();
        assert_eq!(composer.cursor(), 0);
    }

    #[test]
    fn vertical_navigation_preserves_the_goal_across_a_short_line() {
        let mut composer = Composer::new(100);
        composer
            .update(Edit::Insert("abcdef\nx\nabcdef".into()))
            .unwrap();
        composer.update(Edit::Up).unwrap();
        assert_eq!(composer.line_column(), (1, 1));
        composer.update(Edit::Up).unwrap();
        assert_eq!(composer.line_column(), (0, 6));
        composer.update(Edit::Down).unwrap();
        composer.update(Edit::Down).unwrap();
        assert_eq!(composer.line_column(), (2, 6));
    }

    #[test]
    fn vertical_navigation_uses_display_columns_and_leading_wide_edges() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("界x\nabcd".into())).unwrap();
        composer.update(Edit::Home).unwrap();
        composer.update(Edit::Right).unwrap();
        composer.update(Edit::Up).unwrap();
        assert_eq!(composer.line_column(), (0, 0));
        composer.update(Edit::Down).unwrap();
        assert_eq!(composer.line_column(), (1, 1));
    }

    #[test]
    fn line_columns_include_combining_marks_emoji_and_tab_stops() {
        let mut composer = Composer::new(100);
        composer
            .update(Edit::Insert("界e\u{301}\t🙂".into()))
            .unwrap();
        assert_eq!(composer.line_column(), (0, 6));
    }

    #[test]
    fn horizontal_navigation_resets_the_vertical_goal() {
        let mut composer = Composer::new(100);
        composer
            .update(Edit::Insert("abcdef\nx\nabcdef".into()))
            .unwrap();
        composer.update(Edit::Up).unwrap();
        composer.update(Edit::Left).unwrap();
        composer.update(Edit::Up).unwrap();
        assert_eq!(composer.line_column(), (0, 0));
    }

    #[test]
    fn history_retains_at_most_128_transactions() {
        let mut composer = Composer::new(200);
        for _ in 0..130 {
            composer.update(Edit::Insert("a".into())).unwrap();
        }
        let mut count = 0;
        while composer.update(Edit::Undo).unwrap() {
            count += 1;
        }
        assert_eq!((count, composer.text()), (128, "aa"));
    }

    #[test]
    fn history_evicts_whole_transactions_to_meet_the_byte_budget() {
        let mut composer = Composer::new(2 * 1024 * 1024);
        composer
            .update(Edit::Insert("a".repeat(1024 * 1024)))
            .unwrap();
        composer.update(Edit::Insert("b".into())).unwrap();
        composer.update(Edit::Insert("c".into())).unwrap();
        composer.update(Edit::Undo).unwrap();
        assert_eq!(composer.update(Edit::Undo), Ok(false));
        assert_eq!(composer.text().len(), 1024 * 1024 + 1);
        composer.update(Edit::Redo).unwrap();
        assert!(composer.text().ends_with("bc"));
    }

    #[test]
    fn a_transaction_larger_than_the_history_budget_severs_old_history() {
        let mut composer = Composer::new(3 * 1024 * 1024);
        composer
            .update(Edit::Insert("a".repeat(2 * 1024 * 1024)))
            .unwrap();
        composer.update(Edit::Insert("b".into())).unwrap();
        assert_eq!(composer.update(Edit::Undo), Ok(false));
        assert_eq!(composer.text().len(), 2 * 1024 * 1024 + 1);
    }

    #[test]
    fn mixed_edits_never_leave_the_cursor_inside_a_grapheme() {
        let mut composer = Composer::new(1000);
        let edits = [
            Edit::Insert("🇦🇧🇨👩👧\ne\u{301}\t界\n".into()),
            Edit::Up,
            Edit::Home,
            Edit::Backspace,
            Edit::Left,
            Edit::Insert("\u{200d}".into()),
            Edit::Right,
            Edit::Delete,
            Edit::Home,
            Edit::Right,
            Edit::Insert("🇩".into()),
            Edit::Backspace,
            Edit::Down,
            Edit::End,
            Edit::Insert("\u{301}".into()),
            Edit::Undo,
            Edit::Undo,
            Edit::Redo,
            Edit::Up,
        ];
        for edit in edits {
            composer.update(edit).unwrap();
            assert!(
                composer.cursor() == composer.text().len()
                    || composer
                        .text()
                        .grapheme_indices(true)
                        .any(|(index, _)| index == composer.cursor()),
                "invalid cursor {} in {:?}",
                composer.cursor(),
                composer.text()
            );
        }
    }

    #[test]
    fn visual_navigation_moves_through_soft_rows_and_preserves_the_column() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("abcdefghij".into())).unwrap();
        assert!(composer.move_visual(VerticalDirection::Up, 4));
        assert_eq!(composer.cursor(), 6);
        assert!(composer.move_visual(VerticalDirection::Up, 4));
        assert_eq!(composer.cursor(), 2);
        assert!(!composer.move_visual(VerticalDirection::Up, 4));
        composer.move_visual(VerticalDirection::Down, 4);
        composer.move_visual(VerticalDirection::Down, 4);
        assert_eq!((composer.cursor(), composer.revision()), (10, 1));
        assert!(!composer.move_visual(VerticalDirection::Down, 4));
        composer.update(Edit::Undo).unwrap();
        assert_eq!(composer.text(), "");
    }

    #[test]
    fn edit_up_keeps_its_original_logical_line_semantics() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("abcdefghij".into())).unwrap();
        assert_eq!(composer.update(Edit::Up), Ok(false));
        assert_eq!(composer.cursor(), 10);
    }

    #[test]
    fn full_row_ends_and_soft_boundaries_are_reachable_visual_positions() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("abcdefgh".into())).unwrap();
        composer.move_visual(VerticalDirection::Up, 4);
        assert_eq!(composer.cursor(), 4);
        composer.move_visual(VerticalDirection::Up, 4);
        assert_eq!(composer.cursor(), 0);
        composer.move_visual(VerticalDirection::Down, 4);
        composer.move_visual(VerticalDirection::Down, 4);
        assert_eq!(composer.cursor(), 8);
    }

    #[test]
    fn visual_navigation_keeps_cjk_combining_marks_and_emoji_whole() {
        let mut composer = Composer::new(100);
        composer
            .update(Edit::Insert("界e\u{301}👩‍👧x\nabcd".into()))
            .unwrap();
        composer.update(Edit::Home).unwrap();
        composer.update(Edit::Right).unwrap();
        composer.move_visual(VerticalDirection::Up, 4);
        assert_eq!(composer.cursor(), "界e\u{301}".len());
        composer.move_visual(VerticalDirection::Up, 4);
        assert_eq!(composer.cursor(), 0);
        composer.move_visual(VerticalDirection::Down, 4);
        composer.update(Edit::Right).unwrap();
        composer.move_visual(VerticalDirection::Down, 4);
        assert_eq!(&composer.text()[..composer.cursor()], "界e\u{301}👩‍👧x\nab");
    }

    #[test]
    fn visual_navigation_preserves_its_goal_across_tabs_and_empty_insertion_rows() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("a\tb\nxyz".into())).unwrap();
        composer.move_visual(VerticalDirection::Up, 5);
        assert_eq!(composer.cursor(), 3);
        composer.move_visual(VerticalDirection::Up, 5);
        assert_eq!(composer.cursor(), 1);
        composer.move_visual(VerticalDirection::Down, 5);
        composer.move_visual(VerticalDirection::Down, 5);
        assert_eq!(composer.cursor(), composer.text().len());
    }

    #[test]
    fn visual_navigation_resets_its_goal_after_a_width_change() {
        let mut composer = Composer::new(100);
        composer.update(Edit::Insert("abcdefghij".into())).unwrap();
        composer.move_visual(VerticalDirection::Up, 4);
        assert_eq!(composer.cursor(), 6);
        composer.move_visual(VerticalDirection::Up, 3);
        assert_eq!(composer.cursor(), 3);
    }

    #[test]
    fn visual_navigation_does_not_reuse_a_logical_column_goal() {
        let mut composer = Composer::new(100);
        composer
            .update(Edit::Insert("abcde\nfghijkl".into()))
            .unwrap();
        composer.update(Edit::Up).unwrap();
        assert_eq!(composer.cursor(), 5);
        composer.move_visual(VerticalDirection::Down, 4);
        assert_eq!(composer.cursor(), 7);
    }

    #[test]
    fn zero_width_visual_navigation_uses_safe_one_column_grapheme_rows() {
        let mut composer = Composer::new(100);
        assert!(!composer.move_visual(VerticalDirection::Up, 0));
        assert!(!composer.move_visual(VerticalDirection::Down, 0));
        composer.update(Edit::Insert("界👩‍👧".into())).unwrap();
        composer.move_visual(VerticalDirection::Up, 0);
        assert_eq!(composer.cursor(), "界".len());
        composer.move_visual(VerticalDirection::Up, 0);
        assert_eq!(composer.cursor(), 0);
        assert!(!composer.move_visual(VerticalDirection::Up, 0));
        composer.move_visual(VerticalDirection::Down, 0);
        composer.move_visual(VerticalDirection::Down, 0);
        assert_eq!((composer.text(), composer.cursor()), ("界👩‍👧", "界👩‍👧".len()));
    }
}
