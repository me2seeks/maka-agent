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

import {
  projectInteractionClientCapabilityRequest,
  projectInteractionFormRequest,
  projectInteractionPermissionRequest,
  projectInteractionQuestionRequest,
  projectInteractionSandboxBoundaryRequest,
} from '@maka/core/interaction';

import {
  InteractionProjectionState,
  interactionAnswerForCommand,
  pendingSnapshotsEqual,
} from '../companion/interaction-projection.ts';

function pending(interactionId, request, overrides = {}) {
  return {
    schemaVersion: 1,
    interactionId,
    sessionId: 'session-1',
    turnId: 'turn-1',
    runId: 'run-1',
    revision: 1,
    status: 'pending',
    outcome: null,
    request,
    ...overrides,
  };
}

test('projects session-scoped sandbox and Client Capability approvals', () => {
  const cases = [
    {
      id: 'sandbox-1',
      request: projectInteractionSandboxBoundaryRequest({
        expansion: {
          filesystem: {
            entries: [{ path: '/workspace/cache', access: 'write', scope: 'subtree' }],
          },
        },
        justification: 'The MCP task needs to write its cache.',
      }),
      kind: 'sandbox_boundary',
      answer: { kind: 'sandbox_boundary', decision: 'allow' },
    },
    {
      id: 'capability-1',
      request: projectInteractionClientCapabilityRequest({
        toolUseId: 'mcp-call-1',
        target: {
          providerId: 'mcp',
          contractId: 'contract-v1',
          serverId: 'server-1',
          toolName: 'read_resource',
          capability: 'desktop_mcp',
          scope: { kind: 'mcp_tool', serverId: 'server-1', toolName: 'read_resource' },
        },
      }),
      kind: 'client_capability',
      answer: { kind: 'client_capability', decision: 'allow' },
    },
  ];
  const snapshots = cases.map(({ id, request }) => pending(id, request));
  const state = new InteractionProjectionState();
  const projected = state.project(snapshots);

  assert.deepEqual(
    projected.map(({ id, kind, fields }) => ({ id, kind, fields })),
    cases.map(({ id, kind }) => ({ id, kind, fields: [] })),
  );
  for (const [index, candidate] of cases.entries()) {
    const snapshot = snapshots[index];
    const presentedKind = state.presentedKind(candidate.id);
    assert.equal(presentedKind, candidate.kind);
    assert.deepEqual(
      interactionAnswerForCommand(
        snapshot,
        { id: candidate.id, action: 'allow', values: {} },
        presentedKind,
      ),
      candidate.answer,
    );
    assert.deepEqual(
      interactionAnswerForCommand(
        snapshot,
        { id: candidate.id, action: 'deny', values: {} },
        presentedKind,
      ),
      { ...candidate.answer, decision: 'deny' },
    );
    // These are session-scoped approvals, not the retired allow-once shape.
    assert.equal(
      interactionAnswerForCommand(
        snapshot,
        { id: candidate.id, action: 'allow', values: { rememberForTurn: true } },
        presentedKind,
      ),
      undefined,
    );
  }
});

test('fails closed for legacy permission and oversized sandbox authorization evidence', () => {
  const completeLegacyPermission = projectInteractionPermissionRequest({
    kind: 'tool_permission',
    requestId: 'permission-1',
    toolUseId: 'tool-1',
    toolName: 'Bash',
    category: 'shell_unsafe',
    reason: 'shell_dangerous',
    args: { command: 'echo hello', cwd: '/workspace' },
    rememberForTurnAllowed: true,
  });
  const oversizedSandbox = projectInteractionSandboxBoundaryRequest({
    expansion: {
      filesystem: {
        entries: Array.from({ length: 32 }, (_, index) => ({
          path: `/workspace/entry-${String(index).padStart(2, '0')}-${'x'.repeat(760)}`,
          access: 'write',
          scope: 'exact',
        })),
      },
    },
    justification: 'The MCP task needs its declared cache paths.',
  });
  const state = new InteractionProjectionState();
  const snapshots = [
    pending('permission-1', completeLegacyPermission),
    pending('sandbox-oversized', oversizedSandbox),
  ];
  const projected = state.project(snapshots);

  assert.deepEqual(
    projected.map(({ id, kind }) => ({ id, kind })),
    [
      { id: 'permission-1', kind: 'unsupported' },
      { id: 'sandbox-oversized', kind: 'unsupported' },
    ],
  );
  for (const snapshot of snapshots) {
    assert.equal(
      interactionAnswerForCommand(
        snapshot,
        { id: snapshot.interactionId, action: 'allow', values: {} },
        'unsupported',
      ),
      undefined,
    );
  }
});

