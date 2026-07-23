import test from 'node:test';
import assert from 'node:assert/strict';
import {
  explainErrorName,
  normalizeCaptureError,
  summarizeAudioInputs,
  summarizeTrack,
} from '../probe-shared.mjs';

test('NotFoundError and NotReadableError remain distinct', () => {
  assert.equal(explainErrorName('NotFoundError').category, 'no-input-device');
  assert.equal(explainErrorName('NotReadableError').category, 'device-unreadable');
});

test('capture errors preserve the DOMException name and cap the message', () => {
  const normalized = normalizeCaptureError({
    name: 'NotReadableError',
    message: 'x'.repeat(900),
  });
  assert.equal(normalized.name, 'NotReadableError');
  assert.equal(normalized.message.length, 500);
});

test('track summaries never expose labels or stable device identifiers', () => {
  const summary = summarizeTrack({
    kind: 'audio',
    label: 'Secret microphone',
    enabled: true,
    muted: false,
    readyState: 'live',
    getSettings: () => ({
      sampleRate: 48_000,
      channelCount: 1,
      deviceId: 'secret-device-id',
      groupId: 'secret-group-id',
    }),
  });

  const serialized = JSON.stringify(summary);
  assert.equal(summary.settings.sampleRate, 48_000);
  assert.equal(serialized.includes('Secret microphone'), false);
  assert.equal(serialized.includes('secret-device-id'), false);
  assert.equal(serialized.includes('secret-group-id'), false);
});

test('device summaries count inputs without copying labels', () => {
  const summary = summarizeAudioInputs([
    { kind: 'audioinput', label: 'Built-in microphone' },
    { kind: 'audioinput', label: '' },
    { kind: 'videoinput', label: 'Camera' },
  ]);
  assert.deepEqual(summary, { count: 2, labeledCount: 1 });
  assert.equal(JSON.stringify(summary).includes('Built-in microphone'), false);
});
