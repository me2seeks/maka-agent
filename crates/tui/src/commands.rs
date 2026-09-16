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

//! Local commands only; this is not a plugin execution registry.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListState, Paragraph},
};

use crate::{
    composer::{Composer, Edit},
    i18n,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Requests,
    Help,
    Latest,
    Thinking,
    Preview,
    LiteralSlash,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    Slash,
    Shortcut,
}

const COMMANDS: &[(Command, &str, &str)] = &[
    (Command::Help, i18n::HELP_LABEL, "帮助 快捷键 keyboard help"),
    (Command::Latest, i18n::LATEST_LABEL, "最新 latest"),
    (
        Command::Thinking,
        i18n::THINKING_LABEL,
        "思考 推理 thinking reasoning",
    ),
    (Command::Preview, i18n::PREVIEW_LABEL, "预览 preview echo"),
    (
        Command::Requests,
        "待处理请求",
        "权限 授权 问题 表单 requests permission question form",
    ),
];

pub(crate) struct Commands {
    entry: Entry,
    query: Composer,
    state: ListState,
    keyboard_ready: bool,
    can_edit: bool,
    rows: Rect,
    query_error: Option<&'static str>,
}

impl Commands {
    pub(crate) fn new(area: Rect, entry: Entry) -> Self {
        let available = area.width >= 12 && area.height >= 4;
        Self {
            entry,
            query: Composer::new(128),
            state: ListState::default().with_selected(Some(0)),
            keyboard_ready: available,
            can_edit: available,
            rows: Rect::default(),
            query_error: None,
        }
    }

    pub(crate) fn resize(&mut self, width: u16, height: u16) {
        self.keyboard_ready = false;
        self.can_edit = width >= 12 && height >= 4;
        self.rows = Rect::default();
    }

    fn matches(&self) -> Vec<(Command, &'static str)> {
        let query = self.query.text().to_lowercase();
        COMMANDS
            .iter()
            .filter(|(_, label, aliases)| {
                label.to_lowercase().contains(&query) || aliases.contains(&query)
            })
            .map(|&(command, label, _)| (command, label))
            .collect()
    }

    pub(crate) fn key(&mut self, key: KeyEvent) -> Option<Command> {
        if key.kind == KeyEventKind::Release
            || !self.can_edit
            || key.modifiers.intersects(
                KeyModifiers::CONTROL
                    | KeyModifiers::ALT
                    | KeyModifiers::SUPER
                    | KeyModifiers::META
                    | KeyModifiers::HYPER,
            )
        {
            return None;
        }
        if key.code == KeyCode::Enter {
            return if key.kind == KeyEventKind::Press && self.keyboard_ready {
                self.state
                    .selected()
                    .and_then(|index| self.matches().get(index).map(|&(command, _)| command))
            } else {
                None
            };
        }
        if key.code == KeyCode::Char('/')
            && ((self.query.text().is_empty() && self.entry == Entry::Slash)
                || self.query.text() == "/")
        {
            return (key.kind == KeyEventKind::Press).then_some(Command::LiteralSlash);
        }
        match key.code {
            KeyCode::Up => self.scroll(false),
            KeyCode::Down => self.scroll(true),
            KeyCode::Char(ch) if !ch.is_control() => self.edit(Edit::Insert(ch.to_string())),
            KeyCode::Backspace => self.edit(Edit::Backspace),
            KeyCode::Delete => self.edit(Edit::Delete),
            KeyCode::Left => self.edit(Edit::Left),
            KeyCode::Right => self.edit(Edit::Right),
            KeyCode::Home => self.edit(Edit::Home),
            KeyCode::End => self.edit(Edit::End),
            _ => return None,
        }
        None
    }

    fn edit(&mut self, edit: Edit) {
        self.query_error = self.query.update(edit).err().map(|error| match error {
            crate::composer::EditError::TooLarge { .. } => i18n::COMMAND_LIMIT,
            _ => i18n::COMMAND_INVALID,
        });
        self.state.select(Some(0));
        // Keyboard confirmation uses current search intent. Mouse clicks require
        // the new list's actual painted geometry, never the previous filter.
        self.rows = Rect::default();
    }

