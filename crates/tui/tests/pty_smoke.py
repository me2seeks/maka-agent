# Licensed to the Apache Software Foundation (ASF) under one
# or more contributor license agreements.  See the NOTICE file
# distributed with this work for additional information
# regarding copyright ownership.  The ASF licenses this file
# to you under the Apache License, Version 2.0 (the
# "License"); you may not use this file except in compliance
# with the License.  You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing,
# software distributed under the License is distributed on an
# "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
# KIND, either express or implied.  See the License for the
# specific language governing permissions and limitations
# under the License.

"""Linux PTY smoke checks using existing artifacts; never invokes a build.

Run cargo test and cargo build separately, with one job, before this script.
This checks PTY bytes and termios, not an emulator's visual rendering or IME.
"""

import errno
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import termios
import time


CRATE = Path(__file__).resolve().parents[1]
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", CRATE.parents[1] / "target"))
PANIC_TEST = "terminal::tests::panic_restores_real_terminal_and_previous_hook"


class Session:
    def __init__(self, command, extra_env=None):
        self.master, self.slave = pty.openpty()
        self.original_modes = termios.tcgetattr(self.slave)
        self.output = bytearray()
        self.process = None
        self.resize(80, 24)
        try:
            self.process = subprocess.Popen(
                command,
                stdin=self.slave,
                stdout=self.slave,
                stderr=self.slave,
                env={**os.environ, "TERM": "xterm-256color", **(extra_env or {})},
            )
        except BaseException:
            os.close(self.master)
            os.close(self.slave)
            raise

    def resize(self, width, height):
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
        if self.process is not None:
            self.process.send_signal(signal.SIGWINCH)

    def pump(self, duration=0.15):
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            ready, _, _ = select.select([self.master], [], [], max(0, deadline - time.monotonic()))
            if ready:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        return
                    raise
                if not chunk:
                    return
                self.output.extend(chunk)
                if len(self.output) > 1024 * 1024:
                    raise AssertionError("PTY output exceeded the smoke-test budget")

    def expect(self, content, start=0):
        deadline = time.monotonic() + 5
        while content not in self.output[start:]:
            if time.monotonic() >= deadline or self.process.poll() is not None:
                raise AssertionError(f"Missing PTY output {content!r}; tail={bytes(self.output[-800:])!r}")
            self.pump()

    def send(self, value):
        start = len(self.output)
        os.write(self.master, value)
        return start

    def expect_text(self, content, start=0):
        """Match emitted UTF-8 text across cursor moves, not an emulator screen."""
        deadline = time.monotonic() + 5
        while content.encode() not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(self.output[start:])):
            if time.monotonic() >= deadline or self.process.poll() is not None:
                raise AssertionError(f"Missing emitted text {content!r}; tail={bytes(self.output[-800:])!r}")
            self.pump()

    def completed(self, expected_exit=0):
        deadline = time.monotonic() + 5
        while self.process.poll() is None and time.monotonic() < deadline:
            self.pump()
        if self.process.poll() is None:
            raise AssertionError("PTY child did not exit")
        self.pump()
        assert self.process.returncode == expected_exit, bytes(self.output[-1500:])
        assert termios.tcgetattr(self.slave) == self.original_modes, "termios not restored"
        for sequence in [b"\x1b[>3u", b"\x1b[<1u", b"\x1b[?2004h", b"\x1b[?2004l",
                         b"\x1b[?1049h", b"\x1b[?1049l"]:
            assert self.output.count(sequence) == 1, (sequence, self.output.count(sequence))
        assert b"\x1b[?1000l" in self.output, "mouse capture not disabled"
        assert b"\x1b[?25h" in self.output, "cursor not restored"

    def close(self):
        if self.process.poll() is None:
            # Only the child created by this test is eligible for termination.
            self.process.terminate()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=2)
        os.close(self.master)
        os.close(self.slave)


