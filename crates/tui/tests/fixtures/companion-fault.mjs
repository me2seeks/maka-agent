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

import { writeFileSync } from 'node:fs';
import { setImmediate } from 'node:timers/promises';
import { BUILD_ID, encode, packets, writePacket } from '../../companion/wire.ts';

// Test-only faults, selected only in the child's inherited environment.
const scenario = process.env.MAKA_TUI_IPC_TEST_CASE;
if (process.env.MAKA_TUI_IPC_TEST_PID)
  writeFileSync(process.env.MAKA_TUI_IPC_TEST_PID, String(process.pid));
for await (const packet of packets(process.stdin)) {
  if (scenario === 'handshake_stall') continue;
  if (packet.type === 'hello') {
    if (scenario === 'stderr_flood') {
      for (let index = 0; index < 512; index += 1) {
        await new Promise((resolve, reject) =>
          process.stderr.write('PRIVATE_DRAFT_SECRET'.padEnd(16 * 1024, '.'), (error) =>
            error ? reject(error) : resolve(),
          ),
        );
      }
    }
    const ready = { type: 'ready', build_id: scenario === 'mismatch' ? 'wrong-build' : BUILD_ID };
    if (scenario === 'truncated') {
      process.stdout.write(encode(ready).subarray(0, 10));
      break;
    }
    if (scenario === 'fragmented') {
      for (const byte of encode(ready)) {
        process.stdout.write(Buffer.from([byte]));
        await setImmediate();
      }
    } else {
      await writePacket(process.stdout, ready);
    }
  } else if (packet.type === 'echo') {
    if (scenario === 'response_stall') continue;
    await writePacket(process.stdout, {
      type: 'echoed',
      request_id: packet.request_id + (scenario === 'wrong_id' ? 1 : 0),
      text: scenario === 'wrong_text' ? 'not the snapshot' : packet.text,
    });
  } else if (packet.type === 'shutdown') {
    if (scenario === 'shutdown_stall') continue;
    await writePacket(process.stdout, { type: 'bye' });
    break;
  }
}
process.stdin.destroy();
