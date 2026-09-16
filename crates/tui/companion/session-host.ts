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

/**
 * A deliberately narrow, existing-Host-only session bridge.
 *
 * This file is a private NDJSON adapter for the Ratatui prototype.  It is not
 * a Runtime Host operation proxy: only text submit, stop of the observed root
 * Turn, and close are accepted from the peer.  The official TS Client owns
 * handshake identity, transcript digest verification, fragment assembly and
 * transcript paging; this process only projects those results into the small
 * Rust presentation protocol.
 */

import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { isAbsolute, resolve } from 'node:path';
import type { Readable, Writable } from 'node:stream';

import {
  decodeStoredMessage as decodePersistedStoredMessage,
  type StoredMessage,
} from '@maka/core/session';
import { markPersisted } from '@maka/core/persisted-value';
import {
  connectExistingRuntimeHost,
  RuntimeHostOperationError,
  type RuntimeHostConnection,
  type RuntimeHostSessionSubscription,
} from '@maka/runtime-host/client';
import {
  SESSION_TRANSCRIPT_BOOTSTRAP_MAX_BYTES,
  SESSION_TRANSCRIPT_PAGE_MAX_BYTES,
  type SessionContinuitySnapshot,
  type SubscriptionFrame,
} from '@maka/runtime-host/protocol';

import { assertHostBaseline, HOST_BASELINE } from './host-baseline.ts';

const BRIDGE_VERSION = 1 as const;
const MAX_LINE_BYTES = 384 * 1024;
const MAX_OUTPUT_QUEUE_ENTRIES = 8;
const MAX_OUTPUT_QUEUE_BYTES = 2 * MAX_LINE_BYTES;
const MAX_INPUT_QUEUE_ENTRIES = 4;
const MAX_SEND_TEXT_BYTES = 16 * 1024;
const MAX_HISTORY_BYTES = 256 * 1024;
const MAX_HISTORY_BLOCKS = 128;
const MAX_BLOCK_TEXT_BYTES = 16 * 1024;
const MAX_BLOCK_TITLE_BYTES = 256;
const MAX_BLOCK_ID_BYTES = 512;
const MAX_MESSAGE_BYTES = MAX_HISTORY_BYTES;
const BOOTSTRAP_TIMEOUT_MS = 14_000;
const COMMAND_TIMEOUT_MS = 15_000;
const CLOSE_TIMEOUT_MS = 2_000;
const FIXED_WAITING_NOTICE = '需要其他 Maka 客户端处理当前权限或交互请求';
const FIXED_REJECTED_NOTICE = 'Host 未接受该操作；输入已保留';

type BlockKind = 'user' | 'assistant' | 'thinking' | 'tool';

export interface HostBlock {
  readonly id: string;
  readonly kind: BlockKind;
  readonly title: string;
  readonly text: string;
}

type HostEvent =
  | { readonly kind: 'hello'; readonly version: typeof BRIDGE_VERSION }
  | { readonly kind: 'history'; readonly blocks: readonly HostBlock[] }
  | { readonly kind: 'state'; readonly running: boolean; readonly waiting: boolean }
  | { readonly kind: 'ready'; readonly session_id: string }
  | { readonly kind: 'upsert'; readonly block: HostBlock }
  | { readonly kind: 'submitted'; readonly revision: number; readonly accepted: boolean }
  | { readonly kind: 'stopped' }
  | { readonly kind: 'notice'; readonly text: string }
  | { readonly kind: 'failed' };

type HostCommand =
  | { readonly kind: 'send'; readonly revision: number; readonly text: string }
  | { readonly kind: 'stop' }
  | { readonly kind: 'close' };

const decodeStoredMessage = (value: unknown): StoredMessage =>
  decodePersistedStoredMessage(markPersisted<StoredMessage>(value));

class BridgeFailure extends Error {}

class ProjectionFailure extends BridgeFailure {}

function fixedFailure(): BridgeFailure {
  return new BridgeFailure('session bridge failure');
}

function byteLength(value: string): number {
  return Buffer.byteLength(value, 'utf8');
}

