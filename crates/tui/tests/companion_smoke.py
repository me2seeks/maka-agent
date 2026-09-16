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

"""Serial real Rust/Node IPC fault probes. Builds nothing; never connects Host."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

CRATE = Path(__file__).resolve().parents[1]
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", CRATE.parents[1] / "target"))
BINARY = TARGET / "debug" / ("maka-tui.exe" if sys.platform == "win32" else "maka-tui")
NODE = shutil.which("node")


def probe(script, scenario, expected_success, directory):
    pid_file = directory / f"{scenario}.pid"
    environment = dict(os.environ, MAKA_TUI_IPC_TEST_CASE=scenario, MAKA_TUI_IPC_TEST_PID=str(pid_file))
    start = time.monotonic()
    result = subprocess.run(
        [str(BINARY), "--probe-companion", NODE, str(script)],
        env=environment, capture_output=True, timeout=12, check=False,
    )
    elapsed = time.monotonic() - start
    assert (result.returncode == 0) == expected_success, (scenario, result.returncode, result.stdout, result.stderr)
    assert b"PRIVATE_DRAFT_SECRET" not in result.stdout + result.stderr, "child stderr reached user output"
    assert len(result.stdout) + len(result.stderr) < 4096, "unbounded diagnostic output"
    if expected_success:
        assert b"Echo verified:" in result.stdout and b"Companion closed" in result.stdout
    if pid_file.exists() and sys.platform == "linux":
        pid = int(pid_file.read_text())
        # Exact owned PID, not a broad name/process-tree search. A zombie is also a leak.
        assert not Path(f"/proc/{pid}").exists(), f"{scenario}: owned Companion {pid} was not reaped"
    print(f"PASS {scenario}: {elapsed:.2f}s")


def main():
    if NODE is None or not BINARY.is_file():
        raise SystemExit("Build the debug binary first and provide Node >=22.19 on PATH")
    with tempfile.TemporaryDirectory(prefix="maka-tui-ipc-") as name:
        directory = Path(name)
        probe(CRATE / "companion" / "main.ts", "matching", True, directory)
        fixture = CRATE / "tests" / "fixtures" / "companion-fault.mjs"
        for scenario in ["fragmented", "stderr_flood"]:
            probe(fixture, scenario, True, directory)
        for scenario in ["mismatch", "truncated", "wrong_id", "wrong_text", "handshake_stall", "response_stall", "shutdown_stall"]:
            probe(fixture, scenario, False, directory)


if __name__ == "__main__":
    main()
