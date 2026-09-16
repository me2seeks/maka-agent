<!--
  Licensed to the Apache Software Foundation (ASF) under one
  or more contributor license agreements.  See the NOTICE file
  distributed with this work for additional information
  regarding copyright ownership.  The ASF licenses this file
  to you under the Apache License, Version 2.0 (the
  "License"); you may not use this file except in compliance
  with the License.  You may obtain a copy of the License at

      http://www.apache.org/licenses/LICENSE-2.0

  Unless required by applicable law or agreed to in writing,
  software distributed under the License is distributed on an
  "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
  KIND, either express or implied.  See the License for the
  specific language governing permissions and limitations
  under the License.
-->

[简体中文](./README.zh-CN.md)

# Maka TUI: M0 interaction prototype

Can a user keep reading old thinking while editing input and receiving new output? This Cargo workspace member provides a standalone demo and an explicit TS Host session mode. Neither replaces the current TUI.

Status: **Current source prototype; M0 remains incomplete**. The sections below distinguish implemented entry points from pending capabilities.

## Run without changing the default TUI

`npm run dev:maka-tui` now starts the isolated real-model Host described below instead of opening the demo; use `-- --help` for other modes. For the UI-only demo, run `npm run dev:maka-tui:demo`. The demo requires Rust 1.98.0 and Cargo dependencies, but not an npm dependency installation. Alternatively:

```sh
cd crates/tui
cargo run --locked -j 1 -- --demo-chat
```

The demo requires interactive stdin and stdout. No arguments or `--help` print help without entering terminal mode. It does not contact a Host, run tools, send prompts, read credentials, or persist drafts. Exiting closes the demo immediately; its in-memory input is not persisted. Existing `maka` commands remain unchanged.

## Try reading and editing together

`--demo-chat` (also the `npm run dev:maka-tui:demo` entry) opens a quiet chat shell without automatic replay messages or permission fixtures. Enter requests sending, but the disconnected demo retains the input and reports that sending is unavailable. Shift+Enter or Ctrl+J inserts a newline. Preview neither sends nor saves input. Use `--demo` for the legacy stress replay fixture below.

Type `/` in an empty draft, or Ctrl+P with any draft, to open the command list. Search English names or Chinese keywords, navigate with arrow keys, and select with Enter or a left click. Esc returns to the unchanged draft and reading position. Type `//` to begin a literal path in an empty draft. Pasted slash text never executes commands; pasting inside the overlay cannot change the draft. The list uses Ratatui `List/ListState` and only offers implemented help, latest, thinking, and local preview actions; there are no placeholder Session, Settings, or plugin actions.

The new shell and command list default to Simplified Chinese, with copy in `src/i18n.rs`. Command identities stay stable; legacy replay/IPC diagnostics still include English and there is no locale switch yet. Search supports Left/Right, Home/End, Delete/Backspace and safe paste (line breaks/tabs become spaces, never actions), bounded to 128 bytes. The list supports wheel navigation. A compact query/result view works from 12 columns × 4 rows; smaller windows pause query editing while Esc remains available. Long queries scroll horizontally on grapheme boundaries.

The fixture contains five long thinking blocks, an independent failed-tool status, and a bounded stream. Page up, leave keyboard focus in Draft, paste a multiline message, and change the window width. The same source block remains the reading anchor; draft edits do not follow the stream. Ctrl+T folds all current and future thinking bodies while retaining the hidden body anchor.

| Action | Binding |
| --- | --- |
| Edit while browsing | PgUp/PgDn; mouse wheel over history does not change focus |
| Change keyboard focus | F6; Esc returns to Draft |
| Navigate focused history | Up/Down, j/k, PgUp/PgDn; Home goes to the beginning |
| Explicitly follow new output | F4, or End when history has focus |
| Expand/collapse all thinking | Ctrl+T |
| Newline; visual-row movement | Enter; Up/Down in Draft |
| Undo/redo one edit or whole paste | Ctrl+Z / Ctrl+Y |
| Report draft size, without sending | F5 |
| Pause/resume; stop fixture replay | F2; Ctrl+C |
| Review demo permission | F3 |
| Help; exit | F1; Ctrl+C (shown as ^C in compact hints) |

While the replay is running, Ctrl+C stops it; once idle, Ctrl+C exits immediately.
Ctrl+Q has no action. The legacy replay table is not the chat send-key contract.

Arriving at the last page through ordinary scrolling stays in READING. Only explicit latest navigation re-enables LIVE. “+N” counts content updates while reading, not unread messages.

Paste is text, including slash commands. CRLF is normalized; unsupported controls are rejected atomically. Movement/deletion respects complete graphemes; tabs use four-column stops. Visual movement and cursor placement share one layout. A full logical-line end gets an empty insertion row without adding a newline to the draft. Selection, copying and structured attachments are not implemented.

