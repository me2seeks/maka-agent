import assert from 'node:assert/strict';
import { test } from 'node:test';
import { RuntimeHostSessionProjector, type RuntimeHostTerminalTurn } from '../adapter/index.js';
import {
  SESSION_CONTINUITY_SCHEMA_VERSION,
  type SessionContinuitySnapshot,
} from '../protocol/index.js';

test('projects the canonical failed-Turn diagnostic instead of rebuilding one from its class', () => {
  const failed: RuntimeHostTerminalTurn = {
    sessionId: 'session-1',
    turnId: 'turn-1',
    runId: 'run-1',
    status: 'failed',
    terminalEventId: 'event-1',
    failureClass: 'unknown',
    failureMessage: 'Your plan has no remaining usage.',
  };
  const projector = new RuntimeHostSessionProjector(snapshot(failed), [], () => 42);

  assert.deepEqual(projector.seedTerminal(failed), [
    {
      type: 'error',
      id: 'event-1',
      turnId: 'turn-1',
      ts: 42,
      recoverable: false,
      reason: 'unknown',
      message: 'Your plan has no remaining usage.',
    },
  ]);
});

test('keeps the legacy class fallback for failed Runs without a diagnostic', () => {
  const failed: RuntimeHostTerminalTurn = {
    sessionId: 'session-1',
    turnId: 'turn-1',
    runId: 'run-1',
    status: 'failed',
    terminalEventId: 'event-1',
    failureClass: 'app_restarted',
  };
  const projector = new RuntimeHostSessionProjector(snapshot(failed), [], () => 42);

  assert.equal(
    projector.seedTerminal(failed).find((event) => event.type === 'error')?.message,
    'Turn failed: app_restarted',
  );
});

function snapshot(rootTurn: RuntimeHostTerminalTurn): SessionContinuitySnapshot {
  return {
    schemaVersion: SESSION_CONTINUITY_SCHEMA_VERSION,
    session: {
      sessionId: 'session-1',
      metadataRevision: 1,
      status: 'blocked',
      createdAt: 1,
      lastUsedAt: 2,
      isArchived: false,
    },
    projectionRevision: 1,
    rootTurn,
    goal: null,
    queue: { hostEpoch: 'epoch-1', queueRevision: 1, steering: [], followup: [] },
    interactions: { pending: [] },
  };
}