test('projects all canonical MCP form field kinds and validates one answer', () => {
  const request = projectInteractionFormRequest({
    toolUseId: 'mcp-form-1',
    message: 'Choose deployment settings',
    requester: { name: 'deploy', source: 'Example MCP server' },
    fields: [
      {
        kind: 'string',
        name: 'owner',
        label: 'Owner email',
        required: true,
        format: 'email',
        minLength: 3,
        maxLength: 100,
      },
      {
        kind: 'number',
        name: 'ratio',
        label: 'Traffic ratio',
        required: false,
        minimum: 0,
        maximum: 1,
        default: 0.5,
      },
      {
        kind: 'integer',
        name: 'replicas',
        label: 'Replicas',
        required: true,
        minimum: 1,
        maximum: 10,
      },
      {
        kind: 'boolean',
        name: 'confirm',
        label: 'Confirm deployment',
        required: true,
        default: false,
      },
      {
        kind: 'single_select',
        name: 'environment',
        label: 'Environment',
        required: true,
        options: [
          { value: 'staging', label: 'Staging' },
          { value: 'production', label: 'Production' },
        ],
        default: 'staging',
      },
      {
        kind: 'multi_select',
        name: 'regions',
        label: 'Regions',
        required: false,
        options: [
          { value: 'us', label: 'US' },
          { value: 'eu', label: 'EU' },
        ],
        minItems: 1,
        maxItems: 2,
        default: ['us'],
      },
    ],
  });
  const snapshot = pending('form-1', request);
  const state = new InteractionProjectionState();
  const [projected] = state.project([snapshot]);

  assert.equal(projected.kind, 'form');
  assert.deepEqual(
    projected.fields.map(({ name, kind }) => ({ name, kind })),
    [
      { name: 'owner', kind: 'string' },
      { name: 'ratio', kind: 'number' },
      { name: 'replicas', kind: 'integer' },
      { name: 'confirm', kind: 'boolean' },
      { name: 'environment', kind: 'single_select' },
      { name: 'regions', kind: 'multi_select' },
    ],
  );
  assert.deepEqual(projected.fields.at(-1), {
    name: 'regions',
    label: 'Regions',
    kind: 'multi_select',
    required: false,
    options: [
      { value: 'us', label: 'US' },
      { value: 'eu', label: 'EU' },
    ],
    default: ['us'],
    minItems: 1,
    maxItems: 2,
  });

  const values = {
    owner: 'owner@example.test',
    ratio: 0.25,
    replicas: 3,
    confirm: true,
    environment: 'production',
    regions: ['us', 'eu'],
  };
  assert.equal(
    interactionAnswerForCommand(
      snapshot,
      { id: 'form-1', action: 'accept', values: { ...values, owner: 'not-an-email' } },
      'form',
    ),
    undefined,
  );
  assert.deepEqual(
    interactionAnswerForCommand(snapshot, { id: 'form-1', action: 'accept', values }, 'form'),
    { kind: 'form', action: 'accept', values },
  );
});

test('rejects changed request content and never revives a disappeared identity', () => {
  const original = projectInteractionQuestionRequest({
    toolUseId: 'question-tool',
    questions: [{ question: 'Choose a region', options: [{ label: 'US' }, { label: 'EU' }] }],
  });
  const changed = projectInteractionQuestionRequest({
    toolUseId: 'question-tool',
    questions: [
      { question: 'Choose a different region', options: [{ label: 'US' }, { label: 'EU' }] },
    ],
  });
  const first = pending('question-1', original);
  const state = new InteractionProjectionState();

  assert.equal(pendingSnapshotsEqual(first, pending('question-1', changed)), false);
  assert.equal(state.project([first]).length, 1);
  assert.throws(() => state.project([pending('question-1', changed)]), /reused with new content/);
  assert.deepEqual(state.project([]), []);
  assert.deepEqual(state.project([first]), []);
  assert.equal(state.presentedKind('question-1'), undefined);
});