function hasForbiddenControl(value: string, allowLineBreaks: boolean): boolean {
  return [...value].some(
    (character) =>
      ((character.codePointAt(0) ?? 0) < 0x20 ||
        ((character.codePointAt(0) ?? 0) >= 0x7f && (character.codePointAt(0) ?? 0) <= 0x9f)) &&
      !(allowLineBreaks && (character === '\n' || character === '\t')),
  );
}

function assertSafeText(value: string, maxBytes: number, allowLineBreaks: boolean): void {
  if (!value.isWellFormed() || hasForbiddenControl(value, allowLineBreaks)) {
    throw new ProjectionFailure('presentation text contains unsupported characters');
  }
  if (byteLength(value) > maxBytes) {
    throw new ProjectionFailure('presentation text exceeds its bounded capacity');
  }
}

function assertBlock(block: HostBlock): void {
  if (
    block.id.length === 0 ||
    !block.id.isWellFormed() ||
    byteLength(block.id) > MAX_BLOCK_ID_BYTES ||
    !block.title.isWellFormed() ||
    byteLength(block.title) > MAX_BLOCK_TITLE_BYTES ||
    block.title.includes('\n') ||
    block.title.includes('\t')
  ) {
    throw new ProjectionFailure('presentation block identity or title is invalid');
  }
  assertSafeText(block.title, MAX_BLOCK_TITLE_BYTES, false);
  assertSafeText(block.text, MAX_BLOCK_TEXT_BYTES, true);
}

function serializeValue(value: unknown): string {
  if (typeof value === 'string') return value;
  try {
    const serialized = JSON.stringify(value);
    return serialized === undefined ? '' : serialized;
  } catch {
    throw new ProjectionFailure('stored message cannot be projected');
  }
}

function resultContentText(value: unknown): string {
  const record =
    value !== null && typeof value === 'object' && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : undefined;
  if (record !== undefined && record.kind === 'text' && typeof record.text === 'string') {
    return record.text;
  }
  return serializeValue(value);
}

function messageId(value: StoredMessage): string {
  const id = (value as { id?: unknown }).id;
  if (typeof id !== 'string' || id.length === 0) {
    throw new ProjectionFailure('stored message has no stable identity');
  }
  return id;
}

function messageBlocks(message: StoredMessage): HostBlock[] {
  const id = messageId(message);
  switch (message.type) {
    case 'user':
      return [
        {
          id,
          kind: 'user',
          title: '用户',
          text: message.displayText ?? message.text,
        },
      ];
    case 'assistant': {
      const blocks: HostBlock[] = [];
      const thinking = message.thinking?.text ?? '';
      if (thinking.length > 0) {
        blocks.push({ id: `${id}:thinking`, kind: 'thinking', title: '思考', text: thinking });
      }
      if (message.text.length > 0 || blocks.length === 0) {
        blocks.push({ id, kind: 'assistant', title: '助手', text: message.text });
      }
      return blocks;
    }
    case 'tool_call':
      return [
        {
          id,
          kind: 'tool',
          title: `工具：${message.displayName ?? message.toolName}`,
          text: message.intent ?? serializeValue(message.args),
        },
      ];
    case 'tool_result':
      return [
        {
          id,
          kind: 'tool',
          title: message.isError ? '工具结果：错误' : '工具结果',
          text: resultContentText(message.content),
        },
      ];
    case 'permission_decision':
      return [
        {
          id,
          kind: 'tool',
          title: `权限：${message.toolName}`,
          text: message.decision,
        },
      ];
    case 'token_usage': {
      const totals = [`输入 ${message.input}`, `输出 ${message.output}`];
      if (message.reasoning !== undefined) totals.push(`推理 ${message.reasoning}`);
      if (message.total !== undefined) totals.push(`总计 ${message.total}`);
      if (message.costUsd !== undefined) totals.push(`费用 ${message.costUsd}`);
      return [{ id, kind: 'tool', title: '用量', text: totals.join('，') }];
    }
    case 'turn_state':
      return [
        {
          id,
          kind: 'tool',
          title: `回合：${message.status}`,
          text: message.failureMessage ?? '',
        },
      ];
    case 'system_note':
      return [
        { id, kind: 'tool', title: `系统：${message.kind}`, text: serializeValue(message.data) },
      ];
    default: {
      const unsupported = message as unknown as { type?: unknown };
      return [
        {
          id,
          kind: 'tool',
          title: `消息：${typeof unsupported.type === 'string' ? unsupported.type : '未知'}`,
          text: serializeValue(message),
        },
      ];
    }
  }
}

