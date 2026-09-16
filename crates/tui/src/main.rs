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

//! Explicit entry point for the Maka Ratatui M0 demonstration.

mod companion;
mod host;
mod session_host;
mod terminal;
mod wire;

use std::{
    error::Error,
    io::{self, IsTerminal},
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};

use companion::{Companion, Notice, SendError};
use crossterm::event;
use maka_tui::app::{App, CompanionFeedback};
use maka_tui::host_ui::HostEvent;
use ratatui::DefaultTerminal;
use session_host::SessionHost;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("maka-tui: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if let [flag, node, script, root, session] = arguments.as_slice()
        && flag == "--connect"
    {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("--connect 需要交互式终端".into());
        }
        let session_id = session
            .to_str()
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|ch| ch.is_ascii_alphanumeric() || b"_-".contains(&ch))
            })
            .ok_or("需要有效的已有会话 ID")?;
        if root.is_empty() {
            return Err("需要显式 state-root 路径".into());
        }
        let mut app = App::host_session(session_id.to_owned());
        let mut connection = Some(SessionHost::spawn(
            node,
            Path::new(script),
            Path::new(root),
            session,
        )?);
        let interaction = terminal::run(|terminal, capabilities| {
            app.set_input_capabilities(capabilities.bracketed_paste, capabilities.enhanced_keys);
            event_loop(terminal, &mut app, &mut None, &mut connection)
        });
        let cleanup = connection.map(SessionHost::close).transpose();
        match (interaction, cleanup) {
            (Err(error), Err(cleanup)) => {
                return Err(format!("{error}; Host 客户端清理失败：{cleanup}").into());
            }
            (Err(error), _) | (_, Err(error)) => return Err(error.into()),
            (Ok(()), Ok(_)) => {}
        }
        return Ok(());
    }
    if let [flag, registration] = arguments.as_slice()
        && flag == "--list-sessions"
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let catalog = runtime.block_on(host::read_session_page(Path::new(registration)))?;
        println!("{}", serde_json::to_string(&catalog)?);
        return Ok(());
    }
    let companion_input = match arguments.as_slice() {
        [] => {
            print_help();
            return Ok(());
        }
        [argument] if argument == "--help" || argument == "-h" => {
            print_help();
            return Ok(());
        }
        [argument] if argument == "--demo" || argument == "--demo-chat" => None,
        [argument, node, script]
            if argument == "--demo-companion" || argument == "--probe-companion" =>
        {
            Some((argument == "--probe-companion", node, Path::new(script)))
        }
        _ => {
            return Err(
                "use --demo-chat, --demo, --list-sessions REGISTRATION.json, --demo-companion NODE SCRIPT, --probe-companion NODE SCRIPT, or --help".into(),
            );
        }
    };
    if let Some((true, node, script)) = companion_input {
        return probe_companion(Companion::spawn(node, script)?);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("--demo requires an interactive terminal on stdin and stdout".into());
    }
    let mut app = if arguments
        .first()
        .is_some_and(|argument| argument == "--demo-chat")
    {
        App::chat_demo()
    } else {
        App::demo()?
    };
    let mut companion = companion_input
        .map(|(_, node, script)| Companion::spawn(node, script))
        .transpose()?;
    if companion.is_some() {
        app.enable_companion();
    }
    let interaction = terminal::run(|terminal, capabilities| {
        app.set_input_capabilities(capabilities.bracketed_paste, capabilities.enhanced_keys);
        event_loop(terminal, &mut app, &mut companion, &mut None)
    });
    // Terminal is restored before a bounded process shutdown can wait.
    let cleanup = companion.map(Companion::close).transpose();
    match (interaction, cleanup) {
        (Err(interaction), Err(cleanup)) => {
            return Err(format!("{interaction}; Companion cleanup: {cleanup}").into());
        }
        (Err(error), _) | (_, Err(error)) => return Err(error.into()),
        (Ok(()), Ok(_)) => {}
    }
    println!("Maka TUI demo closed. No Host was connected; demo drafts were not persisted.");
    Ok(())
}