def normal_exit():
    session = Session([str(TARGET / "debug" / "maka-tui"), "--demo"])
    try:
        session.expect(b"DEMO")
        session.send(b"\x1bOQ")  # F2: pause the deterministic stream.
        session.pump()
        assert not termios.tcgetattr(session.slave)[3] & termios.ICANON
        start = session.send(b"\x1b[200~" + "中文 /exit\r\nPTY-DRAFT".encode() + b"\x1b[201~")
        session.expect(b"PTY-DRAFT", start)
        session.send(b"\x1a")  # Ctrl+Z: undo the entire paste, not suspend.
        session.pump()
        start = session.send(b"\x19")  # Ctrl+Y: redo.
        session.expect(b"PTY-DRAFT", start)
        session.send(b"\x03")  # Ctrl+C exits without clearing in-memory input.
        session.completed()
    finally:
        session.close()


def chat_commands():
    session = Session([str(TARGET / "debug" / "maka-tui"), "--demo-chat"])
    try:
        session.expect_text("未连接")
        session.send(b"/")
        session.expect_text("命令")
        start = session.send(b"help")
        session.pump()
        session.send(b"\r")
        session.expect_text("Ctrl+C：退出", start)
        session.send(b"\x1b")
        session.pump()
        session.send(b"//tmp")
        session.expect(b"/tmp")
        session.send(b"\x10")  # Ctrl+P preserves the nonempty draft.
        session.pump()
        session.send(b"\x1b")
        session.pump()
        session.send(b"\x03")
        session.completed()
    finally:
        session.close()


def panic_exit():
    candidates = []
    explicit = os.environ.get("MAKA_TUI_PANIC_TEST_BINARY")
    possible = [Path(explicit)] if explicit else sorted((TARGET / "debug" / "deps").glob("maka_tui-*"))
    for candidate in possible:
        if candidate.is_file() and os.access(candidate, os.X_OK):
            listed = subprocess.run([str(candidate), "--list"], capture_output=True, timeout=5)
            if PANIC_TEST.encode() in listed.stdout:
                candidates.append(candidate)
    assert len(candidates) == 1, f"Set MAKA_TUI_PANIC_TEST_BINARY to the binary test artifact from the latest cargo test output; found {candidates}"
    session = Session([str(candidates[0]), "--exact", PANIC_TEST, "--ignored", "--nocapture", "--test-threads=1"])
    try:
        session.completed()
        assert b"1 passed" in session.output
    finally:
        session.close()


def editing_while_companion_waits():
    node = shutil.which("node")
    assert node, "Node >=22.19 is required for the optional Companion smoke"
    session = Session(
        [str(TARGET / "debug" / "maka-tui"), "--demo-companion", node,
         str(CRATE / "tests" / "fixtures" / "companion-fault.mjs")],
        {"MAKA_TUI_IPC_TEST_CASE": "response_stall"},
    )
    try:
        session.expect(b"ready")
        session.send(b"\x1bOQ")  # F2 pauses fixture output, not the IPC channel.
        session.send(b"\x1b[200~snapshot\x1b[201~")
        session.pump(0.05)
        session.send(b"\x1b[15~")  # F5 queues one local echo; child deliberately withholds it.
        session.expect(b"waiting")
        start = session.send(b"\x1b[200~EDIT-WHILE-WAITING\x1b[201~")
        session.expect(b"EDIT-WHILE-WAITING", start)
        session.send(b"\x03")
        # This fault peer never acknowledges the pending echo. Its bare bye
        # violates the shutdown contract: fail closed, but restore the terminal.
        session.completed(expected_exit=1)
        assert b"invalid Companion shutdown acknowledgement" in session.output
        assert b"PRIVATE_DRAFT_SECRET" not in session.output
    finally:
        session.close()


if __name__ == "__main__":
    chat_commands()
    print("PASS: Linux PTY quiet chat shell, searchable commands, literal slash and draft preservation")
    normal_exit()
    print("PASS: Linux PTY paste/undo/redo, safe exit, tiny resize, termios and mode restoration")
    panic_exit()
    print("PASS: Linux PTY panic cleanup, one keyboard push/pop, previous hook restoration")
    editing_while_companion_waits()
    print("PASS: Linux PTY continued editing, explicit protocol-error exit and terminal restoration while Companion reply is withheld")