class Projection {
  readonly #blocks = new Map<string, HostBlock>();
  readonly #order: string[] = [];
  #bytes = 0;

  applyMessage(message: StoredMessage, placement: 'append' | 'prepend' = 'append'): HostBlock[] {
    const changed: HostBlock[] = [];
    const blocks = messageBlocks(message);
    for (const block of placement === 'prepend' ? blocks.reverse() : blocks) {
      if (placement === 'prepend' ? this.prepend(block) : this.upsert(block)) changed.push(block);
    }
    return changed;
  }

  /**
   * The Host's bounded durable tail is paged in `older` order: the bootstrap
   * page contains the newest tail and each cursor page contains older rows.
   * Insert those rows at the front while retaining the newer value if a
   * defensive duplicate identity ever crosses a page boundary.
   */
  prepend(block: HostBlock): boolean {
    assertBlock(block);
    if (this.#blocks.has(block.id)) return false;
    if (this.#order.length >= MAX_HISTORY_BLOCKS) {
      throw new ProjectionFailure('session history exceeds its block capacity');
    }
    const nextBytes = this.#bytes + byteLength(block.title) + byteLength(block.text);
    if (nextBytes > MAX_HISTORY_BYTES) {
      throw new ProjectionFailure('session history exceeds its bounded capacity');
    }
    this.#blocks.set(block.id, block);
    this.#order.unshift(block.id);
    this.#bytes = nextBytes;
    return true;
  }

  upsert(block: HostBlock): boolean {
    assertBlock(block);
    const previous = this.#blocks.get(block.id);
    if (previous) {
      if (
        previous.kind === block.kind &&
        previous.title === block.title &&
        previous.text === block.text
      ) {
        return false;
      }
      const nextBytes =
        this.#bytes -
        byteLength(previous.title) -
        byteLength(previous.text) +
        byteLength(block.title) +
        byteLength(block.text);
      if (nextBytes > MAX_HISTORY_BYTES) {
        throw new ProjectionFailure('session history exceeds its bounded capacity');
      }
      this.#bytes = nextBytes;
      this.#blocks.set(block.id, block);
      return true;
    }
    if (this.#order.length >= MAX_HISTORY_BLOCKS) {
      throw new ProjectionFailure('session history exceeds its block capacity');
    }
    const nextBytes = this.#bytes + byteLength(block.title) + byteLength(block.text);
    if (nextBytes > MAX_HISTORY_BYTES) {
      throw new ProjectionFailure('session history exceeds its bounded capacity');
    }
    this.#order.push(block.id);
    this.#blocks.set(block.id, block);
    this.#bytes = nextBytes;
    return true;
  }

  get(id: string): HostBlock | undefined {
    return this.#blocks.get(id);
  }

  blocks(): HostBlock[] {
    return this.#order.map((id) => this.#blocks.get(id) as HostBlock);
  }
}

/** A small async queue. Producers await capacity, so input cannot grow unbounded. */
class AsyncQueue<T> implements AsyncIterable<T> {
  readonly #values: T[] = [];
  readonly #waiters: Array<(result: IteratorResult<T>) => void> = [];
  readonly #capacity: number;
  #closed = false;
  #waitingPushes: Array<{ value: T; resolve: () => void; reject: (error: Error) => void }> = [];

  constructor(capacity: number) {
    this.#capacity = capacity;
  }