These paste/Repeat guarantees apply to events the backend identifies as such. If the backend rejects bracketed paste or enhanced keys as unsupported, that optional mode is skipped without releasing an unacquired mode. An INPUT! marker and F1 help explain the limitation: legacy character streams cannot prove paste boundaries or distinguish repeated presses. Other initialization errors remain fatal. Error notices clear after an accepted edit or a newer operational notice, so an old rejection cannot conceal a new request or stop result.

A demo request arrives at replay tick 12 and expires at tick 60. It does not take keyboard focus. F3 explicitly opens it; Enter denies, and only a plain press of “a” records a local allow answer. Expired or superseded requests cannot accept answers. Overlays consume input, including Ctrl+C: close with Esc before issuing a background action. This fixture is not proof of real Host permission safety.

Below 24 columns or 7 rows, the draft replaces history and owns keyboard focus; hidden history navigation is disabled. With at least four rows, focus, current status, draft and exit guidance remain visible. Below 24 columns or 4 rows, an overlay asks for a resize and disables affirmative permission actions. Receiving a resize revokes that action gate before the next frame. Esc still returns. At 1×1, complete instructions physically cannot fit. Resizing does not delete input data or change the stored reading anchor. Help uses an explicit resize prompt when its complete content cannot fit.

## Rust ownership and engineering conventions

### Connect to an existing TS Host session

A newer Host on the same machine does not prevent testing. Never point the older backend at its data directory or bypass compatibility checks. After preparing the TS build below, run from this worktree root:

```sh
npm run dev:maka-tui -- --isolated-real
```

This is the real-model entry. It creates/connects an independently marked production Host only at `crates/tui/.development/epoch154`, which is Git-ignored. On first use, choose the provider, base URL and model ID, then enter an API key locally without echo; the Host saves the connection. It does not read your existing Host's configuration or sessions. The isolated candidate disables ambient OpenAI, Anthropic and DeepSeek bootstrap credentials. It never silently selects the free model. Saving may query the chosen provider's model catalog; model generation starts only when you explicitly send in chat.

Later launches reuse the isolated connection and history. Exit retains data; a Host started by this launcher closes when the launcher exits. The directory contains credentials and chat data: do not share or commit it. Incompatible existing Hosts are rejected, not upgraded. Automated tests use FakeBackend, not real models; provider authentication and generation need local configuration and validation. Open pending tool approvals with F3.

For credential-free protocol/interaction tests instead:

```sh
npm run dev:maka-tui -- --isolated
```

This starts a temporary epoch-154 TS Host with FakeBackend, not a real model. Protocol, session storage and streamed messages go through the actual Host; there are no model calls, and existing connections, credentials and sessions are not copied. Normal exit closes this test Host and removes its temporary data without touching your existing Host. Currently verified on Linux; this is not a persistent real-model development environment.

For an already running, independently configured epoch-154 Host:

```sh
npm run dev:maka-tui -- --root /existing/state-root
npm run dev:maka-tui -- --root /existing/state-root --new
npm run dev:maka-tui -- --root /existing/state-root --session SESSION_ID
```

The first command offers paginated session selection before entering chat (number to select, n to create, p for the next page, q to exit). In-chat `/sessions` is not implemented. Creation uses the current working directory, the Host default model and ask permission mode; missing model configuration is rejected by the Host, not copied from global settings. Existing-Host mode never starts, upgrades, restarts or stops that Host. Incompatibility produces an error, not a demo fallback. Each development launch incrementally builds Rust with one job.

Prepare the pinned TS dependencies and build as described below, then run:

```sh
cargo build --locked -p maka-tui -j 1
target/debug/maka-tui --connect node crates/tui/companion/session-host.ts /explicit/state-root EXISTING_SESSION_ID
```

This entry only connects to an existing epoch-154 Host: no automatic activation,
upgrade, session creation or reconnect. Trust the supplied Node and script paths.
The pinned TS Client owns subscription, history paging and wire validation; Rust
owns terminal presentation and input. Shared Rust protocol crates still support
the native catalog probe, not a complete replacement for the TS subscription client.
Enter sends text; Shift+Enter/Ctrl+J inserts a newline. Input clears only after a
Host admission receipt and only if it has not been edited since submission.
Ctrl+C requests stop while running; a separate press after Host-observed idle exits.
Idle Ctrl+C exits immediately without a discard confirmation. Unsent input is
memory-only and is not saved on exit. Disconnection leaves unacknowledged command
outcomes unknown; there is no automatic resend.

The native `--connect` entry still selects one existing session explicitly; the development launcher provides startup selection and creation. In-chat session switching, attachments, plugin UI and automatic recovery remain pending.

