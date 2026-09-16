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

//! One declarative request editor. It owns input focus, never Host authority.

use std::collections::{BTreeMap, BTreeSet};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};
use serde_json::Value;

use crate::{
    composer::{Composer, Edit},
    host_ui::{HostCommand, HostInteraction, InteractionAction},
    text,
};

pub(crate) struct InteractionPanel {
    pub(crate) request: HostInteraction,
    editors: Vec<Composer>,
    selected: Vec<BTreeSet<usize>>,
    field: usize,
    choice: usize,
    action: usize,
    scroll: usize,
    max_scroll: usize,
    visible: bool,
    reviewed: bool,
    buttons: Vec<Rect>,
    error: String,
    active: bool,
    pending: bool,
    follow_input: bool,
}

fn safe(text: &str, limit: usize) -> bool {
    text.len() <= limit && !text.chars().any(|ch| {
        (ch.is_control() && ch != '\n' && ch != '\t')
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    })
}

pub(crate) fn valid_requests(requests: &[HostInteraction]) -> bool {
    let mut ids = BTreeSet::new();
    requests.len() <= 16
        && requests.iter().all(|request| {
            let mut fields = BTreeSet::new();
            !request.id.is_empty()
                && safe(&request.id, 256)
                && ids.insert(&request.id)
                && matches!(
                    request.kind.as_str(),
                    "sandbox_boundary" | "client_capability" | "question" | "form" | "unsupported"
                )
                && safe(&request.title, 1024)
                && safe(&request.source, 1024)
                && safe(&request.detail, 24 * 1024)
                && request.fields.len() <= 32
                && request.fields.iter().all(|field| {
                    !field.name.is_empty()
                        && safe(&field.name, 256)
                        && fields.insert(&field.name)
                        && safe(&field.label, 1024)
                        && safe(&field.description, 2048)
                        && matches!(
                            field.kind.as_str(),
                            "string"
                                | "number"
                                | "integer"
                                | "boolean"
                                | "single_select"
                                | "multi_select"
                                | "question"
                        )
                        && field.options.len() <= 64
                        && field
                            .options
                            .iter()
                            .all(|option| safe(&option.value, 2048) && safe(&option.label, 1024))
                        && safe(&field.default.to_string(), 8192)
                })
        })
}

impl InteractionPanel {
    pub(crate) fn new(request: HostInteraction) -> Self {
        let mut editors = Vec::new();
        let mut selected = Vec::new();
        for field in &request.fields {
            let mut editor = Composer::new(2048);
            if let Some(value) = field.default.as_str() {
                let _ = editor.update(Edit::Insert(value.into()));
            } else if field.default.is_number() {
                let _ = editor.update(Edit::Insert(field.default.to_string()));
            }
            let mut values = BTreeSet::new();
            for (index, option) in field.options.iter().enumerate() {
                if field.default.as_str() == Some(&option.value)
                    || field.default.as_array().is_some_and(|array| {
                        array
                            .iter()
                            .any(|value| value.as_str() == Some(&option.value))
                    })
                {
                    values.insert(index);
                }
            }
            if let Some(value) = field.default.as_bool() {
                values.insert(usize::from(!value));
            }
            editors.push(editor);
            selected.push(values);
        }
        Self {
            request,
            editors,
            selected,
            field: 0,
            choice: 0,
            action: 0,
            scroll: 0,
            max_scroll: 0,
            visible: false,
            reviewed: false,
            buttons: Vec::new(),
            error: String::new(),
            active: true,
            pending: false,
            follow_input: false,
        }
    }

    pub(crate) fn expire(&mut self) {
        self.active = false;
        self.invalidate();
    }

    pub(crate) fn set_pending(&mut self, pending: bool) {
        self.pending = pending;
        self.invalidate();
    }

    pub(crate) fn reject(&mut self) {
        self.set_pending(false);
        self.error = "Host 未接受回答；检查内容或请求是否已结束，未自动重试".into();
    }

    pub(crate) fn invalidate(&mut self) {
        self.visible = false;
        self.reviewed = false;
        self.buttons.clear();
    }

    fn summary(&self) -> bool {
        self.field >= self.request.fields.len()
    }

