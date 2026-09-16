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
import { spawn } from 'node:child_process';
import { Readable, Writable } from 'node:stream';
import test from 'node:test';
import { setImmediate as nextImmediate } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import {
  BUILD_ID,
  MAX_ECHO_BYTES,
  MAX_FRAME_BYTES,
  encode,
  packets,
  validate,
  writePacket,
} from '../companion/wire.ts';

const QUICK = { timeout: 1_000, concurrency: false };
const CHILD = { timeout: 8_000, concurrency: false };
const HELLO = { type: 'hello', build_id: BUILD_ID };
const SHUTDOWN = { type: 'shutdown' };
const FAILURE_DIAGNOSTIC = 'Maka TUI Companion: protocol or transport failure.';

function header(length, version = 1) {
  const bytes = Buffer.alloc(8);
  bytes.write('MKUI', 0, 'ascii');
  bytes.writeUInt16BE(version, 4);
  bytes.writeUInt16BE(length, 6);
  return bytes;
}

function rawPayload(payload) {
  return Buffer.concat([header(payload.length), payload]);
}

function rawPacket(packet) {
  return rawPayload(Buffer.from(JSON.stringify(packet), 'utf8'));
}

async function decodeChunks(chunks) {
  const decoded = [];
  // Object mode retains the exact chunk boundaries supplied by each test.
  for await (const packet of packets(Readable.from(chunks))) decoded.push(packet);
  return decoded;
}

async function within(promise, milliseconds, message) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(message)), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function runCompanion(t, input) {
  const child = spawn(
    process.execPath,
    [fileURLToPath(new URL('../companion/main.ts', import.meta.url))],
    {
      stdio: ['pipe', 'pipe', 'pipe'],
    },
  );
  const closed = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (code, signal) => resolve({ code, signal }));
  });
  // Install observers immediately so early process/pipe failures never become
  // unhandled rejections while another stream is being consumed.
  void closed.catch(() => undefined);
  child.stdin.on('error', () => undefined);
  const output = (async () => {
    const decoded = [];
    for await (const packet of packets(child.stdout)) {
      if (decoded.length >= 8) throw new Error('Companion exceeded test output packet budget');
      decoded.push(packet);
    }
    return decoded;
  })();
  const diagnostics = (async () => {
    const chunks = [];
    let bytes = 0;
    for await (const chunk of child.stderr) {
      bytes += chunk.length;
      if (bytes > 8 * 1024) throw new Error('Companion exceeded test diagnostic budget');
      chunks.push(chunk);
    }
    return Buffer.concat(chunks).toString('utf8');
  })();
  const completed = Promise.all([closed, output, diagnostics]);
  void completed.catch(() => undefined);
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
    child.stdin.destroy();
    child.stdout.destroy();
    child.stderr.destroy();
    await within(closed, 2_000, 'Companion did not close during test cleanup');
  });
  child.stdin.end(input);
  const [exit, decoded, stderr] = await within(
    completed,
    5_000,
    'Companion test deadline exceeded',
  );
  return { ...exit, decoded, stderr };
}

test('round-trips Unicode across byte-sized header and body fragments', QUICK, async () => {
  const packet = { type: 'echo', request_id: 1, text: '中文 👩🏽‍💻 e\u0301\nsecond line' };
  const bytes = encode(packet);
  const fragments = Array.from(bytes, (byte) => Buffer.from([byte]));
  assert.deepEqual(await decodeChunks(fragments), [packet]);
});

test('decodes several coalesced frames in their original order', QUICK, async () => {
  const echo = { type: 'echo', request_id: 0xffff_ffff, text: 'last valid identity' };
  assert.deepEqual(
    await decodeChunks([Buffer.concat([encode(HELLO), encode(echo), encode(SHUTDOWN)])]),
    [HELLO, echo, SHUTDOWN],
  );
});

test('encode writes the exact fixed-width big-endian envelope', QUICK, () => {
  const frame = encode(SHUTDOWN);
  const body = Buffer.from('{"type":"shutdown"}');
  assert.deepEqual(
    frame,
    Buffer.concat([Buffer.from([0x4d, 0x4b, 0x55, 0x49, 0, 1, 0, body.length]), body]),
  );
});

test('rejects an oversized header before allocating its announced body', QUICK, async (t) => {
  const oversized = MAX_FRAME_BYTES + 1;
  const frameHeader = header(oversized);
  const allocations = [];
  const original = Buffer.alloc;
  t.mock.method(Buffer, 'alloc', (size, ...arguments_) => {
    allocations.push(size);
    return Reflect.apply(original, Buffer, [size, ...arguments_]);
  });
  await assert.rejects(decodeChunks([frameHeader]), /IPC frame exceeds budget/);
  assert.equal(
    allocations.includes(oversized),
    false,
    'untrusted body length must not be allocated',
  );
});

