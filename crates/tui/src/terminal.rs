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

//! Scoped terminal ownership for the single-threaded M0 demonstration.
//!
//! The caller must exclusively own stdin/stdout and the process panic hook for
//! this non-nested scope, entered from normal shell terminal modes. SIGTERM,
//! suspend/resume and external-program handoff are not
//! supported. Cleanup attempts each successfully acquired mode once; an I/O
//! error does not prove whether a terminal consumed a partially written command.

use std::{
    error::Error,
    fmt,
    io::{self, Write},
    panic::{self, AssertUnwindSafe, PanicHookInfo},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{DefaultTerminal, backend::CrosstermBackend};

/// Input modes acquired for this scope, not an end-to-end terminal conformance test.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Capabilities {
    /// Whether enabling bracketed paste succeeded.
    pub(super) bracketed_paste: bool,
    /// Whether pushing keyboard-enhancement flags succeeded.
    pub(super) enhanced_keys: bool,
}

/// Runs one terminal scope, preserving both operation and restoration errors.
///
/// A panic is only caught to restore the previous hook, then resumed unchanged.
pub(super) fn run<T>(
    operation: impl FnOnce(&mut DefaultTerminal, Capabilities) -> io::Result<T>,
) -> io::Result<T> {
    let mut guard = TerminalGuard::install();
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let capabilities = guard.modes.acquire(apply)?;
        // Do not use ratatui::try_init: this scope owns modes and the panic hook.
        let mut terminal = DefaultTerminal::new(CrosstermBackend::new(io::stdout()))?;
        operation(&mut terminal, capabilities)
    }));
    let restoration = guard.finish();
    match result {
        Ok(operation) => finish_result(operation, restoration),
        Err(payload) => {
            if let Err(error) = restoration {
                report_restoration_error(&error);
            }
            panic::resume_unwind(payload)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Raw = 1,
    AlternateScreen = 2,
    BracketedPaste = 4,
    MouseCapture = 8,
    KeyboardEnhancement = 16,
    HiddenCursor = 32,
}

const MODES: [Mode; 6] = [
    Mode::Raw,
    Mode::AlternateScreen,
    Mode::BracketedPaste,
    Mode::MouseCapture,
    Mode::KeyboardEnhancement,
    Mode::HiddenCursor,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Acquire,
    Release,
}

#[derive(Clone, Default)]
struct Modes {
    acquired: Arc<AtomicU8>,
}

impl Modes {
    fn acquire(
        &self,
        mut apply: impl FnMut(Mode, Action) -> io::Result<()>,
    ) -> io::Result<Capabilities> {
        for mode in MODES {
            if let Err(error) = apply(mode, Action::Acquire) {
                if error.kind() == io::ErrorKind::Unsupported
                    && matches!(mode, Mode::BracketedPaste | Mode::KeyboardEnhancement)
                {
                    continue;
                }
                return Err(io::Error::new(
                    error.kind(),
                    AcquisitionError {
                        mode,
                        source: error,
                    },
                ));
            }
            self.acquired.fetch_or(mode as u8, Ordering::AcqRel);
        }
        let acquired = self.acquired.load(Ordering::Acquire);
        Ok(Capabilities {
            bracketed_paste: acquired & Mode::BracketedPaste as u8 != 0,
            enhanced_keys: acquired & Mode::KeyboardEnhancement as u8 != 0,
        })
    }

    fn release(
        &self,
        mut apply: impl FnMut(Mode, Action) -> io::Result<()>,
    ) -> Result<(), RestorationError> {
        // Claim the entire acquired set before I/O. Hook, finish and Drop must
        // never pop the same keyboard-enhancement stack entry a second time.
        let acquired = self.acquired.swap(0, Ordering::AcqRel);
        let mut failures = Vec::new();
        for mode in MODES.into_iter().rev() {
            if acquired & mode as u8 != 0
                && let Err(error) = apply(mode, Action::Release)
            {
                failures.push((mode, error));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(RestorationError(failures))
        }
    }
}

fn apply(mode: Mode, action: Action) -> io::Result<()> {
    match (mode, action) {
        (Mode::Raw, Action::Acquire) => enable_raw_mode(),
        (Mode::Raw, Action::Release) => disable_raw_mode(),
        (Mode::AlternateScreen, Action::Acquire) => execute!(io::stdout(), EnterAlternateScreen),
        (Mode::AlternateScreen, Action::Release) => execute!(io::stdout(), LeaveAlternateScreen),
        (Mode::BracketedPaste, Action::Acquire) => execute!(io::stdout(), EnableBracketedPaste),
        (Mode::BracketedPaste, Action::Release) => execute!(io::stdout(), DisableBracketedPaste),
        (Mode::MouseCapture, Action::Acquire) => execute!(io::stdout(), EnableMouseCapture),
        (Mode::MouseCapture, Action::Release) => execute!(io::stdout(), DisableMouseCapture),
        (Mode::KeyboardEnhancement, Action::Acquire) => execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        ),
        (Mode::KeyboardEnhancement, Action::Release) => {
            execute!(io::stdout(), PopKeyboardEnhancementFlags)
        }
        (Mode::HiddenCursor, Action::Acquire) => execute!(io::stdout(), Hide),
        (Mode::HiddenCursor, Action::Release) => execute!(io::stdout(), Show),
    }
}

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

struct TerminalGuard {
    modes: Modes,
    previous_hook: Option<Arc<PanicHook>>,
}

impl TerminalGuard {
    fn install() -> Self {
        let modes = Modes::default();
        let hook_modes = modes.clone();
        let previous_hook = Arc::new(panic::take_hook());
        let hook_previous = Arc::clone(&previous_hook);
        panic::set_hook(Box::new(move |information| {
            if let Err(error) = hook_modes.release(apply) {
                report_restoration_error(&error);
            }
            hook_previous(information);
        }));
        Self {
            modes,
            previous_hook: Some(previous_hook),
        }
    }

    fn finish(&mut self) -> Result<(), RestorationError> {
        let result = self.modes.release(apply);
        // set_hook/take_hook panic on an unwinding thread. run() restores the
        // hook after catch_unwind; Drop is only a best-effort fallback there.
        if !std::thread::panicking()
            && let Some(previous) = self.previous_hook.take()
        {
            drop(panic::take_hook());
            let previous = match Arc::try_unwrap(previous) {
                Ok(previous) => previous,
                Err(previous) => Box::new(move |information: &PanicHookInfo<'_>| {
                    previous(information);
                }),
            };
            panic::set_hook(previous);
        }
        result
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[derive(Debug, thiserror::Error)]
#[error("acquire {mode:?}: {source}")]
struct AcquisitionError {
    mode: Mode,
    #[source]
    source: io::Error,
}

#[derive(Debug)]
struct RestorationError(Vec<(Mode, io::Error)>);

impl fmt::Display for RestorationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, (mode, error)) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str("; ")?;
            }
            write!(formatter, "restore {mode:?}: {error}")?;
        }
        Ok(())
    }
}