    fn authorization(&self) -> bool {
        matches!(
            self.request.kind.as_str(),
            "sandbox_boundary" | "client_capability"
        )
    }

    fn choices(&self) -> usize {
        self.request
            .fields
            .get(self.field)
            .map_or(0, |field| match field.kind.as_str() {
                "boolean" => 2,
                "question" => field.options.len() + 1,
                _ => field.options.len(),
            })
    }

    fn editing(&self) -> bool {
        self.request.fields.get(self.field).is_some_and(|field| {
            matches!(field.kind.as_str(), "string" | "number" | "integer")
                || (field.kind == "question" && self.choice == field.options.len())
        })
    }

    pub(crate) fn paste(&mut self, value: &str) {
        if self.visible && self.editing() {
            self.edit(Edit::Insert(value.into()));
        }
    }

    fn edit(&mut self, edit: Edit) {
        if let Edit::Insert(value) = &edit
            && !safe(value, 2048)
        {
            self.error = "输入含不支持的字符或超过 2048 字节；原值保留".into();
            return;
        }
        self.follow_input = true;
        if self
            .request
            .fields
            .get(self.field)
            .is_some_and(|field| field.kind == "question")
        {
            self.selected[self.field].clear();
        }
        if let Some(editor) = self.editors.get_mut(self.field) {
            self.error = match editor.update(edit) {
                Ok(_) => String::new(),
                Err(_) => "输入不合法或超过 2048 字节；原值保留".into(),
            };
        }
    }

    fn change_field(&mut self, next: usize) {
        self.field = next;
        self.choice = 0;
        self.action = 0;
        self.scroll = 0;
        self.error.clear();
        self.invalidate();
    }

    pub(crate) fn key(&mut self, key: KeyEvent) -> Option<HostCommand> {
        if !self.visible || !self.active || self.pending || key.kind == KeyEventKind::Release {
            return None;
        }
        let newline = (key.code == KeyCode::Enter && key.modifiers == KeyModifiers::SHIFT)
            || (key.code == KeyCode::Char('j') && key.modifiers == KeyModifiers::CONTROL);
        if newline
            && self.editing()
            && self
                .request
                .fields
                .get(self.field)
                .is_some_and(|field| matches!(field.kind.as_str(), "string" | "question"))
        {
            self.edit(Edit::Insert("\n".into()));
            return None;
        }
        if key.modifiers.intersects(
            KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SUPER
                | KeyModifiers::META
                | KeyModifiers::HYPER,
        ) {
            return None;
        }
        let press = key.kind == KeyEventKind::Press;
        match key.code {
            KeyCode::F(2) if press && matches!(self.request.kind.as_str(), "question" | "form") => {
                return Some(self.cancel());
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(5).min(self.max_scroll),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
            KeyCode::BackTab if press && !self.request.fields.is_empty() => {
                self.change_field(self.field.saturating_sub(1))
            }
            KeyCode::Tab if press && !self.summary() => {
                if self.value(self.field).is_ok() {
                    self.change_field(self.field + 1);
                } else {
                    self.error = "请完成当前字段，或使用 Esc 暂时返回聊天".into();
                }
            }
            KeyCode::Enter if press && !self.summary() => {
                if self.value(self.field).is_ok() {
                    self.change_field(self.field + 1);
                } else {
                    self.error = "当前值不满足要求；空格选择选项，Enter 下一步".into();
                }
            }
            KeyCode::Enter if press => return self.activate(),
            KeyCode::Left if self.summary() => self.action = self.action.saturating_sub(1),
            KeyCode::Right if self.summary() => {
                self.action = (self.action + 1).min(self.labels().len().saturating_sub(1))
            }
            KeyCode::Up if self.choices() > 0 => {
                self.follow_input = true;
                self.choice = self.choice.saturating_sub(1);
                self.scroll = self.scroll.saturating_sub(1);
            }
            KeyCode::Down if self.choices() > 0 => {
                self.follow_input = true;
                self.choice = (self.choice + 1).min(self.choices() - 1);
                let field = &self.request.fields[self.field];
                if field.kind == "question"
                    && self.choice == field.options.len()
                    && !self.selected[self.field].is_empty()
                {
                    self.selected[self.field].clear();
                    self.editors[self.field] = Composer::new(2048);
                }
                self.scroll = self.scroll.saturating_add(1).min(self.max_scroll);
            }
            KeyCode::Char(' ') if !self.editing() && self.choices() > 0 && press => {
                let field = &self.request.fields[self.field];
                let selected = &mut self.selected[self.field];
                let was_selected = selected.contains(&self.choice);
                if field.kind != "multi_select" {
                    selected.clear();
                }
                if was_selected {
                    selected.remove(&self.choice);
                } else {
                    selected.insert(self.choice);
                }
                if field.kind == "question" && self.choice < field.options.len() {
                    self.editors[self.field] = Composer::new(2048);
                    if !was_selected {
                        let _ = self.editors[self.field]
                            .update(Edit::Insert(field.options[self.choice].value.clone()));
                    }
                }
                self.error.clear();
            }
            code if self.editing() => match code {
                KeyCode::Char(ch) if !ch.is_control() => self.edit(Edit::Insert(ch.to_string())),
                KeyCode::Backspace => self.edit(Edit::Backspace),
                KeyCode::Delete => self.edit(Edit::Delete),
                KeyCode::Left => self.edit(Edit::Left),
                KeyCode::Right => self.edit(Edit::Right),
                KeyCode::Home => self.edit(Edit::Home),
                KeyCode::End => self.edit(Edit::End),
                _ => {}
            },
            _ => {}
        }
        None
    }

    pub(crate) fn wheel(&mut self, down: bool) {
        if down {
            self.scroll = self.scroll.saturating_add(3).min(self.max_scroll);
        } else {
            self.scroll = self.scroll.saturating_sub(3);
        }
    }

    pub(crate) fn click(&mut self, x: u16, y: u16) -> Option<HostCommand> {
        if !self.visible || !self.active || self.pending {
            return None;
        }
        if let Some(index) = self
            .buttons
            .iter()
            .position(|area| area.contains(Position::new(x, y)))
        {
            self.action = index;
            return self.activate();
        }
        None
    }

    fn labels(&self) -> Vec<&'static str> {
        match self.request.kind.as_str() {
            "sandbox_boundary" | "client_capability" => vec!["拒绝", "批准会话范围"],
            "unsupported" => vec![],
            _ if self.summary() => vec!["返回修改", "取消回答", "提交"],
            _ => vec!["取消回答 F2"],
        }
    }

