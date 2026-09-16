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
 * The private Rust wire projection for Host Interactions.
 *
 * This module is deliberately an adapter, not another Interaction authority:
 * it keeps the Host snapshot/request identity and turns UI intents back into
 * the canonical answer union.  The Runtime Host remains the only writer.
 */

import { createHash } from 'node:crypto';
import {
  decodeInteractionAnswer,
  isInteractionAnswerValidForRequest,
  type InteractionAnswer,
  type InteractionFormField,
  type InteractionFormValue,
  type InteractionRequest,
} from '@maka/core/interaction';
import type { InteractionPendingSnapshot } from '@maka/runtime-host/protocol';

export const MAX_PENDING_INTERACTIONS = 16 as const;
export const MAX_INTERACTION_LINE_BYTES = 384 * 1024;
export const MAX_TRACKED_INTERACTION_IDS = 4_096;

const MAX_ID_BYTES = 256;
const MAX_TITLE_BYTES = 1_024;
const MAX_SOURCE_BYTES = 1_024;
const MAX_DETAIL_BYTES = 24 * 1024;
const MAX_FIELD_NAME_BYTES = 256;
const MAX_FIELD_LABEL_BYTES = 1_024;
const MAX_FIELD_DESCRIPTION_BYTES = 2 * 1024;
const MAX_FIELD_DEFAULT_BYTES = 8 * 1024;
const MAX_OPTION_VALUE_BYTES = 2 * 1024;
const MAX_OPTION_LABEL_BYTES = 1_024;
const MAX_FIELDS = 32;
const MAX_OPTIONS = 64;

const UNSUPPORTED_TITLE = '当前交互请求暂不支持';
const UNSUPPORTED_SOURCE = 'Runtime Host';
const UNSUPPORTED_DETAIL =
  '此请求类型或其完整审查内容暂不支持在此 Companion 中回答。请在其他 Maka 客户端处理；不会自动授权。';

export type InteractionWireKind =
  | 'sandbox_boundary'
  | 'client_capability'
  | 'question'
  | 'form'
  | 'unsupported';

export interface HostInteractionOption {
  readonly value: string;
  readonly label: string;
}

/** Mirrors the Rust `HostField` camelCase serde projection. */
export interface HostInteractionField {
  readonly name: string;
  readonly label: string;
  readonly kind: InteractionFormField['kind'] | 'question';
  readonly required: boolean;
  readonly description?: string;
  readonly default?: InteractionFormValue;
  readonly options?: readonly HostInteractionOption[];
  readonly minLength?: number;
  readonly maxLength?: number;
  readonly format?: 'email' | 'uri' | 'date' | 'date-time';
  readonly minimum?: number;
  readonly maximum?: number;
  readonly minItems?: number;
  readonly maxItems?: number;
}

export interface HostInteraction {
  readonly id: string;
  readonly kind: InteractionWireKind;
  readonly title: string;
  readonly source: string;
  readonly detail: string;
  readonly fields: readonly HostInteractionField[];
}

export type InteractionAction = 'allow' | 'deny' | 'accept' | 'decline' | 'cancel';

export interface InteractionAnswerCommand {
  readonly id: string;
  readonly action: InteractionAction;
  readonly values: Readonly<Record<string, unknown>>;
}

class InteractionProjectionLimitError extends Error {}

function byteLength(value: string): number {
  return Buffer.byteLength(value, 'utf8');
}

function assertSafeText(value: string, maxBytes: number, label: string): void {
  if (!value.isWellFormed() || byteLength(value) > maxBytes) {
    throw new InteractionProjectionLimitError(`${label} exceeds the presentation bound`);
  }
  for (const character of value) {
    const codePoint = character.codePointAt(0) ?? 0;
    if (
      (codePoint < 0x20 || (codePoint >= 0x7f && codePoint <= 0x9f)) &&
      character !== '\n' &&
      character !== '\t'
    ) {
      throw new InteractionProjectionLimitError(`${label} contains an unsupported character`);
    }
    if (
      codePoint === 0x061c ||
      codePoint === 0x200e ||
      codePoint === 0x200f ||
      (codePoint >= 0x202a && codePoint <= 0x202e) ||
      (codePoint >= 0x2066 && codePoint <= 0x2069)
    ) {
      throw new InteractionProjectionLimitError(`${label} contains a bidi control`);
    }
  }
}

function serializedBytes(value: unknown): number {
  let serialized: string;
  try {
    serialized = JSON.stringify(value);
  } catch {
    throw new InteractionProjectionLimitError('Interaction cannot be serialized');
  }
  if (serialized === undefined) throw new InteractionProjectionLimitError('Interaction is empty');
  return byteLength(serialized);
}