### Permissions, Agent questions and forms

Incoming Host requests show a notice without taking chat input focus. F3 or `/requests` in the command list opens a request. Esc returns to chat and retains answers in process; it does not submit cancellation. Authorization defaults to deny. Approval requires scrolling to the end of the complete review before explicitly choosing it. Epoch-154 sandbox expansion and client-capability grants affect subsequent calls in the current session, so the button explicitly says “approve session scope,” not “allow once.” Forms support text, numbers, integers, booleans, single-select and multi-select; Agent questions also allow free text. Use arrows to move, Space to select, and Enter/Tab to advance, followed by a separate submission review. Shift+Enter/Ctrl+J inserts a newline in text; F2 cancels an answer even before required fields are filled. Action buttons support mouse clicks; options currently use the keyboard.

Answers require a Host query, validation and receipt. Pending submissions cannot repeat; expired requests are disabled. Interaction actions require at least 40×10 terminal cells. Input and answers are bounded; approval is unavailable when the full review cannot be presented. Legacy permission and unknown request kinds are explicitly unsupported, never mapped to a generic approval.

MCP or plugins using the Host's existing declarative forms can share this renderer. This is not a TUI plugin runtime or proof of end-to-end MCP acceptance. The Companion does not register an MCP capability provider or load MCP servers itself. The Host still owns execution and persistence.

Multiple requests show a count and are handled sequentially; after submitting or cancelling the current request, use F3 for the next one. Tool output may use explicitly labelled truncation/control-character-cleaning previews, with originals retained by the Host. Authorization reviews never use truncated previews.

Send/display limits are 16 KiB per body and 128 blocks/256 KiB overall; excess history stops the
connection with a notice, not a claim of complete history. Isolated Linux Host +
FakeBackend PTY checks cover actual send/receive, question answers, sandbox denial and stop, not real models, IME or
cross-platform acceptance.

### Pinned TS Host read-only probe

The project now belongs to the root Cargo workspace, with a root lockfile and
toolchain. The shared protocol, transport and their runtime/presentation type
dependencies are sourced from commit `98f40e46a855bb0817e563a878424559b5a42b3f`;
`crates/upstream.json` records the exact source selection.

The native Rust client can read one catalog page without a Node Companion:

```sh
cargo build --locked -p maka-tui -j 1
target/debug/maka-tui --list-sessions /explicit/path/to/registration.json
```

This developer command verifies the live root, Host epoch, protocol and composition
against the explicitly supplied registration, and has a five-second total network
deadline. Output is private diagnostic JSON, not a public report. It does not
start/upgrade a Host or write session state. Interactive messaging uses the
separate `--connect` entry. Build the binary before `test:maka-tui-host`;
that core integration test exercises Rust against the isolated TS Host.
Only a ready Host is queried; other accepted lifecycle states return a not-ready
error without sending the query. The caller must trust the registration file and
its endpoint: matching identity fields does not authenticate ownership. Trusted
control-namespace discovery and endpoint ownership checks are not implemented.
Shared crates were imported source-only, not with their upstream conformance
test suites; this is not a claim of complete protocol coverage.

The separate developer probe uses TS Host source commit
`05d4d8ec45524f119c2dc1e4eae21826d6f90e7a` (protocol 0, compatibility epoch 154),
recorded in `host-baseline.json`. It does not change the Rust demo or its echo
protocol. Real messaging is enabled only in explicit `--connect` mode; local
echo is never a Host admission receipt.

Prepare the pinned backend locally, one command at a time (no full workspace build):

```sh
npm ci --ignore-scripts --no-audit --no-fund
node scripts/apply-dependency-patches.mjs
node scripts/sync-model-metadata.mjs
NODE_OPTIONS=--max-old-space-size=1536 node node_modules/typescript/bin/tsc -b packages/mcp packages/runtime-host
npm run typecheck:maka-tui-companion
npm run test:maka-tui-host
node crates/tui/companion/probe-host.ts --root /explicit/existing/state-root
```

The last command requires an explicitly selected, already running local Host.
It reads one bounded catalog page (at most 32 items), disconnects, then prints JSON.
The entire successful JSON (including names, IDs, cursor and hostEpoch) is private
diagnostic output; do not publish it unredacted. Catalog requests time out after
five seconds, and the probe awaits connection cleanup. It never starts, restarts, upgrades or stops that Host,
creates a root, submits messages, or marks sessions read. Tests start and stop
only their own temporary FakeBackend Host.

The probe checks backend tracked source against the pin and the imported
protocol constants against the manifest. TUI-only commits may advance the
branch without changing this backend baseline. This is **not** a complete
artifact freshness check or proof of the connected Host's exact build SHA;
the official client verifies protocol/epoch/composition compatibility and
connection identity. Rebuild after backend experiments; do not reuse another
worktree's `dist`. No automatic repinning occurs.

