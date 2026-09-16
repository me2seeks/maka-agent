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
import { spawnSync } from 'node:child_process';
import { mkdtemp, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { setImmediate as nextImmediate } from 'node:timers/promises';
import { assertHostBaseline, HOST_BASELINE } from '../companion/host-baseline.ts';
import { readHostSessionPage } from '../companion/host-reader.ts';

const revision = 'sha256:' + 'a'.repeat(64);
const cursor = { revision, cursor: 'next-page' };

test('pinned source and built protocol agree', () => {
  assertHostBaseline();
  assert.equal(HOST_BASELINE.compatibilityEpoch, 154);
});

test('connects with pinned protocol/composition; reads one page and closes', async () => {
  let closed = 0;
  const requests = [];
  const result = await readHostSessionPage('/explicit/root', undefined, async (input) => {
    assert.deepEqual(input, {
      rootPath: '/explicit/root',
      protocol: { min: 0, max: 0 },
      compositionId: 'maka.interactive',
    });
    return {
      kind: 'connected',
      connection: {
        hostEpoch: 'epoch',
        request: async (...args) => {
          requests.push(args);
          return {
            kind: 'page',
            revision,
            sessions: [
              { id: 's1', name: '中文会话', isArchived: false, secret: 'not projected' },
              {
                kind: 'unsupported_legacy_record',
                id: 'old',
                revision: 1,
                reason: 'not_wire_representable',
              },
            ],
            nextCursor: cursor.cursor,
          };
        },
        close: async () => {
          await nextImmediate();
          closed += 1;
        },
      },
    };
  });
  assert.deepEqual(requests, [['session.catalog.query', { kind: 'list_start' }, 5_000]]);
  assert.equal(closed, 1);
  assert.deepEqual(result.sessions, [
    { kind: 'session', id: 's1', name: '中文会话', isArchived: false },
    { kind: 'unsupported_legacy_record', id: 'old' },
  ]);
  assert.deepEqual(result.nextCursor, cursor);
});

test('continuation preserves revision and closes on stale cursor without retry', async () => {
  let requests = 0;
  let closed = 0;
  await assert.rejects(
    readHostSessionPage('/explicit/root', cursor, async () => ({
      kind: 'connected',
      connection: {
        request: async (operation, input) => {
          requests += 1;
          assert.equal(operation, 'session.catalog.query');
          assert.deepEqual(input, { kind: 'list_continue', ...cursor });
          return {
            kind: 'revision_changed',
            expectedRevision: revision,
            actualRevision: 'sha256:' + 'b'.repeat(64),
          };
        },
        close: async () => {
          closed += 1;
        },
      },
    })),
  );
  assert.equal(requests, 1);
  assert.equal(closed, 1);
});

test('connection refusal is projected without registration details or restart', async () => {
  for (const kind of ['incompatible', 'upgrade_required', 'draining', 'unavailable']) {
    let calls = 0;
    const result = await readHostSessionPage('/explicit/root', undefined, async () => {
      calls += 1;
      return { kind, reason: 'not_registered', registration: { secret: 'redacted' } };
    });
    assert.deepEqual(
      result,
      kind === 'unavailable' ? { kind, reason: 'not_registered' } : { kind },
    );
    assert.equal(calls, 1);
  }
});

test('request failure always closes the connection and does not retry', async () => {
  let closed = 0;
  let requests = 0;
  await assert.rejects(
    readHostSessionPage('/explicit/root', undefined, async () => ({
      kind: 'connected',
      connection: {
        request: async () => {
          requests += 1;
          throw new Error('timeout');
        },
        close: async () => {
          closed += 1;
        },
      },
    })),
  );
  assert.equal(requests, 1);
  assert.equal(closed, 1);
});

test('empty root cannot trigger default-root discovery', async () => {
  await assert.rejects(
    readHostSessionPage('  ', undefined, async () => {
      assert.fail('must not connect');
    }),
  );
});

test('asynchronous cleanup failure cannot be reported as a successful page', async () => {
  await assert.rejects(
    readHostSessionPage('/explicit/root', undefined, async () => ({
      kind: 'connected',
      connection: {
        request: async () => ({ kind: 'page', revision, sessions: [], nextCursor: null }),
        close: async () => {
          await nextImmediate();
          throw new Error('cleanup failed');
        },
      },
    })),
    /cleanup failed/,
  );
});

test('real existing-Host client leaves an unmarked temporary directory untouched', async () => {
  const root = await mkdtemp(join(tmpdir(), 'maka-tui-host-read-'));
  try {
    const before = await readdir(root);
    await assert.rejects(readHostSessionPage(root), { code: 'root_unmarked' });
    assert.deepEqual(await readdir(root), before);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('CLI requires an explicit root and keeps errors free of private paths', async () => {
  const script = fileURLToPath(new URL('../companion/probe-host.ts', import.meta.url));
  const usage = spawnSync(process.execPath, [script], { encoding: 'utf8', timeout: 10_000 });
  assert.equal(usage.status, 2);
  assert.equal(usage.stdout, '');
  assert.match(usage.stderr, /用法/);
  const root = await mkdtemp(join(tmpdir(), 'maka-tui-private-path-'));
  try {
    const failed = spawnSync(process.execPath, [script, '--root', root], {
      encoding: 'utf8',
      timeout: 10_000,
    });
    assert.equal(failed.status, 1);
    assert.equal(failed.stdout, '');
    assert.match(failed.stderr, /只读 Host 探测失败/);
    assert.equal(failed.stderr.includes(root), false);
    assert.deepEqual(await readdir(root), []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