function assertSafeDefault(value: InteractionFormValue): void {
  if (typeof value === 'string') {
    assertSafeText(value, MAX_OPTION_VALUE_BYTES, 'Interaction field default');
  } else if (Array.isArray(value)) {
    for (const item of value)
      assertSafeText(item, MAX_OPTION_VALUE_BYTES, 'Interaction field default');
  }
}

function assertWireField(field: HostInteractionField): void {
  assertSafeText(field.name, MAX_FIELD_NAME_BYTES, 'Interaction field name');
  assertSafeText(field.label, MAX_FIELD_LABEL_BYTES, 'Interaction field label');
  if (field.description !== undefined)
    assertSafeText(field.description, MAX_FIELD_DESCRIPTION_BYTES, 'Interaction field description');
  if (field.default !== undefined) {
    assertSafeDefault(field.default);
    if (serializedBytes(field.default) > MAX_FIELD_DEFAULT_BYTES) {
      throw new InteractionProjectionLimitError('Interaction field default exceeds its bound');
    }
  }
  if (field.options !== undefined) {
    if (field.options.length > MAX_OPTIONS)
      throw new InteractionProjectionLimitError('Interaction field has too many options');
    for (const option of field.options) {
      assertSafeText(option.value, MAX_OPTION_VALUE_BYTES, 'Interaction option value');
      assertSafeText(option.label, MAX_OPTION_LABEL_BYTES, 'Interaction option label');
    }
  }
}

function assertWireRequest(request: HostInteraction): void {
  assertSafeText(request.id, MAX_ID_BYTES, 'Interaction identity');
  assertSafeText(request.title, MAX_TITLE_BYTES, 'Interaction title');
  assertSafeText(request.source, MAX_SOURCE_BYTES, 'Interaction source');
  assertSafeText(request.detail, MAX_DETAIL_BYTES, 'Interaction detail');
  if (request.fields.length > MAX_FIELDS)
    throw new InteractionProjectionLimitError('Interaction has too many fields');
  const names = new Set<string>();
  for (const field of request.fields) {
    if (names.has(field.name))
      throw new InteractionProjectionLimitError('Interaction field names collide');
    names.add(field.name);
    assertWireField(field);
  }
  if (serializedBytes(request) > MAX_INTERACTION_LINE_BYTES)
    throw new InteractionProjectionLimitError('Interaction exceeds the private wire bound');
}

function unsupported(id: string): HostInteraction {
  return {
    id,
    kind: 'unsupported',
    title: UNSUPPORTED_TITLE,
    source: UNSUPPORTED_SOURCE,
    detail: UNSUPPORTED_DETAIL,
    fields: [],
  };
}

function questionFieldName(index: number): string {
  return String(index);
}

function projectQuestion(
  request: Extract<InteractionRequest, { readonly kind: 'question' }>,
  id: string,
): HostInteraction {
  return {
    id,
    kind: 'question',
    title: 'Agent 提问',
    source: request.toolUseId,
    detail: '请逐项回答，可选择选项或自由输入。',
    fields: request.questions.map((question, index) => ({
      name: questionFieldName(index),
      label: question.question,
      // This is a private extension of the form vocabulary.  It means
      // question answer (option or free text), not a Runtime Host form kind.
      kind: 'question',
      required: false,
      ...(question.options.some((option) => option.description !== undefined)
        ? {
            description: question.options
              .filter((option) => option.description !== undefined)
              .map((option) => `选项「${option.label}」：${option.description}`)
              .join('\n'),
          }
        : {}),
      options: question.options.map((option) => ({ value: option.label, label: option.label })),
    })),
  };
}

function projectFormField(field: InteractionFormField): HostInteractionField {
  const common = {
    name: field.name,
    label: field.label,
    kind: field.kind,
    required: field.required,
    ...(field.description === undefined ? {} : { description: field.description }),
  } as const;
  switch (field.kind) {
    case 'string':
      return {
        ...common,
        ...(field.default === undefined ? {} : { default: field.default }),
        ...(field.minLength === undefined ? {} : { minLength: field.minLength }),
        ...(field.maxLength === undefined ? {} : { maxLength: field.maxLength }),
        ...(field.format === undefined ? {} : { format: field.format }),
      };
    case 'number':
    case 'integer':
      return {
        ...common,
        ...(field.default === undefined ? {} : { default: field.default }),
        ...(field.minimum === undefined ? {} : { minimum: field.minimum }),
        ...(field.maximum === undefined ? {} : { maximum: field.maximum }),
      };
    case 'boolean':
      return { ...common, ...(field.default === undefined ? {} : { default: field.default }) };
    case 'single_select':
      return {
        ...common,
        options: field.options.map((option) => ({ value: option.value, label: option.label })),
        ...(field.default === undefined ? {} : { default: field.default }),
      };
    case 'multi_select':
      return {
        ...common,
        options: field.options.map((option) => ({ value: option.value, label: option.label })),
        ...(field.default === undefined ? {} : { default: [...field.default] }),
        ...(field.minItems === undefined ? {} : { minItems: field.minItems }),
        ...(field.maxItems === undefined ? {} : { maxItems: field.maxItems }),
      };
  }
}

