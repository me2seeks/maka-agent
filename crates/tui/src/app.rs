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

//! Deterministic M0 workbench and its single input-routing policy.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block as Border, Borders, Clear, Paragraph},
};

use crate::{
    commands::{Command, Commands, Entry},
    composer::{Composer, Edit, VerticalDirection},
    host_ui::{HostBlock, HostBlockKind, HostCommand, HostEvent, HostInteraction},
    i18n,
    interaction::{InteractionPanel, valid_requests},
    text,
    transcript::{Block, BlockKind, Reading, Transcript, TranscriptError},
};

/// Keyboard focus is separate from transcript reading position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Edit the draft while mouse scrolling can still browse history.
    Composer,
    /// Navigate transcript with the keyboard.
    Transcript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    Help,
    Permission(PermissionKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PermissionKey {
    id: u64,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermissionState {
    Pending,
    Expired,
    Answered,
}

struct DemoPermission {
    key: PermissionKey,
    state: PermissionState,
}

struct InputCapabilities {
    bracketed_paste: bool,
    enhanced_keys: bool,
}

/// Observations from the optional local echo Companion, never a Host receipt.
pub enum CompanionFeedback {
    /// The private protocol handshake succeeded.
    Ready,
    /// The exact requested snapshot was echoed; edits made since then remain local.
    Echoed {
        /// UTF-8 bytes in the acknowledged snapshot.
        bytes: usize,
    },
    /// The draft exceeds the small diagnostic channel's budget.
    TooLarge,
    /// Transport failed. No request is automatically retried.
    Disconnected,
}

#[derive(Clone, Copy)]
enum CompanionState {
    Connecting,
    Ready,
    Pending { revision: u64 },
    Disconnected,
}

struct HostView {
    session: String,
    ready: bool,
    running: bool,
    waiting: bool,
    pending: Option<u64>,
    stop_pending: bool,
    identities: Vec<String>,
    command: Option<HostCommand>,
    interactions: Vec<HostInteraction>,
    answering: Option<String>,
}

/// Terminal state and user intents; Host I/O remains owned by the caller.
pub struct App {
    transcript: Transcript,
    composer: Composer,
    focus: Focus,
    overlay: Option<Overlay>,
    commands: Option<Commands>,
    status: String,
    edit_error: Option<String>,
    layout_area: Rect,
    transcript_area: Rect,
    composer_area: Rect,
    composer_width: u16,
    geometry_published: bool,
    input_capabilities: Option<InputCapabilities>,
    replay: bool,
    streaming: bool,
    ticks: usize,
    permission: Option<DemoPermission>,
    overlay_actions_visible: bool,
    consumed_overlay_key: Option<KeyCode>,
    quit: bool,
    companion: Option<CompanionState>,
    echo_requested: bool,
    host: Option<HostView>,
    interaction: Option<InteractionPanel>,
    interaction_open: bool,
}

impl App {
    /// Constructs a quiet chat shell without replay fixtures or a Host connection.
    pub fn chat_demo() -> Self {
        Self::new(Transcript::new(), false)
    }

    /// Constructs a reproducible fixture with five long thinking blocks.
    ///
    /// # Errors
    /// Returns a transcript validation error if the built-in fixture is invalid.
    pub fn demo() -> Result<Self, TranscriptError> {
        let mut transcript = Transcript::new();
        transcript.push(Block::new(
            1,
            BlockKind::User,
            "You · demo fixture".into(),
            "Read earlier reasoning while editing a new draft. No Host is connected.".into(),
        ))?;
        for id in 2..=6 {
            transcript.push(Block::new(id, BlockKind::Thinking, format!("Thinking {} · completed fixture", id - 1), (1..=12).map(|line| format!("Step {line}: preserve reading position while input continues. 中文 / e\u{301} / 👩‍👩‍👧‍👦\n")).collect()))?;
        }
        transcript.push(Block::new(7, BlockKind::Tool, "Tool · FAILED · fixture only".into(), "One assertion failed. This status remains visible independently of thinking expansion.".into()))?;
        transcript.push(Block::new(
            8,
            BlockKind::Assistant,
            "Assistant · replay stream".into(),
            "Deterministic output: ".into(),
        ))?;
        transcript.toggle_thinking();
        Ok(Self::new(transcript, true))
    }

    fn new(transcript: Transcript, replay: bool) -> Self {
        Self {
            transcript,
            composer: Composer::new(64 * 1024),
            focus: Focus::Composer,
            overlay: None,
            commands: None,
            status: if replay {
                "DEMO ONLY · draft is not persisted · Enter: newline · F5: preview".into()
            } else {
                i18n::CHAT_NOTICE.into()
            },
            edit_error: None,
            layout_area: Rect::default(),
            transcript_area: Rect::default(),
            composer_area: Rect::default(),
            composer_width: 80,
            geometry_published: false,
            input_capabilities: None,
            replay,
            streaming: replay,
            ticks: 0,
            permission: None,
            overlay_actions_visible: false,
            consumed_overlay_key: None,
            quit: false,
            companion: None,
            echo_requested: false,
            host: None,
            interaction: None,
            interaction_open: false,
        }
    }

    /// Opens an empty view for one explicitly selected existing Host session.
    pub fn host_session(session: String) -> Self {
        let mut app = Self::chat_demo();
        app.host = Some(HostView {
            session,
            ready: false,
            running: false,
            waiting: false,
            pending: None,
            stop_pending: false,
            identities: Vec::new(),
            command: None,
            interactions: Vec::new(),
            answering: None,
        });
        app.set_status("正在连接 Host 并加载会话…");
        app
    }

    /// Takes one explicit intent without blocking terminal input.
    pub fn take_host_command(&mut self) -> Option<HostCommand> {
        self.host.as_mut().and_then(|host| host.command.take())
    }

    /// Applies a correlated client event. False asks the caller to close the bridge.
    pub fn host_event(&mut self, event: HostEvent) -> bool {
        let Some(host) = &mut self.host else {
            return false;
        };
        let accepted = match event {
            HostEvent::Interactions { requests } if valid_requests(&requests) => {
                if let Some(panel) = &mut self.interaction
                    && !requests.iter().any(|request| request == &panel.request)
                {
                    panel.expire();
                }
                host.interactions = requests;
                true
            }
            HostEvent::Answered { id, accepted } if host.answering.as_ref() == Some(&id) => {
                host.answering = None;
                if accepted {
                    host.interactions.retain(|request| request.id != id);
                    self.interaction = None;
                    self.interaction_open = false;
                    self.consumed_overlay_key = Some(KeyCode::Enter);
                } else if let Some(panel) = &mut self.interaction {
                    panel.reject();
                }
                self.set_status(if accepted {
                    "Host 已确认回答"
                } else {
                    "回答未提交：请求可能已结束或内容不符合要求；未自动重试"
                });
                true
            }
            HostEvent::Ready { session_id } if !host.ready && host.session == session_id => {
                host.ready = true;
                self.set_status("已连接 · Enter 发送 · 运行时 Ctrl+C 停止");
                true
            }
            HostEvent::History { blocks } if !host.ready && host.identities.is_empty() => {
                let mut transcript = Transcript::new();
                let mut identities = Vec::new();
                let valid = blocks.into_iter().all(|block| {
                    !identities.contains(&block.id)
                        && Self::host_block(&mut transcript, &mut identities, block).is_ok()
                });
                if valid {
                    self.transcript = transcript;
                    host.identities = identities;
                }
                valid
            }
            HostEvent::Upsert { block } => {
                Self::host_block(&mut self.transcript, &mut host.identities, block).is_ok()
            }
            HostEvent::State { running, waiting } => {
                let changed = host.running != running || host.waiting != waiting;
                host.running = running;
                host.waiting = waiting;
                if changed {
                    self.set_status(if waiting {
                        "等待你的处理 · F3 或 /requests 查看；不会自动授权"
                    } else if running {
                        "正在运行 · Ctrl+C 停止"
                    } else {
                        "就绪 · Enter 发送"
                    });
                }
                true
            }
            HostEvent::Submitted { revision, accepted } if host.pending == Some(revision) => {
                host.pending = None;
                if accepted && self.composer.revision() == revision {
                    self.composer = Composer::new(64 * 1024);
                }
                self.set_status(if accepted {
                    "Host 已接收"
                } else {
                    "Host 未接收；输入已保留"
                });
                true
            }
            HostEvent::Stopped {} if host.stop_pending => {
                host.stop_pending = false;
                self.set_status("Host 已确认停止请求");
                true
            }
            HostEvent::Notice { text }
                if text.len() <= 512 && !text.chars().any(char::is_control) =>
            {
                self.set_status(text);
                true
            }
            _ => false,
        };
        if !accepted {
            if let Some(host) = &mut self.host {
                host.ready = false;
                host.running = false;
                host.command = None;
                host.interactions.clear();
            }
            if let Some(panel) = &mut self.interaction {
                panel.expire();
            }
            self.set_status("Host 已断开；输入已保留。未确认的发送或回答结果未知，不会自动重试");
        }
        accepted
    }

    fn host_block(
        transcript: &mut Transcript,
        identities: &mut Vec<String>,
        block: HostBlock,
    ) -> Result<(), TranscriptError> {
        if block.id.is_empty() || block.id.len() > 512 {
            return Err(TranscriptError::Identity);
        }
        let index = identities
            .iter()
            .position(|id| id == &block.id)
            .unwrap_or(identities.len());
        let kind = match block.kind {
            HostBlockKind::User => BlockKind::User,
            HostBlockKind::Assistant => BlockKind::Assistant,
            HostBlockKind::Thinking => BlockKind::Thinking,
            HostBlockKind::Tool => BlockKind::Tool,
        };
        transcript.replace(Block::new(index as u64 + 1, kind, block.title, block.text))?;
        if index == identities.len() {
            identities.push(block.id);
        }
        Ok(())
    }

    fn open_interaction(&mut self) {
        let Some(host) = &self.host else {
            self.set_status("当前未连接 Host，没有真实交互请求");
            return;
        };
        if !host.ready || host.answering.is_some() {
            self.set_status("尚未连接或正在等待回答确认，请稍候");
            return;
        }
        if self.interaction.as_ref().is_some_and(|panel| {
            host.interactions
                .iter()
                .any(|request| request == &panel.request)
        }) {
            self.interaction_open = true;
        } else if let Some(request) = host
            .interactions
            .iter()
            .find(|request| request.kind != "unsupported")
            .or_else(|| host.interactions.first())
        {
            self.interaction = Some(InteractionPanel::new(request.clone()));
            self.interaction_open = true;
        } else {
            self.set_status("当前没有待处理请求");
        }
    }

    fn answer_interaction(&mut self, command: HostCommand) {
        let HostCommand::Answer { id, .. } = &command else {
            return;
        };
        let Some(host) = &mut self.host else {
            return;
        };
        if !host.ready
            || host.answering.is_some()
            || host.pending.is_some()
            || host.stop_pending
            || !host.interactions.iter().any(|request| &request.id == id)
        {
            self.set_status("当前不能回答：请求已结束或仍有操作等待确认");
            return;
        }
        host.answering = Some(id.clone());
        host.command = Some(command);
        if let Some(panel) = &mut self.interaction {
            panel.set_pending(true);
        }
        self.set_status("正在提交回答，等待 Host 确认…");
    }

    fn send_to_host(&mut self) {
        let Some(host) = &mut self.host else {
            self.set_status(i18n::CHAT_SEND_UNAVAILABLE);
            return;
        };
        if !host.ready {
            self.set_status("尚未连接，无法发送；输入已保留");
            return;
        }
        if host.pending.is_some() {
            self.set_status("正在等待 Host 接收确认，请勿重复发送");
            return;
        }
        if host.stop_pending {
            self.set_status("正在等待停止确认，请稍后发送");
            return;
        }
        if host.answering.is_some() {
            self.set_status("正在等待 Host 确认回答，请稍后发送");
            return;
        }
        if self.composer.text().trim().is_empty() {
            self.set_status(i18n::CHAT_EMPTY);
            return;
        }
        if self.composer.text().len() > 16 * 1024 {
            self.set_status("消息超过当前 16 KiB 显示上限；请缩短后发送");
            return;
        }
        let revision = self.composer.revision();
        host.pending = Some(revision);
        host.command = Some(HostCommand::Send {
            revision,
            text: self.composer.text().into(),
        });
        self.set_status("正在发送，等待 Host 确认…");
    }

    /// Enables an explicitly chosen local IPC experiment. Drafts stay memory-only.
    pub fn enable_companion(&mut self) {
        self.companion = Some(CompanionState::Connecting);
        self.set_status("Companion connecting · local echo only · no Host");
    }

    /// Takes one user-initiated echo action. Input routing remains inside App.
    pub fn take_companion_echo(&mut self) -> bool {
        std::mem::take(&mut self.echo_requested)
    }

    /// Updates transport feedback without changing draft, focus or reading position.
    pub fn companion_feedback(&mut self, feedback: CompanionFeedback) {
        match feedback {
            CompanionFeedback::Ready => {
                self.companion = Some(CompanionState::Ready);
                self.set_status("Companion ready · F5: local echo (up to 4 KiB) · NOT Host send");
            }
            CompanionFeedback::Echoed { bytes } => {
                if let Some(CompanionState::Pending { revision }) = self.companion {
                    self.companion = Some(CompanionState::Ready);
                    self.set_status(format!(
                        "Local echo: {bytes} bytes from draft r{revision} · NOT sent to Host · draft kept"
                    ));
                }
            }
            CompanionFeedback::TooLarge => {
                self.companion = Some(CompanionState::Ready);
                self.set_status("Echo limit: 4 KiB · nothing sent · complete draft kept");
            }
            CompanionFeedback::Disconnected => {
                self.companion = Some(CompanionState::Disconnected);
                self.echo_requested = false;
                self.set_status("Companion disconnected · draft kept · no retry · no Host");
            }
        }
    }

    fn companion_badge(&self) -> &'static str {
        match self.companion {
            None => "",
            Some(CompanionState::Connecting) => "IPC connecting · ",
            Some(CompanionState::Ready) => "IPC ready · ",
            Some(CompanionState::Pending { .. }) => "IPC waiting · ",
            Some(CompanionState::Disconnected) => "IPC OFF · ",
        }
    }

    fn preview_or_echo(&mut self) {
        if self.composer.text().is_empty() {
            self.set_status(if self.replay {
                "Draft is empty"
            } else {
                i18n::CHAT_EMPTY
            });
            return;
        }
        match self.companion {
            None if !self.replay => self.set_status(i18n::preview(self.composer.text().len())),
            None => self.set_status(format!(
                "Preview only · {} bytes · not sent · draft retained",
                self.composer.text().len()
            )),
            Some(CompanionState::Ready) => {
                self.companion = Some(CompanionState::Pending {
                    revision: self.composer.revision(),
                });
                self.echo_requested = true;
                self.set_status("Local echo waiting · keep editing · NOT Host send");
            }
            Some(CompanionState::Pending { .. }) => {
                self.set_status("Local echo already waiting · no duplicate queued · draft kept");
            }
            Some(CompanionState::Connecting) => {
                self.set_status("Companion still connecting · nothing queued · draft kept");
            }
            Some(CompanionState::Disconnected) => {
                self.set_status("Companion disconnected · nothing queued · draft kept");
            }
        }
    }

    /// Read-only draft access for callers and behavior tests.
    pub fn composer(&self) -> &Composer {
        &self.composer
    }

    /// Read-only transcript state for callers and behavior tests.
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// Current keyboard focus, independent of overlays and mouse scrolling.
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// Whether the event loop should exit after an idle exit request.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Whether more deterministic replay ticks are needed.
    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    /// Records which optional input modes the terminal backend accepted.
    /// A successful request is not proof that an emulator implements that mode.
    pub fn set_input_capabilities(&mut self, bracketed_paste: bool, enhanced_keys: bool) {
        self.input_capabilities = Some(InputCapabilities {
            bracketed_paste,
            enhanced_keys,
        });
        if let Some(warning) = self.input_warning() {
            self.set_status(warning);
        }
    }

    /// Applies one replay tick. The caller controls time; no timer thread exists.
    pub fn tick(&mut self) {
        if !self.streaming {
            return;
        }
        self.ticks += 1;
        if let Err(error) = self.transcript.append(
            8,
            if self.ticks.is_multiple_of(6) {
                "new line\n"
            } else {
                "token "
            },
        ) {
            self.set_status(error.to_string());
            self.streaming = false;
        }
        if self.ticks == 12 {
            self.permission = Some(DemoPermission {
                key: PermissionKey {
                    id: 1,
                    generation: 1,
                },
                state: PermissionState::Pending,
            });
            self.set_status("F3: request waiting · DEMO · input focus unchanged");
        }
        if self.ticks == 60
            && let Some(permission) = &mut self.permission
            && permission.state == PermissionState::Pending
        {
            permission.state = PermissionState::Expired;
            self.set_status("F3: request expired · no answer will be recorded");
        }
        if self.ticks >= 120 {
            self.streaming = false;
        }
    }

    /// Routes an event once. Paste never becomes commands or a submit action.
    pub fn update(&mut self, event: Event) {
        match event {
            Event::Resize(width, height) => {
                if let Some(panel) = &mut self.interaction {
                    panel.invalidate();
                }
                if let Some(commands) = &mut self.commands {
                    commands.resize(width, height);
                }
                // Input can arrive during draw throttling. Withdraw affirmative
                // actions immediately and use new widths for keyboard navigation.
                self.overlay_actions_visible = false;
                self.geometry_published = false;
                self.prepare_layout(Rect::new(0, 0, width, height));
            }
            Event::Key(key) => {
                let identity = match key.code {
                    KeyCode::Char(character) => KeyCode::Char(character.to_ascii_lowercase()),
                    code => code,
                };
                if self.consumed_overlay_key == Some(identity) {
                    if key.kind == KeyEventKind::Repeat {
                        return;
                    }
                    // A reported release or a fresh press begins a new intent.
                    // Legacy streams that label all repeats Press cannot prove it.
                    self.consumed_overlay_key = None;
                }
                if key.kind != KeyEventKind::Release {
                    self.key(key);
                }
            }
            Event::Paste(value) if self.interaction_open => {
                if let Some(panel) = &mut self.interaction {
                    panel.paste(&value);
                }
            }
            Event::Paste(value) if self.commands.is_some() => {
                if let Some(commands) = &mut self.commands {
                    commands.paste(&value);
                }
            }
            Event::Paste(value)
                if self.overlay.is_none()
                    && self.commands.is_none()
                    && self.focus == Focus::Composer =>
            {
                self.edit(Edit::Insert(value))
            }
            Event::Mouse(mouse) if self.interaction_open => {
                if let Some(panel) = &mut self.interaction {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => panel.wheel(false),
                        MouseEventKind::ScrollDown => panel.wheel(true),
                        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                            if let Some(command) = panel.click(mouse.column, mouse.row) {
                                self.answer_interaction(command);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Event::Mouse(mouse) if self.overlay.is_none() && self.geometry_published => {
                if let Some(commands) = &mut self.commands {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => commands.scroll(false),
                        MouseEventKind::ScrollDown => commands.scroll(true),
                        _ => {}
                    }
                    if mouse.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left)
                        && let Some(command) = commands.click(mouse.column, mouse.row)
                    {
                        self.run_command(command);
                    }
                    return;
                }
                let position = Position::new(mouse.column, mouse.row);
                if self.transcript_area.contains(position) {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => self.transcript.scroll(-3),
                        MouseEventKind::ScrollDown => self.transcript.scroll(3),
                        MouseEventKind::Down(_) if !self.transcript.visible().is_empty() => {
                            self.focus = Focus::Transcript
                        }
                        _ => {}
                    }
                } else if self.composer_area.contains(position)
                    && matches!(mouse.kind, MouseEventKind::Down(_))
                {
                    self.focus = Focus::Composer;
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, key: KeyEvent) {
        let pressed = key.kind == KeyEventKind::Press;
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.interaction_open
            && let Some(panel) = &mut self.interaction
        {
            if pressed && key.code == KeyCode::Esc && key.modifiers.is_empty() {
                self.interaction_open = false;
                self.consumed_overlay_key = Some(KeyCode::Esc);
            } else if let Some(command) = panel.key(key) {
                self.answer_interaction(command);
                self.consumed_overlay_key = Some(key.code);
            }
            return;
        }
        if let Some(commands) = &mut self.commands {
            if pressed && key.code == KeyCode::Esc && key.modifiers.is_empty() {
                self.commands = None;
                self.consumed_overlay_key = Some(KeyCode::Esc);
            } else if let Some(command) = commands.key(key) {
                self.run_command(command);
                self.consumed_overlay_key = Some(key.code);
            }
            return;
        }
        if let Some(overlay) = self.overlay {
            if !pressed
                || key.modifiers.intersects(
                    KeyModifiers::CONTROL
                        | KeyModifiers::ALT
                        | KeyModifiers::SUPER
                        | KeyModifiers::META
                        | KeyModifiers::HYPER,
                )
            {
                return;
            }
            match (overlay, key.code) {
                (_, KeyCode::Esc) => self.overlay = None,
                (Overlay::Help, KeyCode::F(1)) => self.overlay = None,
                (Overlay::Permission(key), KeyCode::Char('a')) if self.overlay_actions_visible => {
                    self.answer_permission(key, true);
                }
                (Overlay::Permission(key), KeyCode::Char('d') | KeyCode::Enter) => {
                    self.answer_permission(key, false);
                }
                _ => {}
            }
            if self.overlay.is_none() {
                self.consumed_overlay_key = Some(match key.code {
                    KeyCode::Char(character) => KeyCode::Char(character.to_ascii_lowercase()),
                    code => code,
                });
            }
            return;
        }
        if pressed
            && ((key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('p'))
                || (key.code == KeyCode::Char('/')
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL
                            | KeyModifiers::ALT
                            | KeyModifiers::SUPER
                            | KeyModifiers::META
                            | KeyModifiers::HYPER,
                    )
                    && self.focus == Focus::Composer
                    && self.composer.text().is_empty()))
        {
            self.commands = Some(Commands::new(
                if self.geometry_published {
                    self.layout_area
                } else {
                    Rect::default()
                },
                if key.code == KeyCode::Char('/') {
                    Entry::Slash
                } else {
                    Entry::Shortcut
                },
            ));
            return;
        }
        if key.modifiers == KeyModifiers::CONTROL
            && key.code == KeyCode::Char('c')
            && pressed
            && self.replay
            && self.streaming
        {
            self.streaming = false;
            self.set_status(
                "Replay stopped locally · no Host Turn was running · F2: resume replay",
            );
            return;
        }
        if key.modifiers == KeyModifiers::CONTROL && pressed && key.code == KeyCode::Char('c') {
            if let Some(host) = &mut self.host {
                if host.ready && host.stop_pending {
                    self.set_status("正在等待 Host 停止确认…");
                    return;
                }
                if host.ready && host.running {
                    if !host.stop_pending {
                        host.stop_pending = true;
                        host.command = Some(HostCommand::Stop);
                    }
                    self.set_status("已请求停止，等待 Host 确认…");
                    return;
                }
                if host.ready && (host.pending.is_some() || host.answering.is_some()) {
                    self.set_status("正在等待发送确认；连接中断时不会自动重发");
                    return;
                }
            }
            self.quit = true;
            return;
        }
        match key.code {
            KeyCode::F(1) if pressed => self.overlay = Some(Overlay::Help),
            KeyCode::F(2) if pressed => {
                if !self.replay {
                    self.set_status(if self.host.is_some() {
                        "当前是 Host 会话，没有演示回放"
                    } else {
                        i18n::CHAT_NO_REPLAY
                    });
                    return;
                }
                self.streaming = !self.streaming && self.ticks < 120;
                self.set_status(if self.streaming {
                    "Replay resumed"
                } else {
                    "Replay paused or complete"
                });
            }
            KeyCode::F(3) if pressed => {
                if self.host.is_some() {
                    self.open_interaction();
                } else if let Some(permission) = &self.permission {
                    self.overlay = Some(Overlay::Permission(permission.key));
                    self.overlay_actions_visible = false;
                } else {
                    self.set_status(if self.host.is_some() {
                        "请在现有 Maka 客户端中处理权限或交互请求"
                    } else if self.replay {
                        "No pending demo request; one arrives after 12 replay ticks"
                    } else {
                        i18n::CHAT_NO_REQUEST
                    });
                }
            }
            KeyCode::F(4) if pressed && !self.transcript_area.is_empty() => {
                self.transcript.follow_latest()
            }
            KeyCode::F(5) if pressed => {
                self.preview_or_echo();
            }
            KeyCode::F(6)
                if pressed
                    && !self.transcript_area.is_empty()
                    && !self.transcript.visible().is_empty() =>
            {
                self.focus = if self.focus == Focus::Composer {
                    Focus::Transcript
                } else {
                    Focus::Composer
                }
            }
            KeyCode::Char('t') if control && pressed => self.transcript.toggle_thinking(),
            KeyCode::Esc if pressed => self.focus = Focus::Composer,
            _ if self.focus == Focus::Transcript => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.transcript.scroll(-1),
                KeyCode::Down | KeyCode::Char('j') => self.transcript.scroll(1),
                KeyCode::PageUp => self
                    .transcript
                    .scroll(-isize::try_from(self.transcript_area.height.max(1)).unwrap_or(1)),
                KeyCode::PageDown => self
                    .transcript
                    .scroll(isize::try_from(self.transcript_area.height.max(1)).unwrap_or(1)),
                KeyCode::Home => self.transcript.scroll(isize::MIN),
                KeyCode::End => self.transcript.follow_latest(),
                _ => {}
            },
            _ => self.editor_key(key),
        }
    }

    fn permission_is_pending(&self, key: PermissionKey) -> bool {
        self.permission.as_ref().is_some_and(|permission| {
            permission.key == key && permission.state == PermissionState::Pending
        })
    }

    fn run_command(&mut self, command: Command) {
        self.commands = None;
        match command {
            Command::Requests => self.open_interaction(),
            Command::Help => self.overlay = Some(Overlay::Help),
            Command::Latest => {
                self.transcript.follow_latest();
                self.set_status(if self.transcript.visible().is_empty() {
                    if self.host.is_some() {
                        "当前没有可显示的历史消息"
                    } else {
                        i18n::LATEST_EMPTY
                    }
                } else {
                    i18n::LATEST_DONE
                });
            }
            Command::Thinking => {
                self.transcript.toggle_thinking();
                self.set_status(if self.transcript.thinking_expanded() {
                    i18n::THINKING_EXPANDED
                } else {
                    i18n::THINKING_FOLDED
                });
            }
            Command::Preview => self.preview_or_echo(),
            Command::LiteralSlash => self.edit(Edit::Insert("/".into())),
        }
    }

    fn answer_permission(&mut self, key: PermissionKey, allow: bool) {
        if !self.permission_is_pending(key) {
            self.set_status("Demo request expired or already answered · no answer recorded");
            return;
        }
        if let Some(permission) = &mut self.permission {
            permission.state = PermissionState::Answered;
        }
        self.overlay = None;
        self.set_status(format!(
            "Demo answer: {} · no permission was granted to a Host",
            if allow { "allow" } else { "deny" }
        ));
    }

    fn editor_key(&mut self, key: KeyEvent) {
        if !self.replay && key.code == KeyCode::Enter && key.modifiers.is_empty() {
            if key.kind == KeyEventKind::Press {
                self.send_to_host();
            }
            return;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if matches!(key.code, KeyCode::Up | KeyCode::Down) {
            self.composer.move_visual(
                if key.code == KeyCode::Up {
                    VerticalDirection::Up
                } else {
                    VerticalDirection::Down
                },
                self.composer_width,
            );
            return;
        }
        let edit = match key.code {
            KeyCode::Char('z') if control => Some(Edit::Undo),
            KeyCode::Char('y') if control => Some(Edit::Redo),
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL
                        | KeyModifiers::ALT
                        | KeyModifiers::SUPER
                        | KeyModifiers::META
                        | KeyModifiers::HYPER,
                ) =>
            {
                Some(Edit::Insert(ch.to_string()))
            }
            KeyCode::Enter if self.replay || key.modifiers == KeyModifiers::SHIFT => {
                Some(Edit::Insert("\n".into()))
            }
            KeyCode::Char('j') if key.modifiers == KeyModifiers::CONTROL => {
                Some(Edit::Insert("\n".into()))
            }
            KeyCode::Tab => Some(Edit::Insert("\t".into())),
            KeyCode::Left => Some(Edit::Left),
            KeyCode::Right => Some(Edit::Right),
            KeyCode::Home => Some(Edit::Home),
            KeyCode::End => Some(Edit::End),
            KeyCode::Backspace => Some(Edit::Backspace),
            KeyCode::Delete => Some(Edit::Delete),
            KeyCode::PageUp if !self.transcript_area.is_empty() => {
                self.transcript
                    .scroll(-isize::try_from(self.transcript_area.height.max(1)).unwrap_or(1));
                None
            }
            KeyCode::PageDown if !self.transcript_area.is_empty() => {
                self.transcript
                    .scroll(isize::try_from(self.transcript_area.height.max(1)).unwrap_or(1));
                None
            }
            _ => None,
        };
        if let Some(edit) = edit {
            self.edit(edit);
        }
    }

    fn edit(&mut self, edit: Edit) {
        match self.composer.update(edit) {
            Err(error) => {
                self.edit_error = Some(if self.replay {
                    format!("Input rejected: {error}")
                } else {
                    i18n::edit_error(&error)
                })
            }
            Ok(true) => self.edit_error = None,
            Ok(false) => {}
        }
    }

    fn status_text(&self) -> &str {
        self.edit_error.as_deref().unwrap_or(self.status.as_str())
    }

    fn set_status(&mut self, message: impl Into<String>) {
        // A new operational notice supersedes an older transient edit failure.
        self.status = message.into();
        self.edit_error = None;
    }

    fn prepare_layout(&mut self, area: Rect) -> text::ComposerLayout {
        self.layout_area = area;
        let small = area.width < 24 || area.height < 7;
        self.composer_width = if small {
            area.width
        } else {
            area.width.saturating_sub(2)
        };
        let layout = text::composer_layout(
            self.composer.text(),
            self.composer.cursor(),
            self.composer_width,
        );
        if small {
            self.focus = Focus::Composer;
            self.transcript_area = Rect::default();
            let header_height = if area.height >= 4 {
                2
            } else {
                u16::from(area.height > 1)
            };
            let footer_height = u16::from(area.height > 2);
            self.composer_area = Rect::new(
                area.x,
                area.y + header_height,
                area.width,
                area.height.saturating_sub(header_height + footer_height),
            )
            .intersection(area);
        } else {
            let draft_height = u16::try_from(layout.lines.len())
                .unwrap_or(u16::MAX)
                .clamp(1, 6)
                .min(area.height.saturating_sub(6))
                .max(1)
                + 2;
            let body_height = area.height.saturating_sub(draft_height + 3);
            self.transcript_area = Rect::new(area.x, area.y + 1, area.width, body_height);
            self.composer_area =
                Rect::new(area.x, area.y + 1 + body_height, area.width, draft_height);
            self.transcript
                .resize(self.transcript_area.width, self.transcript_area.height);
        }
        layout
    }

    fn input_warning(&self) -> Option<&'static str> {
        match &self.input_capabilities {
            Some(capabilities)
                if !self.replay
                    && (!capabilities.bracketed_paste || !capabilities.enhanced_keys) =>
            {
                Some(i18n::CHAT_INPUT_LIMITS)
            }
            Some(capabilities) if !capabilities.bracketed_paste => {
                Some("No paste batching · use small inputs · F1 help")
            }
            Some(capabilities) if !capabilities.enhanced_keys => {
                Some("Enhanced keys unavailable · F1 for input limits")
            }
            _ => None,
        }
    }

    /// Draws a complete frame. Very small terminals may need resizing to show
    /// help or permission actions; hidden affirmative actions are disabled.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let draft_layout = self.prepare_layout(area);
        if area.width < 24 || area.height < 7 {
            self.render_small(frame, area, &draft_layout);
        } else {
            let reading = match self.transcript.reading() {
                Reading::FollowTail => "LIVE",
                Reading::Anchored(_) => "READING",
            };
            frame.render_widget(
                Paragraph::new(if !self.replay {
                    format!(
                        "{}{}{}",
                        self.companion_badge(),
                        if let Some(host) = &self.host {
                            if host.ready && !host.interactions.is_empty() {
                                format!(
                                    "Maka · 有待处理请求（{} 项，逐个处理）· F3",
                                    host.interactions.len()
                                )
                            } else if host.ready {
                                "Maka · 已连接 TS Host".into()
                            } else {
                                "Maka · Host 未连接".into()
                            }
                        } else {
                            i18n::CHAT_TITLE.into()
                        },
                        if self.input_warning().is_some() {
                            " [INPUT!]"
                        } else {
                            ""
                        }
                    )
                } else {
                    format!(
                        "{}{reading} +{}{} · DEMO / NO HOST",
                        self.companion_badge(),
                        self.transcript.unread_updates(),
                        if self.input_warning().is_some() {
                            " [INPUT!]"
                        } else {
                            ""
                        },
                    )
                }),
                Rect::new(area.x, area.y, area.width, 1),
            );
            let rows: Vec<Line<'_>> = self
                .transcript
                .visible()
                .iter()
                .map(|row| {
                    let style = if row.heading {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    Line::styled(row.text.as_str(), style)
                })
                .collect();
            frame.render_widget(Paragraph::new(rows), self.transcript_area);
            let title = if self.focus == Focus::Composer {
                if self.replay {
                    "[FOCUS] Draft"
                } else {
                    i18n::CHAT_INPUT
                }
            } else {
                if self.replay {
                    "Draft · F6 to edit"
                } else {
                    i18n::CHAT_READ
                }
            };
            let border = Border::default().borders(Borders::ALL).title(title);
            let inner = border.inner(self.composer_area);
            frame.render_widget(border, self.composer_area);
            self.render_draft(frame, inner, &draft_layout);
            frame.render_widget(
                Paragraph::new(self.status_text()),
                Rect::new(area.x, area.bottom() - 2, area.width, 1),
            );
            frame.render_widget(
                Paragraph::new(if !self.replay {
                    if self.host.as_ref().is_some_and(|host| host.running) {
                        "/ 命令 · Enter 发送 · Shift+Enter 换行 · Ctrl+C 停止"
                    } else {
                        i18n::CHAT_HINT
                    }
                } else if self.streaming && self.focus == Focus::Transcript {
                    "[FOCUS] History ^C stop replay · F1 help · F4 latest · F6 edit"
                } else if self.streaming {
                    "^C stop replay · / commands · F1 help · F4 latest"
                } else if self.focus == Focus::Transcript {
                    "[FOCUS] History ^C exit · F1 help · F4 latest · F6 edit"
                } else {
                    "^C exit · / commands · Ctrl+P · Enter newline"
                }),
                Rect::new(area.x, area.bottom() - 1, area.width, 1),
            );
        }
        if let Some(overlay) = self.overlay {
            self.render_overlay(frame, area, overlay);
        }
        if let Some(commands) = &mut self.commands {
            commands.render(frame);
        }
        if self.interaction_open
            && let Some(panel) = &mut self.interaction
        {
            panel.render(frame, area);
        }
        self.geometry_published = true;
    }

    fn render_small(&self, frame: &mut Frame<'_>, area: Rect, layout: &text::ComposerLayout) {
        if area.height >= 4 {
            frame.render_widget(
                Paragraph::new(if !self.replay {
                    i18n::CHAT_INPUT.to_owned()
                } else if self.companion.is_some() {
                    format!(
                        "{}Draft{}",
                        self.companion_badge(),
                        if self.input_warning().is_some() {
                            " [INPUT!]"
                        } else {
                            ""
                        }
                    )
                } else if self.input_warning().is_some() {
                    "[FOCUS] Draft [INPUT!]".into()
                } else {
                    "[FOCUS] Draft · small".into()
                }),
                Rect::new(area.x, area.y, area.width, 1),
            );
        }
        if area.height > 1 {
            frame.render_widget(
                Paragraph::new(self.status_text()),
                Rect::new(area.x, area.y + u16::from(area.height >= 4), area.width, 1),
            );
        }
        self.render_draft(frame, self.composer_area, layout);
        if area.height > 2 {
            frame.render_widget(
                Paragraph::new(if self.host.as_ref().is_some_and(|host| host.running) {
                    "^C 停止 · F1 帮助"
                } else if self.replay && self.streaming {
                    "^C stop replay · F1 help"
                } else if self.replay {
                    "^C exit · F1 help"
                } else {
                    "^C 退出 · F1 帮助"
                }),
                Rect::new(area.x, area.bottom() - 1, area.width, 1),
            );
        }
    }

    fn render_draft(&self, frame: &mut Frame<'_>, area: Rect, layout: &text::ComposerLayout) {
        if area.is_empty() {
            return;
        }
        let current = layout.cursor_row;
        let top = current
            .saturating_add(1)
            .saturating_sub(usize::from(area.height));
        let display: Vec<Line<'_>> = layout
            .lines
            .iter()
            .skip(top)
            .take(usize::from(area.height))
            .map(|line| Line::raw(line.text.as_str()))
            .collect();
        frame.render_widget(Paragraph::new(display), area);
        if self.focus == Focus::Composer
            && self.overlay.is_none()
            && self.commands.is_none()
            && !self.interaction_open
        {
            let row = u16::try_from(current.saturating_sub(top))
                .unwrap_or(0)
                .min(area.height.saturating_sub(1));
            frame.set_cursor_position(Position::new(area.x + layout.cursor_column, area.y + row));
        }
    }

    fn render_overlay(&mut self, frame: &mut Frame<'_>, area: Rect, overlay: Overlay) {
        let permission_pending = match overlay {
            Overlay::Permission(key) => self.permission_is_pending(key),
            _ => false,
        };
        self.overlay_actions_visible =
            area.width >= if self.replay { 24 } else { 12 } && area.height >= 4;
        let (title, content) = match overlay {
            Overlay::Help if self.host.is_some() => ("帮助 · TS Host 会话", i18n::HOST_HELP),
            Overlay::Help if !self.replay => (i18n::CHAT_HELP_TITLE, i18n::CHAT_HELP),
            Overlay::Help if self.companion.is_some() => (
                "Help · LOCAL IPC / NO HOST",
                "F5: echo draft to local Companion (up to 4 KiB)\nEcho is NOT a Host submission. Draft stays local.\nOne request at a time; no automatic retry.\nF6: focus; PgUp/PgDn / mouse: read history\nF4: latest; Ctrl+T: all thinking\nEnter: newline; Ctrl+Z / Ctrl+Y: undo / redo\nF2 / Ctrl+C: pause / stop FIXTURE (not IPC)\nF3: demo permission; no real command runs\nEsc: close\nDraft is memory-only; exit closes the Companion.",
            ),
            Overlay::Help => (
                "Help · DEMO ONLY",
                "F6: switch keyboard focus\nPgUp/PgDn or mouse wheel: read history\nF4: latest; Ctrl+T: all thinking\nEnter: newline; F5: preview (NOT send)\nCtrl+Z / Ctrl+Y: undo / redo\nF2: replay pause; Ctrl+C: stop replay\nF3: review demo request (expires at tick 60)\nEsc: close\nOverlays consume keys; no background actions.\nDraft is memory-only. No Host is connected.",
            ),
            Overlay::Permission(_) if permission_pending => (
                "Demo permission · no Host",
                "A fixture asks to run a command.\nNothing will execute.\na: record allow; d / Enter: deny\nEsc: leave pending\nDefault: deny. Reported repeats are ignored.\nThis demo request expires at replay tick 60.",
            ),
            Overlay::Permission(_) => (
                "Demo request no longer active",
                "Expired, superseded, or already answered.\nNo answer can be recorded.\nEsc: return to draft",
            ),
        };
        let input_note = if overlay == Overlay::Help && !self.replay {
            match &self.input_capabilities {
                Some(capabilities) if !capabilities.bracketed_paste => {
                    "\n终端不支持粘贴分组，请使用短输入。\n无法区分普通按键与粘贴的字符。"
                }
                Some(capabilities) if !capabilities.enhanced_keys => {
                    "\n终端不支持增强按键，长按可能被识别为多次按下。"
                }
                _ => "",
            }
        } else if overlay == Overlay::Help {
            match &self.input_capabilities {
                Some(capabilities) if !capabilities.bracketed_paste => {
                    "\nPaste batching unavailable: use small inputs.\nRaw pasted characters cannot be identified as paste.\nEnhanced-key support may also be unavailable."
                }
                Some(capabilities) if !capabilities.enhanced_keys => {
                    "\nEnhanced keys unavailable: repeat events may be\nindistinguishable from presses. Verify your terminal."
                }
                Some(_) => "\nInput modes requested; emulator support needs testing.",
                None => "\nFixture input only; no terminal capability evidence.",
            }
        } else {
            ""
        };
        let width = area.width.min(72);
        let wrapped = text::wrap(&format!("{content}{input_note}"), width.saturating_sub(2));
        if (area.width < 50 && self.replay)
            || wrapped.len().saturating_add(2) > usize::from(area.height)
        {
            if !self.replay {
                // No affirmative action is accepted when the full copy is hidden.
                self.overlay_actions_visible = false;
                frame.render_widget(Clear, area);
                frame.render_widget(Paragraph::new(i18n::CHAT_RESIZE), area);
                return;
            }
            let content = if !self.overlay_actions_visible {
                "Resize to review\nEsc: back"
            } else {
                match overlay {
                    Overlay::Help => {
                        "F1 / Esc: close help\nResize for keys/limits\nCtrl+C exits after close"
                    }
                    Overlay::Permission(_) if permission_pending => {
                        "DEMO only: no execution\na: allow; Enter: deny\nEsc: leave pending\nExpires during replay"
                    }
                    Overlay::Permission(_) => {
                        "Request no longer active\nNo answer can be recorded\nEsc: back"
                    }
                }
            };
            frame.render_widget(Clear, area);
            frame.render_widget(Paragraph::new(content), area);
            return;
        }
        let height = u16::try_from(wrapped.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let border = Border::default()
            .borders(Borders::ALL)
            .title(title)
            .style(Style::default().fg(Color::Yellow));
        let inner = border.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(border, popup);
        frame.render_widget(
            Paragraph::new(
                wrapped
                    .into_iter()
                    .map(|line| Line::from(line.text))
                    .collect::<Vec<_>>(),
            ),
            inner,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn draw(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .chunks(usize::from(width.max(1)))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    #[test]
    fn host_interaction_keeps_focus_and_requires_correlated_receipt() {
        let mut app = App::host_session("session-1".into());
        app.host_event(HostEvent::Ready {
            session_id: "session-1".into(),
        });
        app.update(Event::Paste("尚未发送".into()));
        let request = HostInteraction {
            id: "permission-1".into(),
            kind: "sandbox_boundary".into(),
            title: "执行工具".into(),
            source: "Host".into(),
            detail: "完整的执行范围".into(),
            fields: Vec::new(),
        };
        assert!(app.host_event(HostEvent::Interactions {
            requests: vec![request.clone()]
        }));
        assert!(!app.interaction_open);
        app.update(key(KeyCode::F(3)));
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::Esc));
        assert!(!app.interaction_open);
        assert_eq!(app.composer.text(), "尚未发送");
        app.update(key(KeyCode::F(3)));
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::Enter));
        assert!(matches!(
            app.take_host_command(),
            Some(HostCommand::Answer {
                action: crate::host_ui::InteractionAction::Deny,
                ..
            })
        ));
        app.update(key(KeyCode::Esc));
        assert!(app.host.as_ref().unwrap().answering.is_some());
        assert!(app.host_event(HostEvent::Answered {
            id: request.id.clone(),
            accepted: false
        }));
        app.host_event(HostEvent::State {
            running: false,
            waiting: false,
        });
        assert!(app.status_text().contains("回答未提交"));
        app.host_event(HostEvent::Interactions {
            requests: Vec::new(),
        });
        app.answer_interaction(HostCommand::Answer {
            id: request.id,
            action: crate::host_ui::InteractionAction::Allow,
            values: Default::default(),
        });
        assert!(app.take_host_command().is_none());
    }

    #[test]
    fn host_send_requires_receipt_and_never_resubmits_pending_input() {
        let mut app = App::host_session("session-1".into());
        assert!(app.host_event(HostEvent::Ready {
            session_id: "session-1".into()
        }));
        app.update(Event::Paste("你好\nHost".into()));
        app.update(key(KeyCode::Enter));
        let Some(HostCommand::Send { revision, text }) = app.take_host_command() else {
            panic!("missing send");
        };
        assert_eq!(text, "你好\nHost");
        assert_eq!(app.composer.text(), text);
        app.update(key(KeyCode::Enter));
        assert!(app.take_host_command().is_none());
        assert!(app.host_event(HostEvent::Submitted {
            revision,
            accepted: true
        }));
        assert!(app.composer.text().is_empty());
    }

    #[test]
    fn host_receipt_preserves_new_edits_and_disconnect_disables_send() {
        let mut app = App::host_session("session-1".into());
        app.host_event(HostEvent::Ready {
            session_id: "session-1".into(),
        });
        app.update(Event::Paste("first".into()));
        app.update(key(KeyCode::Enter));
        let Some(HostCommand::Send { revision, .. }) = app.take_host_command() else {
            panic!("missing send");
        };
        app.update(Event::Paste(" edit".into()));
        app.host_event(HostEvent::Submitted {
            revision,
            accepted: true,
        });
        assert_eq!(app.composer.text(), "first edit");
        assert!(!app.host_event(HostEvent::Failed {}));
        app.update(key(KeyCode::Enter));
        assert!(app.take_host_command().is_none());
        assert_eq!(app.composer.text(), "first edit");
    }

    #[test]
    fn host_stop_waits_for_observed_idle_before_control_c_exits() {
        let mut app = App::host_session("session-1".into());
        app.host_event(HostEvent::Ready {
            session_id: "session-1".into(),
        });
        app.host_event(HostEvent::State {
            running: true,
            waiting: false,
        });
        let stop = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        app.update(stop.clone());
        assert!(matches!(app.take_host_command(), Some(HostCommand::Stop)));
        app.update(stop.clone());
        assert!(app.take_host_command().is_none());
        assert!(!app.should_quit());
        app.host_event(HostEvent::State {
            running: false,
            waiting: false,
        });
        app.update(stop.clone());
        assert!(
            !app.should_quit(),
            "idle alone cannot acknowledge the stop request"
        );
        app.host_event(HostEvent::Stopped {});
        app.update(stop);
        assert!(app.should_quit());
    }

    #[test]
    fn chat_enter_does_not_fake_a_send_and_shift_enter_adds_a_newline() {
        let mut app = App::chat_demo();
        app.update(Event::Paste("第一行\n第二行".into()));
        app.update(key(KeyCode::Enter));
        assert_eq!(app.composer.text(), "第一行\n第二行");
        assert!(app.status_text().contains("未连接 Host"));
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT,
        )));
        assert_eq!(app.composer.text(), "第一行\n第二行\n");
    }

    #[test]
    fn control_q_is_not_an_exit_alias_in_either_mode() {
        for mut app in [App::chat_demo(), App::demo().unwrap()] {
            app.update(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::CONTROL,
            )));
            assert!(!app.should_quit());
            assert!(app.overlay.is_none());
            assert!(app.composer.text().is_empty());
        }
    }

    #[test]
    fn replay_control_c_stops_before_a_separate_press_exits() {
        let mut app = App::demo().unwrap();
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(!app.is_streaming());
        assert!(!app.should_quit());
        app.update(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEventKind::Repeat,
        )));
        assert!(!app.should_quit());
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit());
    }

    #[test]
    fn idle_chat_control_c_exits_without_confirmation() {
        let mut app = App::chat_demo();
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit());
    }

    #[test]
    fn chat_control_c_exits_without_clearing_unsent_input() {
        let mut app = App::chat_demo();
        app.update(Event::Paste("还没发".into()));
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit());
        assert!(app.overlay.is_none());
        assert_eq!(app.composer.text(), "还没发");
    }

    #[test]
    fn chat_control_c_does_not_exit_from_repeat_or_extra_modifiers() {
        for event in [
            KeyEvent::new_with_kind(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
                KeyEventKind::Repeat,
            ),
            KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
        ] {
            let mut app = App::chat_demo();
            app.update(Event::Key(event));
            assert!(!app.should_quit());
        }
    }

    #[test]
    fn chat_help_consumes_control_c_without_exiting() {
        let mut app = App::chat_demo();
        app.update(key(KeyCode::F(1)));
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(!app.should_quit());
    }

    #[test]
    fn chat_surface_uses_input_not_draft_management_copy() {
        let mut app = App::chat_demo();
        let screen = draw(&mut app, 80, 24).join("\n");
        assert!(!screen.contains("草稿"));
        assert!(screen.contains("Ctrl+C"));
    }

    #[test]
    fn chinese_chat_help_keeps_input_at_narrow_sizes() {
        let mut app = App::chat_demo();
        app.update(Event::Paste("输入".into()));
        app.update(key(KeyCode::F(1)));
        let screen = draw(&mut app, 24, 7).join("\n").replace(' ', "");
        assert!(screen.contains("请放大窗口"));
        app.update(key(KeyCode::Esc));
        assert_eq!(app.composer.text(), "输入");
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit());
        assert_eq!(app.composer.text(), "输入");
    }

    #[test]
    fn command_navigation_repeats_but_confirmation_does_not() {
        let mut app = App::chat_demo();
        draw(&mut app, 24, 7);
        app.update(key(KeyCode::Char('/')));
        app.update(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Down,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )));
        let screen = draw(&mut app, 24, 7).join("\n");
        assert!(screen.contains("/latest"));
        app.update(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )));
        assert!(app.commands.is_some());
        app.update(key(KeyCode::Enter));
        assert!(app.commands.is_none());
    }

    #[test]
    fn command_mouse_click_runs_only_current_rendered_row() {
        let mut app = App::chat_demo();
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::Char('/')));
        draw(&mut app, 80, 24);
        let click = Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 14,
            row: 9,
            modifiers: KeyModifiers::NONE,
        });
        app.update(click.clone());
        assert_eq!(app.overlay, Some(Overlay::Help));
        app.update(key(KeyCode::Esc));
        app.update(key(KeyCode::Char('/')));
        draw(&mut app, 80, 24);
        app.update(Event::Resize(40, 12));
        app.update(click);
        assert!(app.overlay.is_none());
        assert!(app.commands.is_some());
    }

    #[test]
    fn chat_shell_does_not_generate_fixture_messages_or_requests() {
        let mut app = App::chat_demo();
        for _ in 0..130 {
            app.tick();
        }
        app.update(key(KeyCode::F(2)));
        app.tick();
        let screen = draw(&mut app, 80, 24).join("\n");
        assert!(!app.is_streaming());
        assert!(app.permission.is_none());
        assert!(!screen.contains("Thinking"));
        assert!(!screen.contains("FAILED"));
        assert!(screen.replace(' ', "").contains("未连接Host"));
    }

    #[test]
    fn command_search_preserves_draft_focus_and_reading() {
        let mut app = App::demo().unwrap();
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::PageUp));
        app.update(Event::Paste("keep 中文 👩‍👩‍👧‍👦".into()));
        let reading = app.transcript.reading();
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        )));
        for ch in "思考".chars() {
            app.update(key(KeyCode::Char(ch)));
        }
        let screen = draw(&mut app, 40, 12).join("\n");
        assert!(screen.contains("/thinking"));
        app.update(Event::Paste("/latest\n".into()));
        app.update(key(KeyCode::Esc));
        assert_eq!(app.composer.text(), "keep 中文 👩‍👩‍👧‍👦");
        assert_eq!(app.transcript.reading(), reading);
        assert_eq!(app.focus, Focus::Composer);
    }

    #[test]
    fn slash_paste_and_literal_escape_remain_text() {
        let mut app = App::demo().unwrap();
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::Char('/')));
        assert!(app.commands.is_some());
        app.update(key(KeyCode::Char('/')));
        assert!(app.commands.is_none());
        app.update(Event::Paste("tmp/test\n/help".into()));
        assert_eq!(app.composer.text(), "/tmp/test\n/help");
        let mut app = App::demo().unwrap();
        app.update(Event::Paste("/help".into()));
        assert!(app.commands.is_none());
        assert_eq!(app.composer.text(), "/help");
    }

    #[test]
    fn command_execution_uses_current_query_but_waits_for_resize_redraw() {
        let mut app = App::demo().unwrap();
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::Char('/')));
        for ch in "latest".chars() {
            app.update(key(KeyCode::Char(ch)));
        }
        app.update(key(KeyCode::Enter));
        assert!(app.commands.is_none());
        app.update(key(KeyCode::Char('/')));
        draw(&mut app, 80, 24);
        app.update(Event::Resize(1, 1));
        app.update(key(KeyCode::Enter));
        assert!(app.commands.is_some());
        draw(&mut app, 1, 1);
        app.update(key(KeyCode::Enter));
        assert!(app.commands.is_some());
        draw(&mut app, 24, 7);
        app.update(key(KeyCode::Enter));
        assert!(app.commands.is_none());
    }

    #[test]
    fn shortcut_accepts_typed_slash_identifier_and_reports_local_action() {
        let mut app = App::chat_demo();
        draw(&mut app, 80, 24);
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        )));
        for ch in "/thinking".chars() {
            app.update(key(KeyCode::Char(ch)));
        }
        app.update(key(KeyCode::Enter));
        assert!(app.commands.is_none());
        assert_eq!(app.status_text(), i18n::THINKING_EXPANDED);
        assert!(app.composer.text().is_empty());
    }

    #[test]
    fn meta_and_hyper_do_not_open_edit_or_confirm_commands() {
        for modifier in [KeyModifiers::META, KeyModifiers::HYPER] {
            let mut app = App::chat_demo();
            draw(&mut app, 80, 24);
            app.update(Event::Key(KeyEvent::new(KeyCode::Char('/'), modifier)));
            assert!(app.commands.is_none());
            assert!(app.composer.text().is_empty());
            app.update(key(KeyCode::Char('/')));
            app.update(Event::Key(KeyEvent::new(KeyCode::Char('x'), modifier)));
            app.update(Event::Key(KeyEvent::new(KeyCode::Enter, modifier)));
            app.update(Event::Key(KeyEvent::new(KeyCode::Esc, modifier)));
            assert!(app.commands.is_some());
            app.update(key(KeyCode::Enter));
            assert_eq!(app.overlay, Some(Overlay::Help));
        }
    }

    #[test]
    fn empty_chat_cannot_lose_editing_focus_to_history() {
        let mut app = App::chat_demo();
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::F(6)));
        app.update(key(KeyCode::Char('x')));
        assert_eq!(app.composer.text(), "x");
    }

    #[test]
    fn compact_palette_edits_and_pastes_query_without_touching_draft() {
        let mut app = App::chat_demo();
        draw(&mut app, 23, 6);
        app.update(key(KeyCode::Char('/')));
        draw(&mut app, 23, 6);
        app.update(Event::Paste("helx".into()));
        app.update(key(KeyCode::Left));
        app.update(key(KeyCode::Delete));
        app.update(key(KeyCode::Char('p')));
        assert!(draw(&mut app, 23, 6).join("\n").contains("/help"));
        app.update(key(KeyCode::Enter));
        assert_eq!(app.overlay, Some(Overlay::Help));
        assert!(app.composer.text().is_empty());
    }

    #[test]
    fn command_no_match_cannot_execute_or_leak_enter_into_draft() {
        let mut app = App::demo().unwrap();
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::Char('/')));
        for ch in "unknown".chars() {
            app.update(key(KeyCode::Char(ch)));
        }
        assert!(
            draw(&mut app, 40, 12)
                .join("\n")
                .replace(' ', "")
                .contains(i18n::COMMAND_EMPTY)
        );
        app.update(key(KeyCode::Enter));
        assert!(app.commands.is_some());
        assert!(app.composer.text().is_empty());
    }

    #[test]
    fn command_click_after_query_change_cannot_use_previous_rows() {
        let mut app = App::demo().unwrap();
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::Char('/')));
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::Char('x')));
        assert!(app.commands.as_ref().unwrap().click(15, 10).is_none());
    }

    #[test]
    fn compact_palette_accepts_fast_search_without_waiting_for_popup_redraw() {
        let mut app = App::chat_demo();
        draw(&mut app, 23, 6);
        app.update(key(KeyCode::Char('/')));
        for ch in "help".chars() {
            app.update(key(KeyCode::Char(ch)));
        }
        app.update(key(KeyCode::Enter));
        assert_eq!(app.overlay, Some(Overlay::Help));
    }

    #[test]
    fn echo_acknowledges_a_snapshot_without_clearing_later_edits_or_moving_reading() {
        let mut app = App::demo().unwrap();
        app.enable_companion();
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::PageUp));
        let anchor = app.transcript().reading();
        app.update(Event::Paste("first snapshot".into()));
        app.update(key(KeyCode::F(5)));
        assert!(!app.take_companion_echo());
        app.companion_feedback(CompanionFeedback::Ready);
        let revision = app.composer().revision();
        app.update(key(KeyCode::F(5)));
        assert!(app.take_companion_echo());
        app.update(Event::Paste(" plus new edits".into()));
        app.update(key(KeyCode::F(5)));
        assert!(!app.take_companion_echo());
        app.companion_feedback(CompanionFeedback::Echoed { bytes: 14 });
        assert_eq!(app.composer().text(), "first snapshot plus new edits");
        assert!(app.status_text().contains(&format!("draft r{revision}")));
        assert!(app.status_text().contains("NOT sent to Host"));
        assert_eq!(app.transcript().reading(), anchor);
        assert_eq!(app.focus(), Focus::Composer);
    }

    #[test]
    fn disconnected_companion_does_not_retry_and_remains_visible_after_other_notices() {
        let mut app = App::demo().unwrap();
        app.enable_companion();
        app.companion_feedback(CompanionFeedback::Ready);
        app.update(Event::Paste("keep this draft".into()));
        app.update(key(KeyCode::F(5)));
        app.companion_feedback(CompanionFeedback::Disconnected);
        assert!(!app.take_companion_echo());
        app.update(key(KeyCode::F(5)));
        assert!(!app.take_companion_echo());
        app.set_status("Unrelated replay notice");
        assert!(draw(&mut app, 24, 8)[0].starts_with("IPC OFF"));
        assert!(draw(&mut app, 23, 4)[0].starts_with("IPC OFF"));
        assert_eq!(app.composer().text(), "keep this draft");
    }

    #[test]
    fn echo_budget_rejection_and_modal_keys_do_not_lose_the_draft() {
        let mut app = App::demo().unwrap();
        app.enable_companion();
        app.companion_feedback(CompanionFeedback::Ready);
        app.update(Event::Paste("large local draft".into()));
        app.update(key(KeyCode::F(1)));
        app.update(key(KeyCode::F(5)));
        assert!(!app.take_companion_echo());
        app.update(key(KeyCode::Esc));
        app.update(key(KeyCode::F(5)));
        assert!(app.take_companion_echo());
        app.companion_feedback(CompanionFeedback::TooLarge);
        assert_eq!(app.composer().text(), "large local draft");
        assert!(app.status_text().contains("nothing sent"));
        app.update(key(KeyCode::F(5)));
        assert!(app.take_companion_echo());
    }

    #[test]
    fn reading_and_draft_survive_stream_resize_and_thinking_toggle() {
        let mut app = App::demo().unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        app.update(key(KeyCode::PageUp));
        let anchor = app.transcript().reading();
        app.update(Event::Paste("补充 /exit\r\ne\u{301} 👩‍👩‍👧‍👦".into()));
        app.tick();
        app.update(key(KeyCode::F(5)));
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        )));
        terminal.backend_mut().resize(40, 12);
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert_eq!(app.transcript().reading(), anchor);
        assert_eq!(app.composer().text(), "补充 /exit\ne\u{301} 👩‍👩‍👧‍👦");
        assert!(!app.should_quit());
        let screen = draw(&mut app, 40, 12);
        assert!(screen[0].starts_with("READING +1"));
        assert!(screen[1].starts_with("[+] Thinking"));
        assert!(screen.iter().any(|row| row.contains("[FOCUS] Draft")));
        assert!(screen.iter().any(|row| row.contains("/exit")));
        assert!(screen[11].starts_with("^C stop replay"));
        let composer_row = app.composer_area.y;
        app.tick();
        draw(&mut app, 40, 12);
        assert_eq!(app.composer_area.y, composer_row);
    }

    #[test]
    fn permission_arrival_does_not_take_focus_or_consume_paste_as_actions() {
        let mut app = App::demo().unwrap();
        for _ in 0..12 {
            app.tick();
        }
        app.update(key(KeyCode::Enter));
        assert_eq!(app.composer().text(), "\n");
        assert_eq!(app.focus(), Focus::Composer);
        app.update(key(KeyCode::F(3)));
        app.update(Event::Paste("a".into()));
        app.update(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )));
        assert_eq!(
            app.permission.as_ref().unwrap().state,
            PermissionState::Pending
        );
        app.update(key(KeyCode::Esc));
        assert_eq!(app.composer().text(), "\n");
    }

    #[test]
    fn idle_replay_control_c_exits_without_clearing_unsent_input() {
        let mut app = App::demo().unwrap();
        app.update(Event::Paste("keep me".into()));
        app.streaming = false;
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit());
        assert_eq!(app.composer().text(), "keep me");
    }

    #[test]
    fn narrow_wrapped_heading_navigation_changes_the_visible_top_row() {
        let mut app = App::demo().unwrap();
        draw(&mut app, 24, 12);
        app.update(key(KeyCode::F(6)));
        app.update(key(KeyCode::Home));
        let mut previous = draw(&mut app, 24, 12)[1].clone();
        for _ in 0..20 {
            app.update(key(KeyCode::Down));
            let screen = draw(&mut app, 24, 12);
            assert_ne!(screen[1], previous);
            previous = screen[1].clone();
            assert!(screen[0].starts_with("READING"));
            assert!(screen[11].starts_with("[FOCUS] History"));
        }
    }

    #[test]
    fn expired_permission_cannot_record_a_stale_answer_and_overlay_owns_keys() {
        let mut app = App::demo().unwrap();
        for _ in 0..12 {
            app.tick();
        }
        app.update(key(KeyCode::F(3)));
        let screen = draw(&mut app, 80, 24).join("\n");
        assert!(screen.contains("Default: deny"));
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
        )));
        assert!(!app.should_quit());
        assert!(app.is_streaming());
        assert_eq!(
            app.permission.as_ref().unwrap().state,
            PermissionState::Pending
        );
        for _ in 12..60 {
            app.tick();
        }
        let screen = draw(&mut app, 80, 24).join("\n");
        assert!(screen.contains("No answer can be recorded"));
        app.update(key(KeyCode::Char('a')));
        app.update(key(KeyCode::Enter));
        assert_eq!(
            app.permission.as_ref().unwrap().state,
            PermissionState::Expired
        );
        assert!(!app.status.contains("Demo answer:"));
    }

    #[test]
    fn superseded_request_does_not_answer_a_new_generation() {
        let mut app = App::demo().unwrap();
        for _ in 0..12 {
            app.tick();
        }
        app.update(key(KeyCode::F(3)));
        app.permission.as_mut().unwrap().key.generation += 1;
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::Char('a')));
        assert_eq!(
            app.permission.as_ref().unwrap().state,
            PermissionState::Pending
        );
        assert!(!app.status.contains("Demo answer:"));
    }

    #[test]
    fn small_terminal_exits_directly_without_clearing_input() {
        let mut app = App::demo().unwrap();
        app.streaming = false;
        app.update(Event::Paste("draft".into()));
        let screen = draw(&mut app, 10, 3);
        assert!(screen[1].contains("draft"));
        assert!(screen[2].starts_with("^C exit"));
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit());
        assert_eq!(app.composer().text(), "draft");
    }

    #[test]
    fn resize_revokes_permission_affirmative_action_before_next_frame() {
        let mut app = App::demo().unwrap();
        for _ in 0..12 {
            app.tick();
        }
        app.update(key(KeyCode::F(3)));
        draw(&mut app, 80, 24);
        app.update(Event::Resize(23, 6));
        app.update(key(KeyCode::Char('a')));
        assert_eq!(
            app.permission.as_ref().unwrap().state,
            PermissionState::Pending
        );
    }

    #[test]
    fn visual_navigation_uses_received_resize_before_the_next_frame() {
        let mut app = App::demo().unwrap();
        app.update(Event::Paste("a".repeat(60)));
        draw(&mut app, 80, 24);
        app.update(Event::Resize(40, 12));
        app.update(key(KeyCode::Up));
        assert_eq!(app.composer().cursor(), 22);
    }

    #[test]
    fn small_mode_never_focuses_or_navigates_invisible_history() {
        let mut app = App::demo().unwrap();
        draw(&mut app, 80, 24);
        app.update(key(KeyCode::PageUp));
        let anchor = app.transcript().reading();
        app.update(key(KeyCode::F(6)));
        assert_eq!(app.focus(), Focus::Transcript);
        app.update(Event::Resize(23, 6));
        app.update(key(KeyCode::Char('x')));
        app.update(key(KeyCode::F(6)));
        app.update(key(KeyCode::Char('y')));
        app.update(key(KeyCode::PageUp));
        app.update(key(KeyCode::PageDown));
        app.update(key(KeyCode::F(4)));
        let screen = draw(&mut app, 23, 6).join("\n");
        assert_eq!(app.focus(), Focus::Composer);
        assert_eq!(app.composer().text(), "xy");
        assert_eq!(app.transcript().reading(), anchor);
        assert!(screen.contains("[FOCUS] Draft"));
    }

    #[test]
    fn small_mode_shows_permission_arrival_expiry_and_rejected_edits() {
        let mut app = App::demo().unwrap();
        for _ in 0..12 {
            app.tick();
        }
        assert!(
            draw(&mut app, 23, 6)
                .join("\n")
                .contains("F3: request waiting")
        );
        for _ in 12..60 {
            app.tick();
        }
        assert!(
            draw(&mut app, 23, 6)
                .join("\n")
                .contains("F3: request expired")
        );
        app.update(Event::Paste("\u{1b}".into()));
        assert!(draw(&mut app, 23, 6).join("\n").contains("Input rejected:"));
    }

    #[test]
    fn help_reports_overflow_instead_of_hiding_input_limits() {
        for width in [50, 80] {
            let mut app = App::demo().unwrap();
            app.set_input_capabilities(false, false);
            app.update(key(KeyCode::F(1)));
            let screen = draw(&mut app, width, 12).join("\n");
            assert!(screen.contains("Resize for keys/limits"));
            assert!(screen.contains("Esc: close help"));
            let screen = draw(&mut app, 80, 24).join("\n");
            assert!(screen.contains("Paste batching unavailable"));
        }
    }

    #[test]
    fn successful_edit_clears_only_the_editor_rejection() {
        let mut app = App::demo().unwrap();
        for _ in 0..12 {
            app.tick();
        }
        app.update(Event::Paste("\u{1b}".into()));
        assert!(draw(&mut app, 23, 6).join("\n").contains("Input rejected:"));
        app.update(key(KeyCode::Char('x')));
        let screen = draw(&mut app, 23, 6).join("\n");
        assert!(!screen.contains("Input rejected:"));
        assert!(screen.contains("F3: request waiting"));
        assert_eq!(app.composer().text(), "x");
    }

    #[test]
    fn permission_transitions_are_not_hidden_by_an_old_editor_rejection() {
        for (width, height) in [(23, 6), (80, 24)] {
            let mut app = App::demo().unwrap();
            app.update(Event::Paste("\u{1b}".into()));
            for _ in 0..12 {
                app.tick();
            }
            assert!(
                draw(&mut app, width, height)
                    .join("\n")
                    .contains("F3: request waiting")
            );
            app.update(Event::Paste("\u{1b}".into()));
            for _ in 12..60 {
                app.tick();
            }
            assert!(
                draw(&mut app, width, height)
                    .join("\n")
                    .contains("F3: request expired")
            );
        }
    }

    #[test]
    fn stop_feedback_supersedes_a_previous_editor_rejection() {
        for (width, height) in [(23, 6), (80, 24)] {
            let mut app = App::demo().unwrap();
            app.update(Event::Paste("\u{1b}".into()));
            app.update(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            )));
            assert!(
                draw(&mut app, width, height)
                    .join("\n")
                    .contains("Replay stopped")
            );
            assert!(!app.is_streaming());
        }
    }

    #[test]
    fn input_capability_warning_does_not_hide_current_action_or_error() {
        let mut app = App::demo().unwrap();
        app.set_input_capabilities(false, false);
        let screen = draw(&mut app, 80, 24);
        assert!(screen[0].contains("[INPUT!]"));
        assert!(screen[22].contains("No paste batching"));
        for _ in 0..12 {
            app.tick();
        }
        assert!(draw(&mut app, 80, 24)[22].contains("F3: request waiting"));
        app.update(Event::Paste("\u{1b}".into()));
        let screen = draw(&mut app, 80, 24);
        assert!(screen[0].contains("[INPUT!]"));
        assert!(screen[22].contains("Input rejected:"));
    }

    #[test]
    fn answering_or_dismissing_an_overlay_does_not_leak_key_repeat_into_draft() {
        for code in [KeyCode::Char('a'), KeyCode::Char('d'), KeyCode::Enter] {
            let mut app = App::demo().unwrap();
            app.update(Event::Paste("keep".into()));
            for _ in 0..12 {
                app.tick();
            }
            app.update(key(KeyCode::F(3)));
            draw(&mut app, 80, 24);
            app.update(key(code));
            app.update(Event::Key(KeyEvent::new_with_kind(
                code,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )));
            assert_eq!(app.composer().text(), "keep");
            app.update(Event::Key(KeyEvent::new_with_kind(
                code,
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )));
            app.update(key(KeyCode::Char('a')));
            assert_eq!(app.composer().text(), "keepa");
        }
    }

    #[test]
    fn tiny_geometry_does_not_panic_or_drop_the_draft() {
        let mut app = App::demo().unwrap();
        app.update(Event::Paste("draft".into()));
        for (width, height) in [(0, 0), (1, 1), (10, 3), (23, 6), (24, 7), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
        }
        assert_eq!(app.composer().text(), "draft");
    }
}