fn print_help() {
    println!(
        "maka-tui --connect NODE SCRIPT ROOT SESSION_ID\n  使用固定版本的 session-host.ts 连接已有 TS Host 会话。仅执行可信 Node 和脚本。\n"
    );
    println!(
        "maka-tui --list-sessions REGISTRATION.json\n  Read one catalog page from an existing epoch-154 Host; no automatic start or upgrade.\n"
    );
    println!(
        "maka-tui --demo-chat\n  Quiet chat shell: / on an empty draft or Ctrl+P opens commands; // inserts /.\n"
    );
    println!(
        "maka-tui --demo\nmaka-tui --demo-companion NODE SCRIPT\nmaka-tui --probe-companion NODE SCRIPT\n\nExperimental Ratatui M0 replay. No Host, tools, real submissions, or disk-backed drafts.\nEnter inserts a newline; F5 previews, or echoes locally in explicit Companion mode.\nCompanion mode executes the supplied Node executable and script; use trusted matching source paths.\nThe noninteractive probe tests one Unicode echo and owned process cleanup.\nF1 opens help; Ctrl+C exits. Existing maka commands and the default TUI are unchanged."
    );
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    companion: &mut Option<Companion>,
    session_host: &mut Option<SessionHost>,
) -> io::Result<()> {
    let mut dirty = true;
    let mut next_draw = Instant::now();
    let mut next_tick = Instant::now() + Duration::from_millis(100);
    while !app.should_quit() {
        if let Some(host) = session_host {
            for _ in 0..8 {
                let Some(event) = host.poll() else {
                    break;
                };
                dirty = true;
                if !app.host_event(event) {
                    host.cancel();
                    break;
                }
            }
            if let Some(command) = app.take_host_command() {
                if host.send(command).is_err() {
                    app.host_event(HostEvent::Failed {});
                    host.cancel();
                }
                dirty = true;
            }
        }
        if let Some(companion) = companion {
            // A fixed per-turn budget keeps an IPC burst from starving keyboard input.
            for _ in 0..4 {
                let Some(notice) = companion.poll() else {
                    break;
                };
                app.companion_feedback(match notice {
                    Notice::Ready => CompanionFeedback::Ready,
                    Notice::Echoed { bytes, .. } => CompanionFeedback::Echoed { bytes },
                    Notice::Disconnected { .. } => CompanionFeedback::Disconnected,
                });
                dirty = true;
            }
            if app.take_companion_echo() {
                match companion.echo(app.composer().text()) {
                    Ok(_) => {}
                    Err(SendError::TooLarge) => app.companion_feedback(CompanionFeedback::TooLarge),
                    Err(_) => app.companion_feedback(CompanionFeedback::Disconnected),
                }
                dirty = true;
            }
        }
        let now = Instant::now();
        if app.is_streaming() && now >= next_tick {
            app.tick();
            dirty = true;
            next_tick = now + Duration::from_millis(100);
        }
        if dirty && now >= next_draw {
            terminal
                .draw(|frame| app.render(frame))
                .map_err(|error| input_output_error("draw frame", error))?;
            dirty = false;
            next_draw = Instant::now() + Duration::from_millis(34);
        }
        let mut wait = Duration::from_millis(250);
        if companion.is_some() || session_host.is_some() {
            // Pipe arrivals do not read/wake the TTY. A bounded polling opportunity
            // keeps feedback timely without repainting an unchanged frame.
            wait = wait.min(Duration::from_millis(16));
        }
        if dirty {
            wait = wait.min(next_draw.saturating_duration_since(Instant::now()));
        }
        if app.is_streaming() {
            wait = wait.min(next_tick.saturating_duration_since(Instant::now()));
        }
        // All poll/read calls stay on this one thread. No competing stdin reader.
        if event::poll(wait).map_err(|error| input_output_error("poll terminal input", error))? {
            app.update(
                event::read().map_err(|error| input_output_error("read terminal input", error))?,
            );
            dirty = true;
        }
    }
    Ok(())
}

fn probe_companion(mut companion: Companion) -> Result<(), Box<dyn Error>> {
    let result = (|| -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + Duration::from_secs(7);
        while Instant::now() < deadline {
            match companion.poll() {
                Some(Notice::Ready) => {
                    println!("Companion ready (prototype pairing, no Host)");
                    companion.echo("本地 IPC / e\u{301} / 👩‍👩‍👧‍👦\nnot a Host submission")?;
                }
                Some(Notice::Echoed { request_id, bytes }) => {
                    println!("Echo verified: request {request_id}, {bytes} UTF-8 bytes");
                    return Ok(());
                }
                Some(Notice::Disconnected {
                    reason,
                    stderr_bytes,
                }) => {
                    return Err(format!("Companion disconnected: {reason}; {stderr_bytes} stderr bytes drained (contents withheld)").into());
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        Err("Companion probe deadline exceeded".into())
    })();
    let cleanup = companion.close();
    match (result, cleanup) {
        (Ok(()), Ok(())) => {
            println!("Companion closed");
            Ok(())
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(error), Err(cleanup)) => Err(format!("{error}; cleanup: {cleanup}").into()),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{context}: {source}")]
struct EventLoopError {
    context: &'static str,
    #[source]
    source: io::Error,
}

fn input_output_error(context: &'static str, source: io::Error) -> io::Error {
    io::Error::new(source.kind(), EventLoopError { context, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_errors_retain_phase_kind_and_source() {
        let error = input_output_error(
            "read terminal input",
            io::Error::new(io::ErrorKind::BrokenPipe, "fixture failure"),
        );
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(error.to_string(), "read terminal input: fixture failure");
        assert!(error.get_ref().unwrap().source().is_some());
    }
}
