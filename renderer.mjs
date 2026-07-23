import {
  normalizeCaptureError,
  summarizeAudioInputs,
  summarizeTrack,
} from './probe-shared.mjs';
import {
  CAPTURE_WINDOW_MS,
  recordStreamForProbe,
  stopRecorderSafely,
  stopTracksSafely,
} from './capture-lifecycle.mjs';

const state = {
  before: null,
  after: null,
  macPermissionRequest: null,
  capture: null,
  promptObserved: 'unsure',
};

let activeStream = null;
let activeRecorder = null;
let busy = false;
let lastCaptureCleanup = null;

const elements = {
  runtime: document.querySelector('#runtime'),
  nativeStatus: document.querySelector('#native-status'),
  webStatus: document.querySelector('#web-status'),
  devices: document.querySelector('#devices'),
  flow: document.querySelector('#flow'),
  policy: document.querySelector('#policy'),
  refresh: document.querySelector('#refresh'),
  askMac: document.querySelector('#ask-mac'),
  capture: document.querySelector('#capture'),
  promptObserved: document.querySelector('#prompt-observed'),
  result: document.querySelector('#capture-result'),
  report: document.querySelector('#report'),
  copy: document.querySelector('#copy-report'),
  status: document.querySelector('#status-message'),
  flowNote: document.querySelector('#flow-note'),
};

function setStatus(message, tone = 'neutral') {
  elements.status.textContent = message;
  elements.status.dataset.tone = tone;
}

function setBusy(next) {
  busy = next;
  elements.refresh.disabled = next;
  elements.askMac.disabled =
    next ||
    state.before?.main.runtime.platform !== 'darwin' ||
    state.before?.main.configuration.flow !== 'ask-then-capture';
  elements.capture.disabled =
    next ||
    (state.before?.main.configuration.flow === 'ask-then-capture' &&
      state.macPermissionRequest === null);
}

async function readWebPermission() {
  if (!navigator.permissions?.query) return { supported: false, state: 'unsupported' };
  try {
    const permission = await navigator.permissions.query({ name: 'microphone' });
    return { supported: true, state: permission.state };
  } catch (error) {
    return {
      supported: false,
      state: 'unknown',
      error: normalizeCaptureError(error),
    };
  }
}

async function readAudioInputs() {
  if (!navigator.mediaDevices?.enumerateDevices) {
    return { supported: false, count: null, labeledCount: null };
  }
  try {
    const devices = await navigator.mediaDevices.enumerateDevices();
    return { supported: true, ...summarizeAudioInputs(devices) };
  } catch (error) {
    return {
      supported: false,
      count: null,
      labeledCount: null,
      error: normalizeCaptureError(error),
    };
  }
}

async function collectSnapshot() {
  const [main, webPermission, audioInputs] = await Promise.all([
    window.makaMicProbe.getSnapshot(),
    readWebPermission(),
    readAudioInputs(),
  ]);
  return { main, webPermission, audioInputs };
}

function renderSnapshot(snapshot) {
  const { runtime, configuration, nativeMicrophonePermission } = snapshot.main;
  elements.runtime.textContent =
    `${runtime.platform} ${runtime.arch} · ${runtime.osRelease} · ` +
    `Electron ${runtime.electronVersion} · ${runtime.isPackaged ? 'packaged' : 'source run'}`;
  elements.nativeStatus.textContent = nativeMicrophonePermission.supported
    ? nativeMicrophonePermission.status
    : 'unsupported on this platform';
  elements.webStatus.textContent = snapshot.webPermission.state;
  elements.devices.textContent = snapshot.audioInputs.supported
    ? `${snapshot.audioInputs.count} 个输入（${snapshot.audioInputs.labeledCount} 个标签可见，仅记录数量）`
    : '无法枚举';
  elements.flow.textContent = configuration.flow;
  elements.policy.textContent = configuration.sessionPolicy;

  if (configuration.flow === 'direct') {
    elements.flowNote.textContent =
      'direct：不先调用系统授权接口；点击采集时由 getUserMedia 直接触发。这对应 Maka 当前 Voice 页面。';
  } else {
    elements.flowNote.textContent =
      'ask-then-capture：先显式请求 macOS 系统权限，再运行同一采集路径。请与 direct 分开重置后测试。';
  }
}

function buildReport() {
  const latest = state.after ?? state.before;
  return {
    schemaVersion: 1,
    generatedAt: new Date().toISOString(),
    runtime: latest?.main.runtime ?? null,
    experiment: {
      flow: latest?.main.configuration.flow ?? null,
      sessionPolicy: latest?.main.configuration.sessionPolicy ?? null,
      promptObserved: state.promptObserved,
    },
    permission: {
      nativeBefore: state.before?.main.nativeMicrophonePermission ?? null,
      nativeRequest: state.macPermissionRequest,
      nativeAfter: state.after?.main.nativeMicrophonePermission ?? null,
      webBefore: state.before?.webPermission ?? null,
      webAfter: state.after?.webPermission ?? null,
    },
    audioInputs: {
      before: state.before?.audioInputs ?? null,
      after: state.after?.audioInputs ?? null,
    },
    sessionPermissionEvents:
      latest?.main.permissionHandlerEvents ?? null,
    capture: state.capture,
    privacy: {
      audioPersistedByProbeCode: false,
      audioUploadedByProbeCode: false,
      audioPlayedBackByProbeCode: false,
      deviceLabelsIncludedInReport: false,
      deviceIdsIncludedInReport: false,
      note: 'MediaRecorder chunks are counted in memory and discarded.',
    },
  };
}