test('rejects high-bit bytes that ASCII masking could mistake for MKUI', QUICK, async () => {
  const frame = encode(SHUTDOWN);
  for (let index = 0; index < 4; index += 1) frame[index] |= 0x80;
  await assert.rejects(decodeChunks([frame]), /IPC magic or wire version mismatch/);
});

test('rejects zero-length frames', QUICK, async () => {
  await assert.rejects(decodeChunks([header(0)]), /IPC frame exceeds budget/);
});

test('rejects unsupported wire versions before reading the body', QUICK, async () => {
  await assert.rejects(decodeChunks([header(16, 2)]), /IPC magic or wire version mismatch/);
});

test('rejects malformed UTF-8 rather than substituting replacement characters', QUICK, async () => {
  await assert.rejects(decodeChunks([rawPayload(Buffer.from([0xc3, 0x28]))]), TypeError);
});

test('rejects malformed JSON after validating the envelope', QUICK, async () => {
  await assert.rejects(decodeChunks([rawPayload(Buffer.from('{"type":'))]), SyntaxError);
});

for (const [name, packet, expected] of [
  ['unknown type', { type: 'execute', command: 'must not run' }, /Unknown IPC packet type/],
  [
    'extra hello field',
    { ...HELLO, credential: 'must not be accepted' },
    /missing or unknown fields/,
  ],
  ['missing hello field', { type: 'hello' }, /missing or unknown fields/],
  [
    'extra echo field',
    { type: 'echo', request_id: 1, text: 'ok', extra: true },
    /missing or unknown fields/,
  ],
  ['missing echo text', { type: 'echo', request_id: 1 }, /missing or unknown fields/],
  ['extra shutdown field', { type: 'shutdown', request_id: 1 }, /missing or unknown fields/],
  ['array payload', [], /payload must be an object/],
  ['null payload', null, /payload must be an object/],
  ['empty build identity', { type: 'hello', build_id: '' }, /Invalid IPC build identity/],
  ['non-ASCII build identity', { type: 'hello', build_id: '版本' }, /Invalid IPC build identity/],
  [
    'oversized build identity',
    { type: 'hello', build_id: 'x'.repeat(129) },
    /Invalid IPC build identity/,
  ],
]) {
  test(`rejects ${name}`, QUICK, async () => {
    await assert.rejects(decodeChunks([rawPacket(packet)]), expected);
  });
}

for (const requestId of [0, -1, 0x1_0000_0000, 1.5, '1', null]) {
  test(`rejects invalid request identity ${JSON.stringify(requestId)}`, QUICK, async () => {
    await assert.rejects(
      decodeChunks([rawPacket({ type: 'echo', request_id: requestId, text: 'ok' })]),
      /Invalid IPC request identity/,
    );
  });
}

for (const [name, text] of [
  ['high', String.fromCharCode(0xd800)],
  ['low', String.fromCharCode(0xdc00)],
]) {
  test(`rejects an escaped lone ${name} surrogate`, QUICK, async () => {
    await assert.rejects(
      decodeChunks([rawPacket({ type: 'echo', request_id: 1, text })]),
      /invalid Unicode/,
    );
  });
}

test('allows exactly 4096 UTF-8 bytes of multibyte text', QUICK, async () => {
  const packet = { type: 'echo', request_id: 1, text: '🙂'.repeat(MAX_ECHO_BYTES / 4) };
  assert.equal(Buffer.byteLength(packet.text), 4096);
  assert.deepEqual(await decodeChunks([encode(packet)]), [packet]);
});

test('rejects multibyte text exceeding 4096 bytes even below 4096 characters', QUICK, async () => {
  const packet = { type: 'echo', request_id: 1, text: `${'🙂'.repeat(MAX_ECHO_BYTES / 4)}a` };
  assert.equal(Buffer.byteLength(packet.text), 4097);
  assert.throws(() => validate(packet), /echo exceeds its text budget/);
  await assert.rejects(decodeChunks([rawPacket(packet)]), /echo exceeds its text budget/);
});

test('rejects every nonempty truncated header', QUICK, async () => {
  const frame = encode(SHUTDOWN);
  for (let length = 1; length < 8; length += 1) {
    await assert.rejects(decodeChunks([frame.subarray(0, length)]), /Truncated IPC frame/);
  }
});

test('rejects a complete header with a missing or partial body', QUICK, async () => {
  const frame = encode(SHUTDOWN);
  for (const length of [8, 9, frame.length - 1]) {
    await assert.rejects(decodeChunks([frame.subarray(0, length)]), /Truncated IPC frame/);
  }
});

test('rejects a truncated frame following a valid frame', QUICK, async () => {
  await assert.rejects(
    decodeChunks([Buffer.concat([encode(HELLO), encode(SHUTDOWN).subarray(0, 10)])]),
    /Truncated IPC frame/,
  );
});