Following feat/runtime-host-rust, this is a member of the root Cargo workspace: edition 2024, Rust 1.98.0, checked-in Cargo.lock, Apache-2.0, and `publish = false`. Ratatui 0.30.2 and Crossterm 0.29.0 are pinned; optional Ratatui features are disabled. Production code forbids unsafe and requires public API documentation. Development/test debug information is disabled to reduce local artifact and build overhead.

The Composer owns atomic editing, revision and bounded undo. Transcript owns source anchors, wrapping and follow policy. App owns one input-routing policy and the deterministic fixture, not execution facts. The private terminal scope owns mode acquisition and cleanup. It releases successfully acquired modes once in reverse order, restores the prior panic hook, and preserves operation plus restoration errors. Normal completion, errors and unwind panic are supported paths; SIGTERM, suspend/resume, external-program handoff, abort and SIGKILL are not cleanup guarantees.

This editor is deliberately small: scalar-oriented deletion in evaluated controls did not establish the grapheme contract. Selection, IME/emulator behavior and editor reuse/adaptation remain M0 decisions; this prototype does not prove a complete custom editor is the best production choice.

## Validate with one local job

Run these commands **one at a time** from the repository root. Do not run a full npm workspace build for this crate.

```sh
npm run format:maka-tui:check
npm run test:maka-tui
npm run lint:maka-tui
npm run build:maka-tui
python3 crates/tui/tests/pty_smoke.py
cargo deny --manifest-path crates/tui/Cargo.toml --locked --config deny.toml check licenses sources
```

Build/test/lint entries specify `-j 1`; tests also specify `--test-threads=1`. One Cargo job is not a hard memory cap: monitor available memory and active swapping, and pause if pressure becomes sustained. The first dependency build plus regression run took about 32 seconds and reported 329 MiB maximum RSS with one job on this Linux machine; dependencies were already downloaded. This is neither whole-machine peak memory nor an input-latency measurement.

The PTY script builds nothing and uses Python's standard library on Linux. It exercises the existing debug binary and a separately ignored panic test artifact. TestBackend assertions check visible reading/focus/draft/exit text; PTY checks cover termios, protocol cleanup and real input delivery, not terminal-emulator appearance or IME. The dedicated source-admission workflow is defined for Linux/macOS/Windows, but defining it is not evidence those remote jobs passed.

Local verification on 2026-09-12: 76 ordinary Rust tests passed (64 library, 12 binary); the normally ignored panic test passed separately under the PTY harness. Both PTY scenarios, formatting, Clippy with warnings denied, debug build, and dependency license/source checks passed. Two independent gpt-5.6-sol high reviewers examined Rust/terminal ownership and UX concurrently without running builds. Findings were turned into regression tests and repairs; this is source-prototype review, not release approval.

## Limits and next gates

2026-09-16 chat-shell/command-list validation: 84 library and 42 binary tests passed; the ignored panic test passed separately through a PTY. Formatting, Clippy with warnings denied, and a debug build passed. Four Linux PTY scenarios cover the Chinese chat shell, command queries/literal slash, draft editing/exit, panic cleanup, and explicit protocol-error exit with terminal restoration when a fault Companion withholds acknowledgement. Cargo tasks ran serially with `-j 1`, tests with `--test-threads=1`. Two Luna max agents performed read-only code/UX reviews. This is not real Host, emulator/IME, or cross-platform release qualification.

If older artifacts make the PTY panic test ambiguous, set `MAKA_TUI_PANIC_TEST_BINARY` to the absolute `src/main.rs` test-binary path printed by the latest `cargo test`; do not guess or delete other artifacts.

Resident transcript admission is limited to 128 blocks, 16 KiB per body and 256 KiB total source text including titles; titles are at most 256 bytes. Rejected content is not silently truncated. The demo draft is limited to 64 KiB. Undo retains at most 128 transactions and 4 MiB of text snapshots; an individual transaction larger than that history budget severs undo history, a generic module behavior outside the demo's draft size.

Resident reflow and draft layout are synchronous and recomputed from bounded source, including per-grapheme cursor stops. Source-byte budgets do not equal actual heap usage or guarantee frame latency. This is not a paged/virtualized production reader or a 50 MiB history benchmark.

M0 still needs congestion/lifecycle qualification, copy/IME and actual emulator checks, cross-platform qualification and measured latency/resource budgets. Further milestones add session switching, attachments, disconnection recovery, Markdown/search, full command/workflow coverage and packaging. There is no dependency-notice artifact or released binary integration yet. Do not replace the default TUI or claim the rewrite complete from this prototype.