    fn cancel(&self) -> HostCommand {
        HostCommand::Answer {
            id: self.request.id.clone(),
            action: InteractionAction::Cancel,
            values: BTreeMap::new(),
        }
    }

    fn activate(&mut self) -> Option<HostCommand> {
        if !self.summary() {
            return Some(self.cancel());
        }
        let action = match self.request.kind.as_str() {
            _ if self.authorization() && self.action == 0 => InteractionAction::Deny,
            _ if self.authorization() && self.reviewed => InteractionAction::Allow,
            "unsupported" => return None,
            _ if self.authorization() => {
                self.error = "请向下翻页查看完整授权内容，再允许".into();
                return None;
            }
            _ if self.action == 0 => {
                self.change_field(0);
                return None;
            }
            _ if self.action == 1 => InteractionAction::Cancel,
            _ if self.reviewed => InteractionAction::Accept,
            _ => {
                self.error = "请向下翻页查看完整回答，再提交".into();
                return None;
            }
        };
        let mut values = BTreeMap::new();
        if matches!(action, InteractionAction::Accept) {
            for index in 0..self.request.fields.len() {
                match self.value(index) {
                    Ok(Some(value)) => {
                        values.insert(self.request.fields[index].name.clone(), value);
                    }
                    Ok(None) => {}
                    Err(()) => {
                        self.change_field(index);
                        self.error = "请检查此字段".into();
                        return None;
                    }
                }
            }
        }
        Some(HostCommand::Answer {
            id: self.request.id.clone(),
            action,
            values,
        })
    }

