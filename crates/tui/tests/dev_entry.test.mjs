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
import test from 'node:test';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { parseOptions } from '../companion/dev.ts';

test('development entry never guesses a state-root or falls back to the demo', () => {
  assert.deepEqual(parseOptions([]), { kind: 'isolated-real' });
  assert.deepEqual(parseOptions(['--help']), { kind: 'help' });
  assert.deepEqual(parseOptions(['--isolated']), { kind: 'isolated' });
  for (const args of [
    ['--root', 'relative'],
    ['--root'],
    ['--root', '/tmp/test', '--session'],
    ['--root', '/tmp/test', '--new', '--session', 'abc'],
    ['--isolated', '--root', '/tmp/test'],
    ['--root', '/tmp/test', '--start'],
  ])
    assert.throws(() => parseOptions(args));
  assert.deepEqual(parseOptions(['--root', '/tmp/test', '--new']), {
    kind: 'existing',
    root: '/tmp/test',
    session: undefined,
    create: true,
  });
});

test('isolated development launcher connects, exchanges messages and cleans up', {
  timeout: 40_000,
  skip: process.platform !== 'linux',
}, async () => {
  await promisify(execFile)(
    'python3',
    [
      fileURLToPath(new URL('./dev_entry_pty.py', import.meta.url)),
      process.execPath,
      fileURLToPath(new URL('../companion/dev.ts', import.meta.url)),
    ],
    { timeout: 35_000, maxBuffer: 64 * 1024 },
  );
});
