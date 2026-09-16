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

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone)]
pub(crate) struct WrappedLine {
    pub start: usize,
    pub end: usize,
    pub text: String,
    cursor_stops: Vec<CursorStop>,
}

impl WrappedLine {
    fn empty(start: usize) -> Self {
        Self {
            start,
            end: start,
            text: String::new(),
            cursor_stops: vec![CursorStop {
                offset: start,
                column: 0,
            }],
        }
    }

    fn columns(&self) -> usize {
        self.cursor_stops.last().map_or(0, |stop| stop.column)
    }
}

#[derive(Debug, Clone, Copy)]
struct CursorStop {
    offset: usize,
    column: usize,
}

/// The same visual rows and insertion coordinates used for rendering and motion.
#[derive(Debug)]
pub(crate) struct ComposerLayout {
    pub lines: Vec<WrappedLine>,
    pub cursor_row: usize,
    pub cursor_column: u16,
}

impl ComposerLayout {
    /// Select the last grapheme boundary at or before a desired screen column.
    /// Shared soft-wrap boundaries belong to the following row.
    pub(crate) fn cursor_on_row(&self, row: usize, column: usize) -> Option<usize> {
        let line = self.lines.get(row)?;
        let following = self.lines.get(row.saturating_add(1));
        Some(
            line.cursor_stops
                .iter()
                .take_while(|stop| stop.column <= column)
                .filter(|stop| following.is_none_or(|next| stop.offset < next.start))
                .last()
                .map_or(line.start, |stop| stop.offset),
        )
    }
}

// Source byte offsets survive wrapping; rendered terminal columns do not.
pub(crate) fn wrap(text: &str, width: u16) -> Vec<WrappedLine> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut current = WrappedLine::empty(0);
    let mut columns = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if grapheme == "\n" {
            current.end = offset;
            lines.push(current);
            current = WrappedLine::empty(offset + 1);
            columns = 0;
            continue;
        }
        let cells = if grapheme == "\t" {
            4 - columns % 4
        } else {
            grapheme.width()
        };
        if columns > 0 && (columns >= width || columns + cells > width) {
            current.end = offset;
            lines.push(current);
            current = WrappedLine::empty(offset);
            columns = 0;
        }
        let cells = if grapheme == "\t" {
            (4 - columns % 4).min(width)
        } else {
            grapheme.width()
        };
        if grapheme == "\t" {
            current.text.push_str(&" ".repeat(cells));
        } else if cells > width {
            // A one-column viewport cannot display a two-column grapheme.
            // The source remains intact and will reappear after resizing.
            current.text.push('�');
        } else {
            current.text.push_str(grapheme);
        }
        columns += cells.min(width);
        current.end = offset + grapheme.len();
        current.cursor_stops.push(CursorStop {
            offset: current.end,
            column: columns,
        });
    }
    lines.push(current);
    lines
}

/// Wrap a draft while reserving a visible insertion position at full hard-line
/// ends. Such an end gets an empty visual row before its LF or the end of text;
/// no newline is inserted into the draft. A soft-wrap boundary belongs to the
/// next row instead. Width zero uses the same one-column fallback as `wrap`.
pub(crate) fn composer_layout(text: &str, cursor: usize, width: u16) -> ComposerLayout {
    let width = width.max(1);
    let mut wrapped = wrap(text, width).into_iter().peekable();
    let mut lines = Vec::new();
    while let Some(line) = wrapped.next() {
        let needs_insertion_row = line.columns() == usize::from(width)
            && wrapped.peek().is_none_or(|next| next.start > line.end);
        let end = line.end;
        lines.push(line);
        if needs_insertion_row {
            lines.push(WrappedLine::empty(end));
        }
    }
    let cursor = cursor.min(text.len());
    let cursor_row = lines
        .iter()
        .rposition(|line| line.start <= cursor)
        .unwrap_or(0);
    let cursor_column = lines
        .get(cursor_row)
        .and_then(|line| {
            line.cursor_stops
                .iter()
                .take_while(|stop| stop.offset <= cursor)
                .last()
        })
        .map_or(0, |stop| stop.column);
    ComposerLayout {
        lines,
        cursor_row,
        cursor_column: u16::try_from(cursor_column).unwrap_or(width - 1),
    }
}

