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

import type { Readable, Writable } from 'node:stream';
import { TextDecoder } from 'node:util';

// Source-prototype compatibility identity, NOT a released artifact hash.
export const BUILD_ID = 'maka-tui-m0-ipc-1';
export const MAX_FRAME_BYTES = 16 * 1024;
export const MAX_ECHO_BYTES = 4 * 1024;
const decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });

export type Packet =
  | { type: 'hello' | 'ready'; build_id: string }
  | { type: 'echo' | 'echoed'; request_id: number; text: string }
  | { type: 'shutdown' | 'bye' };

function record(value: unknown): asserts value is Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('IPC payload must be an object');
  }
}

function keys(value: Record<string, unknown>, allowed: string[]): void {
  if (
    Object.keys(value).length !== allowed.length ||
    allowed.some((key) => !Object.hasOwn(value, key))
  ) {
    throw new Error('IPC payload has missing or unknown fields');
  }
}

/** Validate a shallow, bounded schema. No arbitrary operation forwarding exists. */
export function validate(value: unknown): Packet {
  record(value);
  switch (value.type) {
    case 'hello':
    case 'ready':
      keys(value, ['type', 'build_id']);
      if (
        typeof value.build_id !== 'string' ||
        value.build_id.length > 128 ||
        !/^[\x20-\x7e]+$/.test(value.build_id)
      ) {
        throw new Error('Invalid IPC build identity');
      }
      return { type: value.type, build_id: value.build_id };
    case 'echo':
    case 'echoed':
      keys(value, ['type', 'request_id', 'text']);
      if (
        !Number.isInteger(value.request_id) ||
        (value.request_id as number) < 1 ||
        (value.request_id as number) > 0xffff_ffff
      ) {
        throw new Error('Invalid IPC request identity');
      }
      if (
        typeof value.text !== 'string' ||
        !value.text.isWellFormed() ||
        Buffer.byteLength(value.text) > MAX_ECHO_BYTES
      ) {
        throw new Error('IPC echo exceeds its text budget or has invalid Unicode');
      }
      return { type: value.type, request_id: value.request_id as number, text: value.text };
    case 'shutdown':
    case 'bye':
      keys(value, ['type']);
      return { type: value.type };
    default:
      throw new Error('Unknown IPC packet type');
  }
}

export function encode(value: Packet): Buffer {
  const payload = Buffer.from(JSON.stringify(validate(value)), 'utf8');
  if (payload.length === 0 || payload.length > MAX_FRAME_BYTES)
    throw new Error('IPC frame exceeds budget');
  const header = Buffer.alloc(8);
  header.write('MKUI', 0, 'ascii');
  header.writeUInt16BE(1, 4);
  header.writeUInt16BE(payload.length, 6);
  return Buffer.concat([header, payload]);
}

/** At most one 16 KiB body is assembled, including when the pipe is fragmented. */
export async function* packets(input: Readable): AsyncGenerator<Packet> {
  const header = Buffer.alloc(8);
  let headerBytes = 0;
  let body: Buffer | undefined;
  let bodyBytes = 0;
  for await (const chunk of input) {
    if (!Buffer.isBuffer(chunk)) throw new Error('IPC stream must contain bytes');
    let offset = 0;
    while (offset < chunk.length) {
      if (body === undefined) {
        const count = Math.min(8 - headerBytes, chunk.length - offset);
        chunk.copy(header, headerBytes, offset, offset + count);
        headerBytes += count;
        offset += count;
        if (headerBytes !== 8) continue;
        const length = header.readUInt16BE(6);
        if (!header.subarray(0, 4).equals(Buffer.from('MKUI')) || header.readUInt16BE(4) !== 1) {
          throw new Error('IPC magic or wire version mismatch');
        }
        if (length === 0 || length > MAX_FRAME_BYTES) throw new Error('IPC frame exceeds budget');
        body = Buffer.alloc(length);
      }
      const count = Math.min(body.length - bodyBytes, chunk.length - offset);
      chunk.copy(body, bodyBytes, offset, offset + count);
      bodyBytes += count;
      offset += count;
      if (bodyBytes === body.length) {
        const packet = validate(JSON.parse(decoder.decode(body)));
        body = undefined;
        headerBytes = 0;
        bodyBytes = 0;
        yield packet;
      }
    }
  }
  if (headerBytes !== 0 || body !== undefined) throw new Error('Truncated IPC frame');
}

/** Sequential writes bound buffering; a stalled peer cannot retain a write forever. */
export async function writePacket(output: Writable, packet: Packet): Promise<void> {
  const bytes = encode(packet);
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      new Promise<void>((resolve, reject) =>
        output.write(bytes, (error) => (error ? reject(error) : resolve())),
      ),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error('IPC write timeout')), 2_000);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}