function projectRequest(snapshot: InteractionPendingSnapshot): HostInteraction {
  const { interactionId: id, request } = snapshot;
  switch (request.kind) {
    // The pinned Host retains the decoder but no longer admits this legacy kind.
    case 'permission':
      return unsupported(id);
    case 'question':
      return projectQuestion(request, id);
    case 'form':
      return {
        id,
        kind: 'form',
        title: request.message,
        source: request.requester.source ?? request.requester.name,
        detail: request.message,
        fields: request.fields.map(projectFormField),
      };
    case 'sandbox_boundary':
      return {
        id,
        kind: 'sandbox_boundary',
        title: '扩大当前会话的沙箱范围',
        source: 'Runtime Host',
        detail: `授权会修改当前会话的执行范围，不仅允许单次工具调用。\n原因：${request.justification}\n\n完整范围（read 读取；write 写入；exact 精确路径；subtree 子目录；network 网络）：\n${JSON.stringify(request.expansion, null, 2)}`,
        fields: [],
      };
    case 'client_capability':
      return {
        id,
        kind: 'client_capability',
        title: '授权当前会话使用客户端能力',
        source: request.target.providerId,
        detail: `授权会保存到当前会话，匹配此范围的后续调用可复用，不是单次允许。\n工具：${request.target.toolName}\n\n完整目标及范围：\n${JSON.stringify(request.target, null, 2)}`,
        fields: [],
      };
  }
}