impl Error for RestorationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.first().map(|(_, error)| error as &dyn Error)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{operation}; terminal restoration also failed: {restoration}")]
struct OperationAndRestorationError {
    #[source]
    operation: io::Error,
    restoration: RestorationError,
}

fn finish_result<T>(
    operation: io::Result<T>,
    restoration: Result<(), RestorationError>,
) -> io::Result<T> {
    match (operation, restoration) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(io::Error::other(error)),
        (Err(operation), Err(restoration)) => Err(io::Error::other(OperationAndRestorationError {
            operation,
            restoration,
        })),
    }
}

fn report_restoration_error(error: &RestorationError) {
    // A panic hook must not panic again if stderr is unavailable.
    let _ = writeln!(
        io::stderr(),
        "maka-tui: terminal restoration failed: {error}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_acquisition_reports_both_input_capabilities() {
        let capabilities = Modes::default().acquire(|_, _| Ok(())).unwrap();
        assert_eq!(
            capabilities,
            Capabilities {
                bracketed_paste: true,
                enhanced_keys: true,
            }
        );
    }

    #[test]
    fn unsupported_optional_mode_preserves_the_other_input_capability() {
        for unsupported in [Mode::BracketedPaste, Mode::KeyboardEnhancement] {
            let capabilities = Modes::default()
                .acquire(|mode, _| {
                    if mode == unsupported {
                        Err(io::ErrorKind::Unsupported.into())
                    } else {
                        Ok(())
                    }
                })
                .unwrap();
            assert_eq!(
                capabilities,
                Capabilities {
                    bracketed_paste: unsupported != Mode::BracketedPaste,
                    enhanced_keys: unsupported != Mode::KeyboardEnhancement,
                }
            );
        }
    }

    #[test]
    fn unsupported_optional_modes_are_not_released() {
        let modes = Modes::default();
        let capabilities = modes
            .acquire(|mode, _| {
                if matches!(mode, Mode::BracketedPaste | Mode::KeyboardEnhancement) {
                    Err(io::ErrorKind::Unsupported.into())
                } else {
                    Ok(())
                }
            })
            .unwrap();
        let mut released = Vec::new();
        modes
            .release(|mode, _| {
                released.push(mode);
                Ok(())
            })
            .unwrap();
        assert_eq!(capabilities, Capabilities::default());
        assert_eq!(
            released,
            [
                Mode::HiddenCursor,
                Mode::MouseCapture,
                Mode::AlternateScreen,
                Mode::Raw
            ]
        );
    }

    #[test]
    fn other_io_errors_from_optional_modes_remain_fatal() {
        for failed_mode in [Mode::BracketedPaste, Mode::KeyboardEnhancement] {
            let error = Modes::default()
                .acquire(|mode, _| {
                    if mode == failed_mode {
                        Err(io::ErrorKind::BrokenPipe.into())
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
    }

    #[test]
    fn unsupported_required_modes_remain_fatal() {
        for failed_mode in [
            Mode::Raw,
            Mode::AlternateScreen,
            Mode::MouseCapture,
            Mode::HiddenCursor,
        ] {
            let error = Modes::default()
                .acquire(|mode, _| {
                    if mode == failed_mode {
                        Err(io::ErrorKind::Unsupported.into())
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        }
    }

    #[test]
    fn acquisition_failure_releases_only_successful_prefix_in_reverse_order() {
        for failed_mode in MODES {
            let modes = Modes::default();
            let mut acquired = Vec::new();
            let result = modes.acquire(|mode, action| {
                assert_eq!(action, Action::Acquire);
                if mode == failed_mode {
                    return Err(io::Error::other("acquisition failed"));
                }
                acquired.push(mode);
                Ok(())
            });
            assert!(result.is_err());
            let error = result.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("acquire {failed_mode:?}"))
            );
            let mut released = Vec::new();
            modes
                .release(|mode, action| {
                    assert_eq!(action, Action::Release);
                    released.push(mode);
                    Ok(())
                })
                .unwrap();
            acquired.reverse();
            assert_eq!(released, acquired, "failed to acquire {failed_mode:?}");
        }
    }

    #[test]
    fn hook_and_drop_claim_each_acquired_mode_only_once() {
        let guard_modes = Modes::default();
        let hook_modes = guard_modes.clone();
        guard_modes.acquire(|_, _| Ok(())).unwrap();
        let mut released = Vec::new();
        hook_modes
            .release(|mode, _| {
                released.push(mode);
                Ok(())
            })
            .unwrap();
        guard_modes
            .release(|mode, _| {
                released.push(mode);
                Ok(())
            })
            .unwrap();
        assert_eq!(released, MODES.into_iter().rev().collect::<Vec<_>>());
    }

    #[test]
    fn restoration_failures_do_not_skip_raw_mode_or_retry_stack_pops() {
        let modes = Modes::default();
        modes.acquire(|_, _| Ok(())).unwrap();
        let mut attempted = Vec::new();
        let error = modes
            .release(|mode, _| {
                attempted.push(mode);
                Err(io::Error::other("restoration failed"))
            })
            .unwrap_err();
        modes
            .release(|mode, _| {
                attempted.push(mode);
                Ok(())
            })
            .unwrap();
        assert_eq!(attempted, MODES.into_iter().rev().collect::<Vec<_>>());
        assert_eq!(error.0.len(), MODES.len());
    }

    #[test]
    fn finish_preserves_operation_and_all_restoration_errors() {
        let restoration = RestorationError(vec![
            (Mode::KeyboardEnhancement, io::Error::other("pop failed")),
            (Mode::Raw, io::Error::other("raw failed")),
        ]);
        let result =
            finish_result::<()>(Err(io::Error::other("event read failed")), Err(restoration));
        assert_eq!(
            result.unwrap_err().to_string(),
            "event read failed; terminal restoration also failed: restore KeyboardEnhancement: pop failed; restore Raw: raw failed"
        );
    }

    #[test]
    fn finish_reports_restoration_failure_after_successful_operation() {
        let restoration = RestorationError(vec![(Mode::Raw, io::Error::other("raw failed"))]);
        let result = finish_result(Ok(()), Err(restoration));
        assert_eq!(result.unwrap_err().to_string(), "restore Raw: raw failed");
    }

    #[test]
    fn finish_preserves_original_error_kind_when_restoration_succeeds() {
        let result = finish_result::<()>(Err(io::ErrorKind::Interrupted.into()), Ok(()));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    #[ignore = "requires a real PTY; run through a PTY harness with --exact terminal::tests::panic_restores_real_terminal_and_previous_hook --ignored --nocapture --test-threads=1"]
    fn panic_restores_real_terminal_and_previous_hook() {
        use std::io::IsTerminal;

        assert!(io::stdin().is_terminal() && io::stdout().is_terminal());
        let outer_hook = panic::take_hook();
        let observed_panics = Arc::new(AtomicU8::new(0));
        let hook_panics = Arc::clone(&observed_panics);
        let expected_hook: PanicHook = Box::new(move |_| {
            hook_panics.fetch_add(1, Ordering::Relaxed);
        });
        let expected_pointer = std::ptr::from_ref(expected_hook.as_ref());
        panic::set_hook(expected_hook);

        let outcome = panic::catch_unwind(|| run::<()>(|_, _| panic!("terminal scope test panic")));
        let raw_mode = crossterm::terminal::is_raw_mode_enabled();
        let restored_hook = panic::take_hook();
        let restored_previous = std::ptr::eq(expected_pointer, restored_hook.as_ref());
        panic::set_hook(outer_hook);

        assert!(outcome.is_err(), "the original panic must keep unwinding");
        assert!(!raw_mode.unwrap(), "raw mode must be released after panic");
        assert!(
            restored_previous,
            "the exact previous hook must be restored"
        );
        assert_eq!(observed_panics.load(Ordering::Relaxed), 1);
    }
}