test('accepts empty transport input without inventing a packet', QUICK, async () => {
  assert.deepEqual(await decodeChunks([]), []);
});

test('rejects text-mode stream chunks', QUICK, async () => {
  await assert.rejects(decodeChunks(['not a Buffer']), /IPC stream must contain bytes/);
});

test('writePacket waits for the controlled output callback', QUICK, async (t) => {
  let release;
  let received;
  const output = new Writable({
    highWaterMark: 1,
    write(chunk, _encoding, callback) {
      received = Buffer.from(chunk);
      release = callback;
    },
  });
  t.after(() => output.destroy());
  let settled = false;
  const writing = writePacket(output, HELLO).then(() => {
    settled = true;
  });
  void writing.catch(() => undefined);
  await nextImmediate();
  assert.equal(settled, false, 'a write under backpressure must remain pending');
  release();
  await writing;
  assert.deepEqual(received, encode(HELLO));
});

test('writePacket preserves a controlled output write failure', QUICK, async (t) => {
  const failure = new Error('controlled output failure');
  const output = new Writable({
    write(_chunk, _encoding, callback) {
      callback(failure);
    },
  });
  // Writable emits an error as well as rejecting the write callback.
  output.on('error', () => undefined);
  t.after(() => output.destroy());
  await assert.rejects(writePacket(output, HELLO), (error) => error === failure);
});

test('writePacket times out a stalled output and permits cleanup', {
  timeout: 5_000,
}, async (t) => {
  const output = new Writable({ write() {} });
  output.on('error', () => undefined);
  t.after(() => output.destroy());
  await assert.rejects(writePacket(output, HELLO), /IPC write timeout/);
});

test('real Companion pairs, echoes Unicode and shuts down cleanly', CHILD, async (t) => {
  const echo = { type: 'echo', request_id: 7, text: '私有草稿 👩🏽‍💻 e\u0301\nnext' };
  const result = await runCompanion(
    t,
    Buffer.concat([encode(HELLO), encode(echo), encode(SHUTDOWN)]),
  );
  assert.deepEqual(result.decoded, [
    { type: 'ready', build_id: BUILD_ID },
    { type: 'echoed', request_id: echo.request_id, text: echo.text },
    { type: 'bye' },
  ]);
  assert.equal(result.code, 0);
  assert.equal(result.signal, null);
  assert.equal(result.stderr.includes(FAILURE_DIAGNOSTIC), false);
  assert.equal(result.stderr.includes(echo.text), false);
});

test('real Companion refuses a mismatched source build identity', CHILD, async (t) => {
  const result = await runCompanion(t, encode({ type: 'hello', build_id: 'different-prototype' }));
  assert.deepEqual(result.decoded, []);
  assert.equal(result.code, 1);
  assert.equal(result.signal, null);
  assert.match(result.stderr, /Maka TUI Companion: protocol or transport failure\./);
});

test('real Companion refuses EOF before hello without publishing ready', CHILD, async (t) => {
  const result = await runCompanion(t, Buffer.alloc(0));
  assert.deepEqual(result.decoded, []);
  assert.equal(result.code, 1);
  assert.equal(result.signal, null);
  assert.match(result.stderr, /Maka TUI Companion: protocol or transport failure\./);
});

test('real Companion treats EOF after hello as failure, not clean shutdown', CHILD, async (t) => {
  const result = await runCompanion(t, encode(HELLO));
  assert.deepEqual(result.decoded, [{ type: 'ready', build_id: BUILD_ID }]);
  assert.equal(result.code, 1);
  assert.equal(result.signal, null);
  assert.match(result.stderr, /Maka TUI Companion: protocol or transport failure\./);
});

test(
  'real Companion rejects duplicate identities without echoing the second draft',
  CHILD,
  async (t) => {
    const first = { type: 'echo', request_id: 9, text: 'first private text' };
    const repeated = { type: 'echo', request_id: 9, text: 'second private text must stay private' };
    const result = await runCompanion(
      t,
      Buffer.concat([encode(HELLO), encode(first), encode(repeated)]),
    );
    assert.deepEqual(result.decoded, [
      { type: 'ready', build_id: BUILD_ID },
      { type: 'echoed', request_id: first.request_id, text: first.text },
    ]);
    assert.equal(result.code, 1);
    assert.equal(result.signal, null);
    assert.match(result.stderr, /Maka TUI Companion: protocol or transport failure\./);
    assert.equal(result.stderr.includes(first.text), false);
    assert.equal(result.stderr.includes(repeated.text), false);
  },
);

test('real Companion refuses echo before pairing', CHILD, async (t) => {
  const result = await runCompanion(t, encode({ type: 'echo', request_id: 1, text: 'not paired' }));
  assert.deepEqual(result.decoded, []);
  assert.equal(result.code, 1);
  assert.equal(result.signal, null);
});