function stable(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value) ?? 'undefined';
  if (Array.isArray(value)) return `[${value.map((item) => stable(item)).join(',')}]`;
  const record = value as Record<string, unknown>;
  return `{${Object.keys(record)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${stable(record[key])}`)
    .join(',')}}`;
}

export function pendingSnapshotsEqual(
  left: InteractionPendingSnapshot,
  right: InteractionPendingSnapshot,
): boolean {
  return (
    left.schemaVersion === right.schemaVersion &&
    left.interactionId === right.interactionId &&
    left.sessionId === right.sessionId &&
    left.turnId === right.turnId &&
    left.runId === right.runId &&
    left.revision === right.revision &&
    left.status === right.status &&
    left.outcome === null &&
    right.outcome === null &&
    stable(left.request) === stable(right.request)
  );
}

/**
 * Tracks canonical request identity across complete projections.  A removed
 * request id can never be admitted again during this bridge lifetime; an old
 * subscription frame may still repeat it while its answer is propagating and
 * is filtered by `markResolved`.
 */
export class InteractionProjectionState {
  readonly #fingerprints = new Map<string, string>();
  readonly #active = new Set<string>();
  readonly #resolved = new Set<string>();
  readonly #presented = new Map<string, InteractionWireKind>();

  markResolved(interactionId: string): void {
    if (!this.#fingerprints.has(interactionId)) return;
    this.#resolved.add(interactionId);
    this.#active.delete(interactionId);
    this.#presented.delete(interactionId);
  }

  presentedKind(interactionId: string): InteractionWireKind | undefined {
    return this.#presented.get(interactionId);
  }

  project(pending: readonly InteractionPendingSnapshot[]): readonly HostInteraction[] {
    if (pending.length > MAX_PENDING_INTERACTIONS)
      throw new InteractionProjectionLimitError('Host exceeded the pending Interaction bound');

    const nextActive = new Set<string>();
    const candidates: HostInteraction[] = [];
    for (const snapshot of pending) {
      if (snapshot.status !== 'pending')
        throw new InteractionProjectionLimitError(
          'Resolved Interaction appeared in pending projection',
        );
      const id = snapshot.interactionId;
      const fingerprint = createHash('sha256').update(stable(snapshot)).digest('hex');
      const previous = this.#fingerprints.get(id);
      if (previous !== undefined && previous !== fingerprint) {
        throw new InteractionProjectionLimitError(
          'Interaction identity was reused with new content',
        );
      }
      if (previous === undefined) {
        if (this.#fingerprints.size >= MAX_TRACKED_INTERACTION_IDS) {
          throw new InteractionProjectionLimitError(
            'Interaction identity tracking capacity exhausted',
          );
        }
        this.#fingerprints.set(id, fingerprint);
      }
      if (this.#resolved.has(id)) continue;
      if (nextActive.has(id))
        throw new InteractionProjectionLimitError('Interaction identity repeated');
      nextActive.add(id);
      let projected: HostInteraction;
      try {
        projected = projectRequest(snapshot);
        assertWireRequest(projected);
      } catch (error) {
        if (!(error instanceof InteractionProjectionLimitError)) throw error;
        projected = unsupported(id);
      }
      candidates.push(projected);
    }

    // A Host snapshot that removes an identity is a definitive resolution (or
    // closure), even if another Client performed it.  Retain that tombstone so
    // a later request cannot silently reuse the same id.
    for (const id of this.#active) {
      if (!nextActive.has(id)) this.#resolved.add(id);
    }
    const fitted = fitLineBudget(candidates);
    this.#presented.clear();
    for (const request of fitted) this.#presented.set(request.id, request.kind);
    this.#active.clear();
    for (const id of nextActive) this.#active.add(id);
    return fitted;
  }
}

function fitLineBudget(candidates: readonly HostInteraction[]): readonly HostInteraction[] {
  const result = [...candidates];
  // Keep the complete snapshot and identities, but degrade individual entries
  // to an explicit unsupported prompt if the private NDJSON line is too large.
  // No detail or risk text is truncated.
  while (
    serializedBytes({ kind: 'interactions', requests: result }) + 1 >
    MAX_INTERACTION_LINE_BYTES
  ) {
    const index = result.findLastIndex((request) => request.kind !== 'unsupported');
    if (index < 0)
      throw new InteractionProjectionLimitError('Interaction snapshot exceeds its wire bound');
    result[index] = unsupported(result[index]!.id);
  }
  return result;
}

function hasOnlyKeys(values: Readonly<Record<string, unknown>>, keys: readonly string[]): boolean {
  const allowed = new Set(keys);
  return Object.keys(values).every((key) => allowed.has(key));
}

/** Converts one private UI intent into the canonical Host answer envelope. */
export function interactionAnswerForCommand(
  snapshot: InteractionPendingSnapshot,
  command: InteractionAnswerCommand,
  presentedKind?: InteractionWireKind,
): InteractionAnswer | undefined {
  if (snapshot.interactionId !== command.id) return undefined;
  const canonicalKind: InteractionWireKind =
    snapshot.request.kind === 'sandbox_boundary' ||
    snapshot.request.kind === 'client_capability' ||
    snapshot.request.kind === 'question' ||
    snapshot.request.kind === 'form'
      ? snapshot.request.kind
      : 'unsupported';
  if (
    presentedKind === undefined ||
    presentedKind !== canonicalKind ||
    presentedKind === 'unsupported'
  ) {
    return undefined;
  }
  const values = command.values;
  let answer: InteractionAnswer;
  const request = snapshot.request;
  if (request.kind === 'sandbox_boundary' || request.kind === 'client_capability') {
    if (
      Object.keys(values).length !== 0 ||
      (command.action !== 'allow' && command.action !== 'deny')
    )
      return undefined;
    answer = { kind: request.kind, decision: command.action };
  } else if (request.kind === 'question') {
    if (command.action !== 'accept' && command.action !== 'cancel') return undefined;
    if (command.action === 'cancel' && Object.keys(values).length !== 0) return undefined;
    const names = request.questions.map((_, index) => questionFieldName(index));
    if (command.action === 'accept' && !hasOnlyKeys(values, names)) return undefined;
    const answers = request.questions.map((_, index) => {
      const value = values[questionFieldName(index)];
      if (value === undefined || value === null) return null;
      return typeof value === 'string' ? value : null;
    });
    if (
      command.action === 'accept' &&
      answers.some(
        (answer, index) => values[questionFieldName(index)] !== undefined && answer === null,
      )
    ) {
      return undefined;
    }
    answer = { kind: 'question', answers };
  } else if (request.kind === 'form') {
    if (command.action === 'accept') {
      answer = {
        kind: 'form',
        action: 'accept',
        values: Object.fromEntries(Object.entries(values)) as Readonly<
          Record<string, InteractionFormValue>
        >,
      };
    } else {
      if (command.action !== 'decline' && command.action !== 'cancel') return undefined;
      if (Object.keys(values).length !== 0) return undefined;
      answer = { kind: 'form', action: command.action };
    }
  } else {
    return undefined;
  }
  try {
    const decoded = decodeInteractionAnswer(answer);
    return isInteractionAnswerValidForRequest(request, decoded) ? decoded : undefined;
  } catch {
    return undefined;
  }
}