  async push(value: T): Promise<void> {
    if (this.#closed) throw fixedFailure();
    const waiter = this.#waiters.shift();
    if (waiter) {
      waiter({ done: false, value });
      return;
    }
    if (this.#values.length < this.#capacity) {
      this.#values.push(value);
      return;
    }
    await new Promise<void>((resolve, reject) => {
      this.#waitingPushes.push({ value, resolve, reject });
    });
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const pending of this.#waitingPushes.splice(0)) pending.reject(fixedFailure());
    while (this.#waiters.length > 0) this.#waiters.shift()?.({ done: true, value: undefined });
  }

  [Symbol.asyncIterator](): AsyncIterator<T> {
    return this;
  }

  next(): Promise<IteratorResult<T>> {
    const value = this.#values.shift();
    if (value !== undefined) {
      this.#promotePush();
      return Promise.resolve({ done: false, value });
    }
    if (this.#closed) return Promise.resolve({ done: true, value: undefined });
    return new Promise<IteratorResult<T>>((resolve) => this.#waiters.push(resolve));
  }

  #promotePush(): void {
    const pending = this.#waitingPushes.shift();
    if (!pending) return;
    if (this.#closed) {
      pending.reject(fixedFailure());
      return;
    }
    this.#values.push(pending.value);
    pending.resolve();
  }
}

/** Serializes transcript observation with a submit/stop command. */
class AsyncMutex {
  #tail = Promise.resolve();

  async run<T>(task: () => Promise<T>): Promise<T> {
    const previous = this.#tail;
    let release!: () => void;
    this.#tail = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous;
    try {
      return await task();
    } finally {
      release();
    }
  }
}

/** Bounded, sequential output with Node stream backpressure. */
class OutputQueue {
  readonly #output: Writable;
  readonly #pending: Array<{ bytes: Buffer; resolve: () => void; reject: (error: Error) => void }> =
    [];
  #pendingBytes = 0;
  #running = false;
  #closed = false;

  constructor(output: Writable) {
    this.#output = output;
  }

  write(event: HostEvent): Promise<void> {
    let encoded: string;
    try {
      encoded = JSON.stringify(event);
    } catch {
      return Promise.reject(fixedFailure());
    }
    const bytes = Buffer.from(`${encoded}\n`, 'utf8');
    if (bytes.length > MAX_LINE_BYTES || this.#closed) return Promise.reject(fixedFailure());
    if (
      this.#pending.length >= MAX_OUTPUT_QUEUE_ENTRIES ||
      this.#pendingBytes + bytes.length > MAX_OUTPUT_QUEUE_BYTES
    ) {
      return Promise.reject(fixedFailure());
    }
    return new Promise<void>((resolve, reject) => {
      this.#pending.push({ bytes, resolve, reject });
      this.#pendingBytes += bytes.length;
      void this.#pump();
    });
  }

  async drain(): Promise<void> {
    while (this.#running || this.#pending.length > 0) {
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
    }
  }

  close(): void {
    this.#closed = true;
    for (const pending of this.#pending.splice(0)) pending.reject(fixedFailure());
    this.#pendingBytes = 0;
  }

  async #pump(): Promise<void> {
    if (this.#running) return;
    this.#running = true;
    try {
      while (this.#pending.length > 0) {
        const pending = this.#pending.shift() as {
          bytes: Buffer;
          resolve: () => void;
          reject: (error: Error) => void;
        };
        this.#pendingBytes -= pending.bytes.length;
        try {
          await this.#write(pending.bytes);
          pending.resolve();
        } catch {
          pending.reject(fixedFailure());
          for (const rest of this.#pending.splice(0)) rest.reject(fixedFailure());
          this.#pendingBytes = 0;
          break;
        }
      }
    } finally {
      this.#running = false;
    }
  }

  async #write(bytes: Buffer): Promise<void> {
    let onError: ((error: Error) => void) | undefined;
    const writeDone = new Promise<void>((resolve, reject) => {
      onError = (error: Error) => reject(error);
      this.#output.once('error', onError);
      this.#output.write(bytes, (error?: Error | null) => {
        if (onError) this.#output.removeListener('error', onError);
        if (error) reject(error);
        else resolve();
      });
    });
    try {
      await writeDone;
    } finally {
      if (onError) this.#output.removeListener('error', onError);
    }
  }
}

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const keys = Object.keys(value);
  return keys.length === expected.length && expected.every((key) => Object.hasOwn(value, key));
}

function parseCommand(line: Buffer): HostCommand {
  let value: unknown;
  try {
    value = JSON.parse(line.toString('utf8'));
  } catch {
    throw fixedFailure();
  }
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw fixedFailure();
  const record = value as Record<string, unknown>;
  if (record.kind === 'send') {
    if (!exactKeys(record, ['kind', 'revision', 'text'])) throw fixedFailure();
    if (
      !Number.isSafeInteger(record.revision) ||
      (record.revision as number) < 0 ||
      typeof record.text !== 'string' ||
      !(record.text as string).isWellFormed() ||
      byteLength(record.text as string) > MAX_SEND_TEXT_BYTES
    ) {
      throw fixedFailure();
    }
    return {
      kind: 'send',
      revision: record.revision as number,
      text: record.text as string,
    };
  }
  if (record.kind === 'stop' && exactKeys(record, ['kind'])) return { kind: 'stop' };
  if (record.kind === 'close' && exactKeys(record, ['kind'])) return { kind: 'close' };
  throw fixedFailure();
}

async function readCommands(input: Readable, queue: AsyncQueue<HostCommand>): Promise<void> {
  let carry = Buffer.alloc(0);
  try {
    for await (const chunk of input) {
      if (!Buffer.isBuffer(chunk)) throw fixedFailure();
      let offset = 0;
      while (offset < chunk.length) {
        const newline = chunk.indexOf(0x0a, offset);
        const end = newline < 0 ? chunk.length : newline;
        const piece = chunk.subarray(offset, end);
        if (carry.length + piece.length > MAX_LINE_BYTES) throw fixedFailure();
        carry = piece.length === 0 ? carry : Buffer.concat([carry, piece]);
        offset = newline < 0 ? chunk.length : newline + 1;
        if (newline < 0) break;
        if (carry.length > 0 && carry[carry.length - 1] === 0x0d) {
          carry = carry.subarray(0, carry.length - 1);
        }
        if (carry.length === 0) throw fixedFailure();
        await queue.push(parseCommand(carry));
        carry = Buffer.alloc(0);
      }
    }
    if (carry.length > 0) await queue.push(parseCommand(carry));
    queue.close();
  } catch {
    queue.close();
    throw fixedFailure();
  }
}

function isTerminalRoot(root: SessionContinuitySnapshot['rootTurn']): boolean {
  return (
    root !== null &&
    (root.status === 'completed' || root.status === 'failed' || root.status === 'cancelled')
  );
}

function stateFromSnapshot(snapshot: SessionContinuitySnapshot): {
  running: boolean;
  waiting: boolean;
} {
  const root = snapshot.rootTurn;
  const running = root !== null && !isTerminalRoot(root);
  const waiting =
    running &&
    (root?.status === 'waiting_for_user' ||
      (Array.isArray(snapshot.interactions.pending) && snapshot.interactions.pending.length > 0));
  return { running, waiting };
}

function foldDelta(current: string, startOffset: number, delta: string, reset: boolean): string {
  const base = reset ? '' : current;
  if (startOffset > base.length) throw new BridgeFailure('assistant delta has a gap');
  const overlap = Math.min(base.length - startOffset, delta.length);
  if (overlap > 0 && base.slice(startOffset, startOffset + overlap) !== delta.slice(0, overlap)) {
    throw new BridgeFailure('assistant delta conflicts with prior output');
  }
  return base + delta.slice(overlap);
}

interface BridgeContext {
  readonly writer: OutputQueue;
  readonly connection: RuntimeHostConnection;
  readonly subscription: RuntimeHostSessionSubscription;
  readonly sessionId: string;
  readonly projection: Projection;
  readonly deltas: Map<string, string>;
  readonly gate: AsyncMutex;
  latestSnapshot: SessionContinuitySnapshot;
  transcriptWatermark: number | null;
  lastWaiting: boolean;
  closing: boolean;
  failed: boolean;
}

async function emit(ctx: BridgeContext, event: HostEvent): Promise<void> {
  if (!ctx.closing) await ctx.writer.write(event);
}

async function fail(ctx: BridgeContext): Promise<void> {
  if (ctx.failed) return;
  ctx.failed = true;
  ctx.closing = true;
  try {
    await ctx.writer.write({ kind: 'failed' });
  } catch {
    // The output peer may already have gone away; never print payload/error text.
  }
}

async function emitState(ctx: BridgeContext, initial = false): Promise<void> {
  const state = stateFromSnapshot(ctx.latestSnapshot);
  await emit(ctx, { kind: 'state', ...state });
  if (!initial && state.waiting && !ctx.lastWaiting) {
    await emit(ctx, { kind: 'notice', text: FIXED_WAITING_NOTICE });
  }
  ctx.lastWaiting = state.waiting;
}

async function emitMessageBlocks(ctx: BridgeContext, message: StoredMessage): Promise<void> {
  for (const block of ctx.projection.applyMessage(message)) {
    await emit(ctx, { kind: 'upsert', block });
  }
}

async function consumeTranscriptAdvance(
  ctx: BridgeContext,
  throughSequence: number,
): Promise<void> {
  if (ctx.transcriptWatermark !== null && throughSequence <= ctx.transcriptWatermark) {
    return;
  }
  let cursor: string | null = null;
  for (;;) {
    const page = await ctx.subscription.loadTranscriptPage({
      source: 'durable',
      direction: 'newer',
      throughSequence,
      cursor,
      anchorSequence: cursor === null ? ctx.transcriptWatermark : null,
      maxBytes: SESSION_TRANSCRIPT_PAGE_MAX_BYTES,
    });
    let assembledBytes = 0;
    const decoded = await ctx.subscription.decodeTranscriptPage(
      page,
      decodeStoredMessage,
      MAX_MESSAGE_BYTES,
      (deltaBytes) => {
        assembledBytes += deltaBytes;
        if (assembledBytes > MAX_HISTORY_BYTES) {
          throw new ProjectionFailure('transcript page exceeds its bounded capacity');
        }
      },
    );
    for (const entry of decoded.messages) await emitMessageBlocks(ctx, entry.message);
    if (decoded.nextCursor === null) break;
    if (decoded.nextCursor === cursor) throw fixedFailure();
    cursor = decoded.nextCursor;
  }
  ctx.transcriptWatermark = throughSequence;
}

async function loadInitialTranscript(
  subscription: RuntimeHostSessionSubscription,
  projection: Projection,
): Promise<void> {
  const bootstrap = subscription.transcriptBootstrap;
  if (!bootstrap) throw fixedFailure();
  let page = bootstrap.durable;
  let cursor: string | null = null;
  let firstPage = true;
  for (;;) {
    let assembledBytes = 0;
    const decoded = await subscription.decodeTranscriptPage(
      page,
      decodeStoredMessage,
      MAX_MESSAGE_BYTES,
      (deltaBytes) => {
        assembledBytes += deltaBytes;
        if (assembledBytes > MAX_HISTORY_BYTES) {
          throw new ProjectionFailure('transcript page exceeds its bounded capacity');
        }
      },
    );
    const placement = page.direction === 'older' && !firstPage ? 'prepend' : 'append';
    const entries = placement === 'prepend' ? [...decoded.messages].reverse() : decoded.messages;
    for (const entry of entries) projection.applyMessage(entry.message, placement);
    firstPage = false;
    if (decoded.nextCursor === null) break;
    if (decoded.nextCursor === cursor) throw fixedFailure();
    cursor = decoded.nextCursor;
    page = await subscription.loadTranscriptPage({
      source: page.source,
      direction: page.direction,
      throughSequence: page.throughSequence,
      cursor,
      anchorSequence: null,
      maxBytes: SESSION_TRANSCRIPT_PAGE_MAX_BYTES,
    });
  }

  let overlayAssembledBytes = 0;
  await subscription.loadTranscriptOverlay(
    (value) => {
      projection.applyMessage(decodeStoredMessage(value));
      return null;
    },
    MAX_MESSAGE_BYTES,
    (deltaBytes) => {
      overlayAssembledBytes += deltaBytes;
      if (overlayAssembledBytes > MAX_HISTORY_BYTES) {
        throw new ProjectionFailure('transcript overlay exceeds its bounded capacity');
      }
    },
  );
}

async function consumeSubscription(ctx: BridgeContext): Promise<void> {
  try {
    for await (const frame of ctx.subscription) {
      if (ctx.closing) return;
      await ctx.gate.run(() => acceptFrame(ctx, frame));
    }
    if (!ctx.closing) throw fixedFailure();
  } catch {
    if (!ctx.closing) await fail(ctx);
  }
}

async function acceptFrame(ctx: BridgeContext, frame: SubscriptionFrame): Promise<void> {
  switch (frame.kind) {
    case 'subscription.session_projection':
      ctx.latestSnapshot = frame.snapshot;
      await emitState(ctx);
      return;
    case 'subscription.session_delta': {
      const delta = frame.delta;
      const key = `${delta.kind}\0${delta.messageId}`;
      const blockId = delta.kind === 'text' ? delta.messageId : `${delta.messageId}:thinking`;
      const current = ctx.deltas.get(key) ?? ctx.projection.get(blockId)?.text ?? '';
      const text = foldDelta(current, delta.startOffset, delta.text, delta.reset === true);
      ctx.deltas.set(key, text);
      const block: HostBlock = {
        id: blockId,
        kind: delta.kind === 'text' ? 'assistant' : 'thinking',
        title: delta.kind === 'text' ? '助手' : '思考',
        text,
      };
      if (ctx.projection.upsert(block)) await emit(ctx, { kind: 'upsert', block });
      return;
    }
    case 'subscription.session_event':
      // Tool events are intentionally not a second transcript authority. The
      // following transcript watermark publishes their durable message.
      return;
    case 'subscription.transcript_advanced':
      await consumeTranscriptAdvance(ctx, frame.throughSequence);
      return;
    case 'subscription.closed':
      throw fixedFailure();
    default:
      return;
  }
}

async function requestWithDeadline<T>(task: Promise<T>, timeoutMs: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  return await Promise.race([
    task,
    new Promise<T>((_, reject) => {
      timer = setTimeout(() => reject(fixedFailure()), timeoutMs);
    }),
  ]).finally(() => clearTimeout(timer));
}

async function handleCommand(ctx: BridgeContext, command: HostCommand): Promise<boolean> {
  if (command.kind === 'close') {
    ctx.closing = true;
    return false;
  }
  if (command.kind === 'send') {
    return ctx.gate.run(async () => {
      const messageId = randomUUID();
      try {
        const result = await requestWithDeadline(
          ctx.connection.request('turn.message.submit', {
            originHostEpoch: ctx.connection.hostEpoch,
            sessionId: ctx.sessionId,
            messageId,
            content: { text: command.text },
            placement: 'current_turn',
          }),
          COMMAND_TIMEOUT_MS,
        );
        const accepted = result.disposition !== 'blocked';
        // `revision` belongs to the composer's local draft.  Host queueRevision
        // is a different authority and must never be sent as the receipt key.
        await emit(ctx, { kind: 'submitted', revision: command.revision, accepted });
        if (
          accepted &&
          ctx.projection.upsert({
            id: messageId,
            kind: 'user',
            title: '用户',
            text: command.text,
          })
        ) {
          await emit(ctx, {
            kind: 'upsert',
            block: { id: messageId, kind: 'user', title: '用户', text: command.text },
          });
        }
      } catch (error) {
        if (error instanceof RuntimeHostOperationError) {
          await emit(ctx, { kind: 'submitted', revision: command.revision, accepted: false });
          await emit(ctx, { kind: 'notice', text: FIXED_REJECTED_NOTICE });
        } else {
          await fail(ctx);
          return false;
        }
      }
      return !ctx.closing;
    });
  }
  return ctx.gate.run(async () => {
    const root = ctx.latestSnapshot.rootTurn;
    if (root === null || isTerminalRoot(root)) {
      await emit(ctx, { kind: 'stopped' });
      return !ctx.closing;
    }
    try {
      await requestWithDeadline(
        ctx.connection.request('turn.stop', {
          sessionId: ctx.sessionId,
          turnId: root.turnId,
          runId: root.runId,
        }),
        COMMAND_TIMEOUT_MS,
      );
      await emit(ctx, { kind: 'stopped' });
    } catch {
      // A rejected stop is not a successful stop receipt.  It is terminal for
      // this one-shot bridge, because a retry could target a different root.
      await fail(ctx);
      return false;
    }
    return !ctx.closing;
  });
}

function validateArguments(rootPath: string, sessionId: string): void {
  if (!isAbsolute(rootPath) || rootPath.length === 0) throw fixedFailure();
  if (
    sessionId.length === 0 ||
    byteLength(sessionId) > 128 ||
    !/^[A-Za-z0-9_-]+$/u.test(sessionId)
  ) {
    throw fixedFailure();
  }
}

async function closeConnection(
  connection: RuntimeHostConnection | undefined,
  subscription: RuntimeHostSessionSubscription | undefined,
): Promise<void> {
  await Promise.allSettled([
    subscription
      ? requestWithDeadline(subscription.close(), CLOSE_TIMEOUT_MS).catch(() => undefined)
      : Promise.resolve(),
    connection
      ? requestWithDeadline(connection.close(), CLOSE_TIMEOUT_MS).catch(() => undefined)
      : Promise.resolve(),
  ]);
}

export async function runSessionHost(
  rootPath: string,
  sessionId: string,
  input: Readable = process.stdin,
  output: Writable = process.stdout,
): Promise<void> {
  const writer = new OutputQueue(output);
  let connection: RuntimeHostConnection | undefined;
  let subscription: RuntimeHostSessionSubscription | undefined;
  let ctx: BridgeContext | undefined;
  try {
    // Pairing is the first frame, before the baseline check or any Host work;
    // Rust can therefore distinguish a silent process from a slow bootstrap.
    await writer.write({ kind: 'hello', version: BRIDGE_VERSION });
    validateArguments(rootPath, sessionId);
    assertHostBaseline();
    const bootstrapDeadline = Date.now() + BOOTSTRAP_TIMEOUT_MS;
    const bootstrapRequest = <T>(task: Promise<T>): Promise<T> =>
      requestWithDeadline(task, Math.max(1, bootstrapDeadline - Date.now()));
    const connected = await requestWithDeadline(
      connectExistingRuntimeHost({
        rootPath,
        protocol: { min: HOST_BASELINE.protocolVersion, max: HOST_BASELINE.protocolVersion },
        compositionId: HOST_BASELINE.compositionId,
      }),
      Math.max(1, bootstrapDeadline - Date.now()),
    );
    if (connected.kind !== 'connected') throw fixedFailure();
    connection = connected.connection;
    subscription = await bootstrapRequest(
      connection.openSessionSubscription({
        sessionId,
        transcript: { kind: 'tail', maxBytes: SESSION_TRANSCRIPT_BOOTSTRAP_MAX_BYTES },
      }),
    );
    const projection = new Projection();
    await bootstrapRequest(loadInitialTranscript(subscription, projection));
    ctx = {
      writer,
      connection,
      subscription,
      sessionId,
      projection,
      deltas: new Map(),
      gate: new AsyncMutex(),
      latestSnapshot: subscription.snapshot,
      transcriptWatermark: subscription.transcriptBootstrap?.throughSequence ?? null,
      lastWaiting: false,
      closing: false,
      failed: false,
    };
    await writer.write({ kind: 'history', blocks: projection.blocks() });
    await emitState(ctx, true);
    await writer.write({ kind: 'ready', session_id: sessionId });
    if (ctx.lastWaiting) await writer.write({ kind: 'notice', text: FIXED_WAITING_NOTICE });
    const queue = new AsyncQueue<HostCommand>(MAX_INPUT_QUEUE_ENTRIES);
    const frameTask = consumeSubscription(ctx);
    const inputTask = readCommands(input, queue).catch(async () => {
      if (ctx && !ctx.closing) await fail(ctx);
    });
    const commandTask = (async () => {
      for await (const command of queue) {
        if (ctx?.closing) break;
        if (!(await handleCommand(ctx as BridgeContext, command))) {
          queue.close();
          break;
        }
      }
    })();
    await Promise.race([frameTask, inputTask, commandTask]);
    queue.close();
  } catch {
    if (ctx) await fail(ctx);
    else {
      try {
        await writer.write({ kind: 'failed' });
      } catch {
        // The Rust peer may have already closed its pipe.
      }
    }
  } finally {
    if (ctx) ctx.closing = true;
    input.destroy();
    await closeConnection(connection, subscription);
    await writer.drain().catch(() => undefined);
    writer.close();
  }
}

async function main(): Promise<void> {
  if (process.argv.length !== 4) throw fixedFailure();
  await runSessionHost(process.argv[2], process.argv[3]);
}

const invokedAsScript =
  process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (invokedAsScript) {
  try {
    await main();
  } catch {
    process.exitCode = 1;
    process.stderr.write('Maka TUI session bridge: protocol or transport failure.\n');
  } finally {
    process.stdin.destroy();
    process.stdout.end();
  }
}
