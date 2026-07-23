'use strict';

const FLOW_VALUES = new Set(['direct', 'ask-then-capture']);
const SESSION_POLICY_VALUES = new Set(['default', 'allow', 'deny']);

function readOption(argv, name, fallback) {
  const prefix = `--${name}=`;
  const match = argv.find((argument) => argument.startsWith(prefix));
  return match ? match.slice(prefix.length) : fallback;
}

function parseProbeConfiguration(argv) {
  const flow = readOption(argv, 'flow', 'direct');
  const sessionPolicy = readOption(argv, 'session-policy', 'default');

  if (!FLOW_VALUES.has(flow)) {
    throw new Error(`Invalid --flow value: ${flow}`);
  }
  if (!SESSION_POLICY_VALUES.has(sessionPolicy)) {
    throw new Error(`Invalid --session-policy value: ${sessionPolicy}`);
  }

  return Object.freeze({ flow, sessionPolicy });
}

function assertMacPermissionRequestAllowed(configuration) {
  if (configuration.flow !== 'ask-then-capture') {
    throw new Error('macOS permission requests are disabled in the direct experiment flow');
  }
}

function locationKind(value) {
  if (!value) return 'missing';
  if (value === 'null') return 'opaque';
  try {
    const parsed = new URL(value);
    return parsed.protocol === 'file:' ? 'local-file' : parsed.protocol.replace(':', '');
  } catch {
    return 'invalid';
  }
}

function isAudioOnly(phase, details) {
  if (phase === 'check') return details.mediaType === 'audio';
  const mediaTypes = details.mediaTypes;
  return (
    Array.isArray(mediaTypes) &&
    mediaTypes.length > 0 &&
    mediaTypes.every((mediaType) => mediaType === 'audio')
  );
}

function requestLocationsAreTrusted({ requestingOrigin, details, trustedUrl }) {
  if (details.requestingUrl !== trustedUrl) return false;
  const origins = [requestingOrigin, details.securityOrigin].filter(Boolean);
  return origins.every((origin) => locationKind(origin) === 'local-file');
}

/**
 * The single decision seam for Electron's check and request permission handlers.
 * It intentionally fails closed: strict allow mode grants only the owned main
 * frame, on the probe's exact local page, for an audio-only media request.
 */
function evaluatePermission(input) {
  const {
    phase,
    sessionPolicy,
    permission,
    webContentsId,
    trustedWebContentsId,
    currentUrl,
    trustedUrl,
    requestingOrigin,
    details = {},
  } = input;

  const facts = {
    ownedWebContents: webContentsId === trustedWebContentsId,
    trustedPage: currentUrl === trustedUrl,
    trustedRequestLocations: requestLocationsAreTrusted({
      requestingOrigin,
      details,
      trustedUrl,
    }),
    mainFrame: details.isMainFrame === true,
    mediaPermission: permission === 'media',
    audioOnly: isAudioOnly(phase, details),
  };

  let allowed = false;
  let reason = 'explicit-deny';

  if (sessionPolicy === 'allow') {
    const failedFact = Object.entries(facts).find(([, passed]) => !passed);
    allowed = failedFact === undefined;
    reason = allowed ? 'trusted-audio-request' : `failed-${failedFact[0]}`;
  } else if (sessionPolicy === 'default') {
    reason = 'default-handler-must-not-be-installed';
  }

  return {
    allowed,
    event: {
      at: new Date().toISOString(),
      phase,
      permission,
      decision: allowed ? 'allow' : 'deny',
      reason,
      facts,
      mediaType:
        phase === 'check'
          ? details.mediaType ?? 'missing'
          : Array.isArray(details.mediaTypes)
            ? [...details.mediaTypes]
            : 'missing',
      locations: {
        currentPage: locationKind(currentUrl),
        requestingOrigin: locationKind(requestingOrigin),
        requestingUrl: locationKind(details.requestingUrl),
        securityOrigin: locationKind(details.securityOrigin),
      },
    },
  };
}

module.exports = {
  assertMacPermissionRequestAllowed,
  evaluatePermission,
  locationKind,
  parseProbeConfiguration,
};
