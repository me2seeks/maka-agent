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

import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { readFile, writeFile } from 'node:fs/promises';
import { resolveRootControlNamespace } from '@maka/storage/root-authority';
import test from 'node:test';
import { assertHostBaseline } from '../companion/host-baseline.ts';
import { readHostSessionPage } from '../companion/host-reader.ts';
// Test-only reuse of the pinned Host's isolated SQLite/FakeBackend fixture.
// Production Companion imports only public client/protocol package exports.
import { withExecutionRoot } from '../../../packages/runtime-host/dist/__tests__/fixtures/execution-host-suite.js';

test('reads an isolated real Host catalog and leaves the shared Host running', {
  timeout: 60_000,
  skip:
    process.platform === 'win32' ? 'Host fixture SQLite shutdown not qualified on Windows' : false,
}, async () => {
  assertHostBaseline();
  await withExecutionRoot(async (fixture) => {
    const host = await fixture.startHost();
    try {
      const first = await readHostSessionPage(fixture.root);
      assert.equal(first.kind, 'page');
      assert.ok(first.sessions.some((session) => session.id === fixture.sessionId));
      const second = await readHostSessionPage(fixture.root);
      assert.equal(second.kind, 'page');
      assert.equal(second.hostEpoch, first.hostEpoch);
      const binary = fileURLToPath(new URL('../../../target/debug/maka-tui', import.meta.url));
      assert.equal(host.child.exitCode, null);
      if (process.platform === 'linux') {
        await promisify(execFile)(
          'python3',
          [
            fileURLToPath(new URL('./session_host_pty.py', import.meta.url)),
            binary,
            process.execPath,
            fileURLToPath(new URL('../companion/session-host.ts', import.meta.url)),
            fixture.root,
            fixture.sessionId,
          ],
          { timeout: 35_000, maxBuffer: 128 * 1024 },
        );
        assert.equal(host.child.exitCode, null);
      }
      const registration = join(
        resolveRootControlNamespace(),
        fixture.capability.rootId,
        'registration.json',
      );
      const native = await promisify(execFile)(binary, ['--list-sessions', registration], {
        timeout: 8_000,
        maxBuffer: 128 * 1024,
      });
      const catalog = JSON.parse(native.stdout);
      assert.equal(catalog.rootId, fixture.capability.rootId);
      assert.equal(catalog.hostEpoch, first.hostEpoch);
      assert.ok(catalog.page.sessions.some((session) => session.id === fixture.sessionId));
      const mismatched = join(fixture.base, 'mismatched-registration.json');
      const record = JSON.parse(await readFile(registration, 'utf8'));
      await writeFile(mismatched, JSON.stringify({ ...record, hostEpoch: 'stale-host-epoch' }));
      await assert.rejects(
        promisify(execFile)(binary, ['--list-sessions', mismatched], { timeout: 8_000 }),
        (error) =>
          error.code === 1 && error.stdout === '' && error.stderr.includes('身份或协议不匹配'),
      );
      assert.equal(host.child.exitCode, null);
    } finally {
      await fixture.stopHost(host);
    }
  });
});
