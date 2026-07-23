const PUBLIC_TRACK_SETTINGS = Object.freeze([
  'sampleRate',
  'sampleSize',
  'channelCount',
  'echoCancellation',
  'noiseSuppression',
  'autoGainControl',
  'latency',
]);

const ERROR_MEANINGS = Object.freeze({
  NotAllowedError: {
    category: 'permission-denied',
    meaning: '权限、系统隐私设置或 Electron Session 策略拒绝了请求。',
  },
  SecurityError: {
    category: 'security-policy',
    meaning: '当前安全上下文或策略不允许访问麦克风。',
  },
  NotFoundError: {
    category: 'no-input-device',
    meaning: '没有找到满足约束的音频输入设备。',
  },
  NotReadableError: {
    category: 'device-unreadable',
    meaning: '设备存在，但操作系统、驱动、硬件故障或占用导致无法读取。',
  },
  AbortError: {
    category: 'capture-aborted',
    meaning: '设备打开后，采集在完成前被中止。',
  },
});

export function explainErrorName(name) {
  return (
    ERROR_MEANINGS[name] ?? {
      category: 'unknown-capture-error',
      meaning: '未分类的采集错误；请保留原始错误名用于后续排查。',
    }
  );
}

export function normalizeCaptureError(error) {
  const name =
    error && typeof error === 'object' && typeof error.name === 'string'
      ? error.name
      : 'UnknownError';
  const message =
    error && typeof error === 'object' && typeof error.message === 'string'
      ? error.message.slice(0, 500)
      : String(error ?? '').slice(0, 500);

  return {
    name,
    message,
    ...explainErrorName(name),
  };
}

export function summarizeTrack(track) {
  const rawSettings = track.getSettings();
  const settings = {};
  for (const key of PUBLIC_TRACK_SETTINGS) {
    const value = rawSettings[key];
    if (
      typeof value === 'number' ||
      typeof value === 'boolean' ||
      typeof value === 'string'
    ) {
      settings[key] = value;
    }
  }

  return {
    kind: track.kind,
    enabled: track.enabled,
    muted: track.muted,
    readyState: track.readyState,
    settings,
  };
}

export function summarizeAudioInputs(devices) {
  const inputs = devices.filter((device) => device.kind === 'audioinput');
  return {
    count: inputs.length,
    labeledCount: inputs.filter((device) => device.label.trim().length > 0).length,
  };
}