pub(crate) fn columns(text: &str) -> usize {
    text.graphemes(true).fold(0_usize, |column, grapheme| {
        column.saturating_add(if grapheme == "\t" {
            4 - column % 4
        } else {
            grapheme.width()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_keeps_combining_and_wide_graphemes_whole() {
        let lines = wrap("e\u{301}中文👩‍👩‍👧‍👦", 4);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["e\u{301}中", "文👩‍👩‍👧‍👦"]
        );
    }

    #[test]
    fn blank_and_final_lines_preserve_source_offsets() {
        let lines = wrap("a\n\nb\n", 8);
        assert_eq!(
            lines
                .iter()
                .map(|line| (line.start, line.end, line.text.as_str()))
                .collect::<Vec<_>>(),
            [(0, 1, "a"), (2, 2, ""), (3, 4, "b"), (5, 5, "")]
        );
    }

    #[test]
    fn a_full_final_row_has_a_visible_insertion_row() {
        let layout = composer_layout("abcd", 4, 4);
        assert_eq!(
            layout
                .lines
                .iter()
                .map(|line| (line.start, line.end, line.text.as_str()))
                .collect::<Vec<_>>(),
            [(0, 4, "abcd"), (4, 4, "")]
        );
        assert_eq!((layout.cursor_row, layout.cursor_column), (1, 0));
    }

    #[test]
    fn a_soft_wrap_boundary_belongs_to_the_following_row() {
        let layout = composer_layout("abcdef", 4, 4);
        assert_eq!(
            (layout.lines.len(), layout.cursor_row, layout.cursor_column),
            (2, 1, 0)
        );
        assert_eq!(layout.cursor_on_row(0, usize::MAX), Some(3));
    }

    #[test]
    fn a_full_hard_line_has_distinct_positions_before_and_after_its_lf() {
        let before = composer_layout("abcd\nx", 4, 4);
        let after = composer_layout("abcd\nx", 5, 4);
        assert_eq!(
            before
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["abcd", "", "x"]
        );
        assert_eq!((before.cursor_row, before.cursor_column), (1, 0));
        assert_eq!((after.cursor_row, after.cursor_column), (2, 0));
    }

    #[test]
    fn visual_columns_map_back_to_complete_cjk_and_combining_graphemes() {
        let layout = composer_layout("e\u{301}界👩‍👧", "e\u{301}".len(), 4);
        assert_eq!((layout.cursor_row, layout.cursor_column), (0, 1));
        assert_eq!(layout.cursor_on_row(0, 2), Some("e\u{301}".len()));
        assert_eq!(layout.cursor_on_row(1, 1), Some("e\u{301}界".len()));
    }

    #[test]
    fn narrow_tabs_use_the_rendered_cell_count_for_cursor_mapping() {
        let layout = composer_layout("\tX", 1, 2);
        assert_eq!(
            layout
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["  ", "X"]
        );
        assert_eq!((layout.cursor_row, layout.cursor_column), (1, 0));
        assert_eq!(layout.cursor_on_row(0, 1), Some(0));
        assert_eq!(layout.cursor_on_row(1, 1), Some(2));
    }

    #[test]
    fn one_column_replacements_keep_original_source_offsets() {
        let layout = composer_layout("界👩‍👧", "界👩‍👧".len(), 1);
        assert_eq!(
            layout
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["�", "�", ""]
        );
        assert_eq!((layout.cursor_row, layout.cursor_column), (2, 0));
        assert_eq!(layout.cursor_on_row(1, 0), Some("界".len()));
    }

    #[test]
    fn every_grapheme_boundary_has_a_visible_cursor_at_every_narrow_width() {
        let source = "ab界e\u{301}\t👩‍👧\n🇺🇸\nxy\u{200b}";
        for width in 0..=8 {
            for cursor in source
                .grapheme_indices(true)
                .map(|(offset, _)| offset)
                .chain(std::iter::once(source.len()))
            {
                let layout = composer_layout(source, cursor, width);
                assert!(layout.cursor_row < layout.lines.len());
                assert!(
                    layout.cursor_column < width.max(1),
                    "cursor {cursor} is off-screen at width {width}: {layout:?}"
                );
                let selected = layout
                    .cursor_on_row(layout.cursor_row, usize::from(layout.cursor_column))
                    .unwrap();
                let selected_layout = composer_layout(source, selected, width);
                assert_eq!(
                    (selected_layout.cursor_row, selected_layout.cursor_column),
                    (layout.cursor_row, layout.cursor_column)
                );
            }
        }
    }

    #[test]
    fn empty_and_nonfull_trailing_lines_need_no_extra_insertion_row() {
        for source in ["", "abc", "a\n", "\n"] {
            let layout = composer_layout(source, source.len(), 4);
            assert_eq!(layout.lines.len(), wrap(source, 4).len());
        }
    }
}