function renderReport() {
  elements.report.value = JSON.stringify(buildReport(), null, 2);
}

async function refresh({ preserveBefore = true } = {}) {
  const snapshot = await collectSnapshot();
  if (!preserveBefore || state.before === null) state.before = snapshot;
  state.after = snapshot;
  renderSnapshot(snapshot);
  renderReport();
  return snapshot;
}

async function runMakaStyleCapture() {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw Object.assign(new Error('navigator.mediaDevices.getUserMedia is unavailable'), {
      name: 'GetUserMediaUnavailable',
    });
  }
  if (typeof MediaRecorder === 'undefined') {
    throw Object.assign(new Error('MediaRecorder is unavailable'), {
      name: 'MediaRecorderUnavailable',
    });
  }

  const acquisitionStarted = performance.now();
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      channelCount: 1,
      sampleRate: 48_000,
    },
    video: false,
  });
  activeStream = stream;
  const acquiredAt = performance.now();
  const tracks = stream.getAudioTracks();
  const tracksAtStart = tracks.map(summarizeTrack);
  lastCaptureCleanup = null;

  try {
    const result = await recordStreamForProbe({
      stream,
      MediaRecorderConstructor: MediaRecorder,
      durationMs: CAPTURE_WINDOW_MS,
      onRecorder: (recorder) => {
        activeRecorder = recorder;
      },
    });
    lastCaptureCleanup = result.tracksEndedAfterCleanup;

    return {
      ok: true,
      acquireMs: Math.round(acquiredAt - acquisitionStarted),
      ...result,
      audioTrackCount: tracks.length,
      tracksAtStart,
    };
  } finally {
    const cleanup = stopTracksSafely(stream);
    lastCaptureCleanup = cleanup.allEnded;
    stopRecorderSafely(activeRecorder);
    activeRecorder = null;
    activeStream = null;
  }
}

async function runAction(action) {
  if (busy) return;
  busy = true;
  setBusy(true);
  try {
    await action();
  } catch (error) {
    setStatus(normalizeCaptureError(error).message || '操作失败', 'error');
  } finally {
    busy = false;
    setBusy(false);
    renderReport();
  }
}

elements.refresh.addEventListener('click', () => {
  void runAction(async () => {
    await refresh();
    setStatus('状态已刷新。', 'success');
  });
});

elements.askMac.addEventListener('click', () => {
  void runAction(async () => {
    if (state.before === null) state.before = await collectSnapshot();
    setStatus('正在等待 macOS 系统权限结果…');
    state.macPermissionRequest = await window.makaMicProbe.askMacPermission();
    await refresh();
    if (state.macPermissionRequest.error) {
      setStatus(
        `macOS 权限请求失败：${state.macPermissionRequest.error.name}`,
        'error',
      );
    } else {
      setStatus(
        state.macPermissionRequest.granted
          ? 'macOS 返回已允许；现在可以运行采集。'
          : 'macOS 未授予权限；再次调用通常不会重复弹窗。',
        state.macPermissionRequest.granted ? 'success' : 'error',
      );
    }
  });
});

elements.capture.addEventListener('click', () => {
  void runAction(async () => {
    if (state.before === null) state.before = await collectSnapshot();
    setStatus('正在打开麦克风并进行约 2 秒的内存自检…');
    state.capture = { status: 'running' };
    renderReport();

    try {
      const result = await runMakaStyleCapture();
      state.capture = {
        status: 'ok',
        ...result,
      };
      elements.result.textContent =
        `成功：${result.recordingMs} ms，${result.audioBytes} bytes；音轨已停止。`;
      elements.result.dataset.tone = 'success';
      setStatus('采集成功，音频字节已丢弃，音轨已停止。', 'success');
    } catch (error) {
      const normalized = normalizeCaptureError(error);
      state.capture = {
        status: 'error',
        error: normalized,
        tracksEndedAfterCleanup: lastCaptureCleanup,
      };
      elements.result.textContent = `${normalized.name}：${normalized.meaning}`;
      elements.result.dataset.tone = 'error';
      setStatus(`采集失败：${normalized.name}`, 'error');
    }

    await refresh();
  });
});

elements.promptObserved.addEventListener('change', () => {
  state.promptObserved = elements.promptObserved.value;
  renderReport();
});

elements.copy.addEventListener('click', () => {
  void runAction(async () => {
    renderReport();
    await window.makaMicProbe.copyReport(elements.report.value);
    setStatus('JSON 已复制到剪贴板。', 'success');
  });
});

window.addEventListener('beforeunload', () => {
  stopTracksSafely(activeStream);
  stopRecorderSafely(activeRecorder);
});

void runAction(async () => {
  await refresh({ preserveBefore: false });
  setStatus('准备就绪。请先确认测试流程，再执行权限或采集步骤。');
});