    fn value(&self, index: usize) -> Result<Option<Value>, ()> {
        let field = &self.request.fields[index];
        let text = self.editors[index].text();
        let selected = &self.selected[index];
        let value = match field.kind.as_str() {
            "boolean" => selected.first().map(|index| Value::Bool(*index == 0)),
            "single_select" => selected
                .first()
                .and_then(|index| field.options.get(*index))
                .map(|option| Value::String(option.value.clone())),
            "multi_select" => {
                if field.min_items.is_some_and(|min| selected.len() < min)
                    || field.max_items.is_some_and(|max| selected.len() > max)
                {
                    return Err(());
                }
                if selected.is_empty() && !field.required {
                    None
                } else {
                    Some(Value::Array(
                        selected
                            .iter()
                            .filter_map(|index| field.options.get(*index))
                            .map(|option| Value::String(option.value.clone()))
                            .collect(),
                    ))
                }
            }
            "integer" if !text.is_empty() => {
                let number: i64 = text.parse().map_err(|_| ())?;
                if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&number)
                    || field.minimum.is_some_and(|min| (number as f64) < min)
                    || field.maximum.is_some_and(|max| (number as f64) > max)
                {
                    return Err(());
                }
                Some(number.into())
            }
            "number" if !text.is_empty() => {
                let number: f64 = text.parse().map_err(|_| ())?;
                if !number.is_finite()
                    || field.minimum.is_some_and(|min| number < min)
                    || field.maximum.is_some_and(|max| number > max)
                {
                    return Err(());
                }
                Some(serde_json::Number::from_f64(number).ok_or(())?.into())
            }
            _ if text.is_empty() => None,
            _ => {
                let len = text.chars().count();
                if field.min_length.is_some_and(|min| len < min)
                    || field.max_length.is_some_and(|max| len > max)
                {
                    return Err(());
                }
                Some(Value::String(text.into()))
            }
        };
        if field.required && value.is_none() {
            Err(())
        } else {
            Ok(value)
        }
    }

    fn content(&self) -> (String, Option<usize>) {
        let mut content = format!("来源：{}\n{}\n\n", self.request.source, self.request.detail);
        let mut focus = None;
        if let Some(field) = self.request.fields.get(self.field) {
            content.push_str(&format!(
                "问题 {}/{}：{}{}\n{}\n",
                self.field + 1,
                self.request.fields.len(),
                field.label,
                if field.required {
                    " *"
                } else {
                    "（可跳过）"
                },
                field.description
            ));
            let mut limits = Vec::new();
            if field.kind == "integer" {
                limits.push("请输入整数".to_owned());
            }
            if let Some(min) = field.minimum {
                limits.push(format!("最小 {min}"));
            }
            if let Some(max) = field.maximum {
                limits.push(format!("最大 {max}"));
            }
            if let Some(min) = field.min_length {
                limits.push(format!("至少 {min} 个字符"));
            }
            if let Some(max) = field.max_length {
                limits.push(format!("最多 {max} 个字符"));
            }
            if let Some(min) = field.min_items {
                limits.push(format!("至少选择 {min} 项"));
            }
            if let Some(max) = field.max_items {
                limits.push(format!("最多选择 {max} 项"));
            }
            if let Some(format) = &field.format {
                limits.push(format!("格式：{format}"));
            }
            if !limits.is_empty() {
                content.push_str(&format!("{}\n", limits.join(" · ")));
            }
            if field.kind == "boolean" {
                for (index, label) in ["是", "否"].iter().enumerate() {
                    if self.choice == index {
                        focus = Some(content.len());
                    }
                    content.push_str(&format!(
                        "{} [{}] {}\n",
                        if self.choice == index { "›" } else { " " },
                        if self.selected[self.field].contains(&index) {
                            "x"
                        } else {
                            " "
                        },
                        label
                    ));
                }
            } else {
                for (index, option) in field.options.iter().enumerate() {
                    if self.choice == index {
                        focus = Some(content.len());
                    }
                    content.push_str(&format!(
                        "{} [{}] {}\n",
                        if self.choice == index { "›" } else { " " },
                        if self.selected[self.field].contains(&index) {
                            "x"
                        } else {
                            " "
                        },
                        option.label
                    ));
                }
            }
            if field.kind == "question" {
                if self.choice == field.options.len() {
                    focus = Some(content.len());
                }
                content.push_str(&format!(
                    "{} 其他：自由输入\n",
                    if self.choice == field.options.len() {
                        "›"
                    } else {
                        " "
                    }
                ));
            }
            if self.editing() {
                let editor = &self.editors[self.field];
                focus = Some(content.len() + "输入：".len() + editor.cursor());
                content.push_str(&format!(
                    "输入：{}▏{}\n",
                    &editor.text()[..editor.cursor()],
                    &editor.text()[editor.cursor()..]
                ));
            }
            content.push_str("\n↑↓ 移动选项 · 空格选中 · Enter/Tab 下一步 · Shift+Tab 上一步\n文字输入：Shift+Enter/Ctrl+J 换行");
        } else if self.request.kind == "unsupported" {
            content.push_str("此请求暂不支持在终端回答。请在其他 Maka 客户端处理；不会自动授权。");
        } else if !self.authorization() {
            content.push_str("确认以下回答（尚未提交）：\n");
            for (index, field) in self.request.fields.iter().enumerate() {
                content.push_str(&format!(
                    "{}：{}\n",
                    field.label,
                    self.value(index)
                        .ok()
                        .flatten()
                        .map_or_else(|| "未填写".into(), |value| value.to_string())
                ));
            }
        }
        (content, focus)
    }

    pub(crate) fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(Clear, area);
        let border = Block::default()
            .borders(Borders::ALL)
            .title(self.request.title.as_str());
        let inner = border.inner(area);
        frame.render_widget(border, area);
        self.buttons.clear();
        if !self.active || self.pending {
            self.visible = false;
            frame.render_widget(
                Paragraph::new(if self.pending {
                    "正在等待 Host 确认；不会重复提交。Esc 暂时返回。"
                } else {
                    "请求已结束或已变化，不能再回答。Esc 返回聊天。"
                }),
                inner,
            );
            return;
        }
        self.visible = area.width >= 40 && area.height >= 10;
        if !self.visible {
            frame.render_widget(
                Paragraph::new("请放大窗口（至少 40×10）；Esc 返回，不提交答案"),
                inner,
            );
            self.reviewed = false;
            return;
        }
        let body = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(3),
        );
        let (content, focus) = self.content();
        let wrapped = text::wrap(&content, body.width);
        self.max_scroll = wrapped.len().saturating_sub(usize::from(body.height));
        if self.follow_input
            && let Some(focus) = focus
            && let Some(row) = wrapped
                .iter()
                .position(|line| line.start <= focus && focus <= line.end)
        {
            if row < self.scroll {
                self.scroll = row;
            }
            if row >= self.scroll + usize::from(body.height) {
                self.scroll = row
                    .saturating_add(1)
                    .saturating_sub(usize::from(body.height));
            }
        }
        self.follow_input = false;
        self.scroll = self.scroll.min(self.max_scroll);
        self.reviewed |= self.scroll == self.max_scroll;
        let lines: Vec<_> = wrapped
            .iter()
            .skip(self.scroll)
            .take(usize::from(body.height))
            .map(|line| Line::raw(line.text.as_str()))
            .collect();
        frame.render_widget(Paragraph::new(lines), body);
        frame.render_widget(
            Paragraph::new(self.error.as_str()),
            Rect::new(inner.x, inner.bottom() - 3, inner.width, 1),
        );
        let mut x = inner.x;
        for (index, label) in self.labels().iter().enumerate() {
            let text = format!(
                "{}[{}] ",
                if self.action == index { "›" } else { " " },
                label
            );
            let width = u16::try_from(crate::text::columns(&text))
                .unwrap_or(inner.width)
                .min(inner.right().saturating_sub(x));
            let button = Rect::new(x, inner.bottom() - 2, width, 1);
            frame.render_widget(Paragraph::new(text), button);
            self.buttons.push(button);
            x = x.saturating_add(width);
        }
        frame.render_widget(
            Paragraph::new("PgUp/PgDn 滚动 · ←→ 选操作 · Esc 暂时返回"),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    fn draw(panel: &mut InteractionPanel, width: u16, height: u16) {
        Terminal::new(TestBackend::new(width, height))
            .unwrap()
            .draw(|frame| panel.render(frame, frame.area()))
            .unwrap();
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn permission() -> HostInteraction {
        serde_json::from_value(json!({"id":"request-1", "kind":"sandbox_boundary", "title":"会话沙箱范围", "source":"Host", "detail":"读取 /tmp/example", "fields":[]})).unwrap()
    }

    #[test]
    fn permission_defaults_to_denial_and_never_allows_hidden_or_expired_actions() {
        let mut panel = InteractionPanel::new(permission());
        draw(&mut panel, 80, 24);
        assert!(matches!(
            panel.key(key(KeyCode::Enter)),
            Some(HostCommand::Answer {
                action: InteractionAction::Deny,
                ..
            })
        ));
        panel.key(key(KeyCode::Right));
        panel.invalidate();
        assert!(panel.key(key(KeyCode::Enter)).is_none());
        draw(&mut panel, 20, 5);
        assert!(panel.key(key(KeyCode::Enter)).is_none());
        draw(&mut panel, 80, 24);
        panel.expire();
        draw(&mut panel, 80, 24);
        assert!(panel.key(key(KeyCode::Enter)).is_none());
    }

    #[test]
    fn long_permission_requires_reviewing_the_bottom_before_allowing() {
        let mut request = permission();
        request.detail = "完整风险信息\n".repeat(40);
        let mut panel = InteractionPanel::new(request);
        draw(&mut panel, 80, 16);
        panel.key(key(KeyCode::Right));
        assert!(panel.key(key(KeyCode::Enter)).is_none());
        for _ in 0..12 {
            panel.key(key(KeyCode::PageDown));
        }
        draw(&mut panel, 80, 16);
        assert!(matches!(
            panel.key(key(KeyCode::Enter)),
            Some(HostCommand::Answer {
                action: InteractionAction::Allow,
                ..
            })
        ));
    }

    #[test]
    fn switching_to_free_answer_does_not_keep_the_previous_option() {
        let request = serde_json::from_value(json!({"id":"question-1", "kind":"question", "title":"提问", "source":"Host", "detail":"", "fields":[{
            "name":"0", "label":"时间", "kind":"question", "required":false,
            "options":[{"value":"本周","label":"本周"},{"value":"下周","label":"下周"}]
        }]})).unwrap();
        let mut panel = InteractionPanel::new(request);
        draw(&mut panel, 80, 24);
        panel.key(key(KeyCode::Char(' ')));
        panel.key(key(KeyCode::Down));
        panel.key(key(KeyCode::Down));
        assert_eq!(panel.value(0), Ok(None));
        panel.paste("明年");
        assert_eq!(panel.value(0), Ok(Some(Value::String("明年".into()))));
    }

    #[test]
    fn multi_select_requires_explicit_selection_and_a_separate_submit_step() {
        let request = serde_json::from_value(json!({"id":"form-1", "kind":"form", "title":"表单", "source":"MCP test", "detail":"选择目标", "fields":[{
            "name":"targets", "label":"目标", "kind":"multi_select", "required":true, "minItems":1,
            "options":[{"value":"one","label":"目标一"},{"value":"two","label":"目标二"}]
        }]})).unwrap();
        let mut panel = InteractionPanel::new(request);
        draw(&mut panel, 80, 24);
        assert!(matches!(
            panel.key(key(KeyCode::F(2))),
            Some(HostCommand::Answer {
                action: InteractionAction::Cancel,
                ..
            })
        ));
        assert!(panel.key(key(KeyCode::Enter)).is_none());
        assert!(!panel.summary());
        panel.key(key(KeyCode::Char(' ')));
        panel.key(key(KeyCode::Down));
        panel.key(key(KeyCode::Char(' ')));
        assert!(panel.key(key(KeyCode::Enter)).is_none());
        draw(&mut panel, 80, 24);
        panel.key(key(KeyCode::Right));
        panel.key(key(KeyCode::Right));
        let mut repeated = key(KeyCode::Enter);
        repeated.kind = KeyEventKind::Repeat;
        assert!(panel.key(repeated).is_none());
        let Some(HostCommand::Answer {
            action: InteractionAction::Accept,
            values,
            ..
        }) = panel.key(key(KeyCode::Enter))
        else {
            panic!("explicit submit must produce an answer");
        };
        assert_eq!(values.get("targets"), Some(&json!(["one", "two"])));
    }
}
