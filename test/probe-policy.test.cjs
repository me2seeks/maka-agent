'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const {
  assertMacPermissionRequestAllowed,
  evaluatePermission,
  locationKind,
  parseProbeConfiguration,
} = require('../probe-policy.cjs');

const trustedUrl = 'file:///opt/maka-mic-probe/index.html';

function evaluate(overrides = {}) {
  return evaluatePermission({
    phase: 'request',
    sessionPolicy: 'allow',
    permission: 'media',
    webContentsId: 7,
    trustedWebContentsId: 7,
    currentUrl: trustedUrl,
    trustedUrl,
    requestingOrigin: 'file://',
    details: {
      isMainFrame: true,
      requestingUrl: trustedUrl,
      securityOrigin: 'file://',
      mediaTypes: ['audio'],
    },
    ...overrides,
  });
}

test('configuration defaults reproduce the direct Maka path', () => {
  assert.deepEqual(parseProbeConfiguration([]), {
    flow: 'direct',
    sessionPolicy: 'default',
  });
});

test('configuration accepts explicit flow and strict policy', () => {
  assert.deepEqual(
    parseProbeConfiguration([
      '--flow=ask-then-capture',
      '--session-policy=deny',
    ]),
    {
      flow: 'ask-then-capture',
      sessionPolicy: 'deny',
    },
  );
});

test('configuration rejects unknown values', () => {
  assert.throws(() => parseProbeConfiguration(['--flow=other']), /Invalid --flow/);
  assert.throws(
    () => parseProbeConfiguration(['--session-policy=other']),
    /Invalid --session-policy/,
  );
});

test('direct flow cannot invoke the explicit macOS permission IPC', () => {
  assert.throws(
    () => assertMacPermissionRequestAllowed({ flow: 'direct' }),
    /disabled in the direct experiment flow/,
  );
  assert.doesNotThrow(() =>
    assertMacPermissionRequestAllowed({ flow: 'ask-then-capture' }),
  );
});

test('strict allow accepts only the owned main-frame audio request', () => {
  assert.equal(evaluate().allowed, true);
  assert.equal(
    evaluate({
      phase: 'check',
      details: {
        isMainFrame: true,
        mediaType: 'audio',
        requestingUrl: trustedUrl,
      },
    }).allowed,
    true,
  );
});

test('strict allow fails closed for every trust or media mismatch', () => {
  assert.equal(evaluate({ permission: 'geolocation' }).allowed, false);
  assert.equal(evaluate({ webContentsId: 99 }).allowed, false);
  assert.equal(evaluate({ currentUrl: 'https://example.com/' }).allowed, false);
  assert.equal(
    evaluate({
      requestingOrigin: 'https://evil.example',
      details: {
        isMainFrame: true,
        requestingUrl: 'https://evil.example/request',
        securityOrigin: 'https://evil.example',
        mediaTypes: ['audio'],
      },
    }).allowed,
    false,
  );
  assert.equal(
    evaluate({
      details: { isMainFrame: true, mediaTypes: ['audio'] },
    }).allowed,
    false,
  );
  assert.equal(
    evaluate({ details: { isMainFrame: false, mediaTypes: ['audio'] } }).allowed,
    false,
  );
  assert.equal(
    evaluate({ details: { isMainFrame: true, mediaTypes: ['video'] } }).allowed,
    false,
  );
  assert.equal(
    evaluate({
      details: { isMainFrame: true, mediaTypes: ['audio', 'video'] },
    }).allowed,
    false,
  );
});

test('deny policy rejects even a trusted audio request', () => {
  assert.equal(evaluate({ sessionPolicy: 'deny' }).allowed, false);
});

test('permission event records location kinds without leaking paths', () => {
  const event = evaluate().event;
  assert.equal(event.locations.currentPage, 'local-file');
  assert.equal(JSON.stringify(event).includes('/opt/maka-mic-probe'), false);
  assert.equal(locationKind('https://example.com/private'), 'https');
});
