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

"""Invoked only by the isolated TS Host fixture; does not create a Host."""

import sys
import re
import time
from pathlib import Path
from pty_smoke import Session


def expect_compact(session, text, start=0):
    deadline = time.monotonic() + 5
    while True:
        emitted = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(session.output[start:]))
        if text.encode() in re.sub(rb"\s+", b"", emitted):
            return
        if time.monotonic() >= deadline or session.process.poll() is not None:
            raise AssertionError(f"Missing Host output {text!r}; tail={bytes(session.output[-1200:])!r}")
        session.pump()


def main():
    binary, node, script, root, session_id = sys.argv[1:]
    command = [binary, "--connect", node, script, root, session_id]
    session = Session(command)
    try:
        session.expect_text("已连接")
        session.send(b"ratatui-host-integration\r")
        # Ratatui can skip painting spaces already present in the terminal.
        expect_compact(session, "rendererloopareconnected.")
        emitted = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(session.output))
        assert emitted.index("用户".encode()) < emitted.index("助手".encode()), "user must precede assistant"
        session.pump(0.4)
        session.send(b"__e2e_hold_open__\r")
        expect_compact(session, "waitingforthetesttostop")
        start = session.send(b"\x03")
        # The short receipt notice can be superseded before the next redraw.
        expect_compact(session, "aborted", start)
        session.pump(0.4)
        assert session.process.poll() is None, "stop must not exit the TUI"
        session.send(b"unsent input\x03")
        session.completed()
    finally:
        session.close()

    # Reopening proves the displayed user message came back from Host storage.
    launcher = str(Path(script).with_name('dev.ts'))
    session = Session([node, launcher, '--root', root])
    try:
        session.expect_text('选择会话')
        session.send(b'1\r')
        session.expect_text("已连接")
        session.expect_text("ratatui-host-integration")
        session.send(b"\x03")
        session.completed()
    finally:
        session.close()
    print("Host PTY: send, streamed response, stop, persisted history, direct exit passed")


if __name__ == "__main__":
    main()
