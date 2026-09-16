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

import { BUILD_ID, packets, writePacket } from './wire.ts';

// No Host imports, profile reads, subprocesses, files or terminal input owner.
// Node's built-in TypeScript stripping (>=22.19) keeps this M0 probe dependency-free.
async function run(): Promise<void> {
  if (process.stdin.isTTY || process.stdout.isTTY || process.argv.length !== 2) {
    throw new Error('Companion requires private IPC pipes and no extra arguments');
  }
  let ready = false;
  let lastRequest = 0;
  const handshakeTimeout = setTimeout(() => {
    process.stdin.destroy(new Error('IPC handshake timeout'));
  }, 3_000);
  try {
    for await (const packet of packets(process.stdin)) {
      if (!ready) {
        if (packet.type !== 'hello' || packet.build_id !== BUILD_ID) {
          throw new Error('IPC pairing mismatch; rebuild matching prototype sources');
        }
        await writePacket(process.stdout, { type: 'ready', build_id: BUILD_ID });
        clearTimeout(handshakeTimeout);
        ready = true;
      } else if (packet.type === 'echo') {
        if (packet.request_id <= lastRequest)
          throw new Error('Repeated or stale IPC request identity');
        lastRequest = packet.request_id;
        await writePacket(process.stdout, {
          type: 'echoed',
          request_id: packet.request_id,
          text: packet.text,
        });
      } else if (packet.type === 'shutdown') {
        await writePacket(process.stdout, { type: 'bye' });
        return;
      } else {
        throw new Error('Unexpected IPC packet in ready state');
      }
    }
    throw new Error('IPC closed without shutdown');
  } finally {
    clearTimeout(handshakeTimeout);
  }
}

// Even failures never write text to the protocol stream. These diagnostics are
// fixed messages: do not log payloads, credentials, drafts or arbitrary exceptions.
process.stdout.on('error', () => {
  process.exitCode = 1;
  process.stdin.destroy();
});
try {
  await run();
} catch {
  process.exitCode = 1;
  process.stderr.write('Maka TUI Companion: protocol or transport failure.\n');
} finally {
  process.stdin.destroy();
  process.stdout.end();
}