    pub(crate) fn paste(&mut self, value: &str) {
        if self.can_edit {
            self.edit(Edit::Insert(
                value.replace("\r\n", " ").replace(['\n', '\t'], " "),
            ));
        }
    }

    pub(crate) fn scroll(&mut self, down: bool) {
        if !self.can_edit {
            return;
        }
        self.state
            .select(self.matches().len().checked_sub(1).map(|last| {
                let selected = self.state.selected().unwrap_or(0);
                if down {
                    selected.saturating_add(1).min(last)
                } else {
                    selected.saturating_sub(1).min(last)
                }
            }));
        self.rows = Rect::default();
    }

    pub(crate) fn click(&self, x: u16, y: u16) -> Option<Command> {
        if !self.rows.contains((x, y).into()) {
            return None;
        }
        self.matches()
            .get(self.state.offset() + usize::from(y - self.rows.y))
            .map(|&(command, _)| command)
    }

    pub(crate) fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        self.resize(area.width, area.height);
        if !self.can_edit {
            frame.render_widget(Clear, area);
            frame.render_widget(Paragraph::new(i18n::COMMAND_RESIZE), area);
            return;
        }
        let width = area.width.min(56);
        let height = area.height.min(10);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .title(i18n::COMMAND_TITLE);
        let compact = area.width < 24 || area.height < 7;
        let inner = if compact { popup } else { block.inner(popup) };
        frame.render_widget(Clear, popup);
        if !compact {
            frame.render_widget(block, popup);
        }
        let (query_line, cursor_column, clipped) = query_window(
            self.query.text(),
            self.query.cursor(),
            usize::from(inner.width - 2),
        );
        frame.render_widget(
            Paragraph::new(format!("{} {query_line}", if clipped { "…" } else { "/" })),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        frame.set_cursor_position((
            inner.x + 2 + u16::try_from(cursor_column).unwrap_or(0),
            inner.y,
        ));
        self.rows = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(3),
        );
        let matches = self.matches();
        if matches.is_empty() {
            self.state.select(None);
            frame.render_widget(Paragraph::new(i18n::COMMAND_EMPTY), self.rows);
        } else {
            let list = List::new(matches.iter().map(|&(_, label)| label))
                .highlight_symbol("> ")
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            frame.render_stateful_widget(list, self.rows, &mut self.state);
        }
        self.keyboard_ready = true;
        let hint = i18n::command_hint(
            self.state.selected().map_or(0, |index| index + 1),
            matches.len(),
            compact,
        );
        frame.render_widget(
            Paragraph::new(self.query_error.unwrap_or(&hint)),
            Rect::new(inner.x, inner.bottom() - 2, inner.width, 2),
        );
    }
}

fn query_window(value: &str, cursor: usize, width: usize) -> (&str, usize, bool) {
    let mut start = cursor;
    let mut cursor_column = 0;
    for (index, grapheme) in value[..cursor].grapheme_indices(true).rev() {
        let cells = grapheme.width();
        if cursor_column + cells > width.saturating_sub(1) {
            break;
        }
        start = index;
        cursor_column += cells;
    }
    let mut end = start;
    let mut cells = 0;
    for grapheme in value[start..].graphemes(true) {
        cells += grapheme.width();
        if cells > width {
            break;
        }
        end += grapheme.len();
    }
    (&value[start..end], cursor_column, start > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_width_ascii_query_keeps_text_and_cursor_visible() {
        assert_eq!(
            query_window("abcdefghijklmnopqrst", 20, 20),
            ("bcdefghijklmnopqrst", 19, true)
        );
    }

    #[test]
    fn exact_width_chinese_query_does_not_turn_into_empty_insertion_row() {
        let query = "一二三四五六七八九十";
        assert_eq!(
            query_window(query, query.len(), 20),
            ("二三四五六七八九十", 18, true)
        );
    }

    #[test]
    fn query_window_keeps_grapheme_clusters_and_cursor_in_middle() {
        let query = "e\u{301}中文";
        assert_eq!(query_window(query, "e\u{301}".len(), 5), (query, 1, false));
    }
}
