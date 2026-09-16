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

// Developer-only, read-only JSON probe. Not the Rust Companion wire entry.
const args = process.argv.slice(2);
if (args.length !== 2 || args[0] !== '--root' || !args[1]?.trim()) {
  process.stderr.write(
    '用法：node crates/tui/companion/probe-host.ts --root <显式的 State Root 路径>\n',
  );
  process.exitCode = 2;
} else {
  try {
    // Keep missing/stale dependency failures inside the redacted error boundary.
    const { assertHostBaseline, HOST_BASELINE } = await import('./host-baseline.ts');
    assertHostBaseline();
    const { readHostSessionPage } = await import('./host-reader.ts');
    const result = await readHostSessionPage(args[1]);
    process.stdout.write(JSON.stringify({ sourceBaseline: HOST_BASELINE, result }) + '\n');
    if (result.kind !== 'page') process.exitCode = 1;
  } catch {
    // Host errors may contain paths or message content. Do not print raw errors.
    process.stderr.write(
      '只读 Host 探测失败：请检查锁定基线、构建产物和 Host 状态；未尝试启动或升级 Host。\n',
    );
    process.exitCode = 1;
  }
}
