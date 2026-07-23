export const CAPTURE_WINDOW_MS = 1_800;
export const RECORDER_FINALIZATION_TIMEOUT_MS = 500;

function delay(durationMs) {
  return new Promise((resolve) => setTimeout(resolve, durationMs));
}

function namedError(name, message, cause) {
  const error = new Error(message, cause === undefined ? undefined : { cause });
  error.name = name;
  return error;
}

export function stopRecorderSafely(recorder) {
  if (!recorder) return null;
  try {
    if (recorder.state !== 'inactive') recorder.stop();
    return null;
  } catch (error) {
    return namedError(
      'MediaRecorderStopError',
      error instanceof Error ? error.message : 'MediaRecorder.stop() failed',
      error,
    );
  }
}

export function stopTracksSafely(stream) {
  if (!stream) return { allEnded: true, stopErrors: 0 };

  let tracks;
  try {
    tracks = stream.getTracks();
  } catch {
    return { allEnded: false, stopErrors: 1 };
  }

  let stopErrors = 0;
  for (const track of tracks) {
    try {
      track.stop();
    } catch {
      stopErrors += 1;
    }
  }

  return {
    allEnded: tracks.every((track) => track.readyState === 'ended'),
    stopErrors,
  };
}

/**
 * Runs the MediaRecorder portion of the probe behind one small, testable seam.
 * Audio tracks are stopped at the nominal 1.8 second deadline before this
 * function waits for MediaRecorder finalization, so a missing `stop` event
 * cannot keep the microphone open.
 */
export async function recordStreamForProbe({
  stream,
  MediaRecorderConstructor,
  durationMs = CAPTURE_WINDOW_MS,
  finalizationTimeoutMs = RECORDER_FINALIZATION_TIMEOUT_MS,
  now = () => performance.now(),
  wait = delay,
  onRecorder = () => {},
}) {
  if (!Number.isFinite(durationMs) || durationMs < 0 || durationMs > CAPTURE_WINDOW_MS) {
    throw new RangeError(`durationMs must be between 0 and ${CAPTURE_WINDOW_MS}`);
  }
  if (!Number.isFinite(finalizationTimeoutMs) || finalizationTimeoutMs < 0) {
    throw new RangeError('finalizationTimeoutMs must be a non-negative number');
  }

  let recorder = null;
  let cleanup = { allEnded: false, stopErrors: 0 };

  try {
    recorder = new MediaRecorderConstructor(stream);
    onRecorder(recorder);
    let audioBytes = 0;
    const settled = new Promise((resolve) => {
      recorder.addEventListener('dataavailable', (event) => {
        audioBytes += event.data.size;
      });
      recorder.addEventListener(
        'stop',
        () => resolve({ status: 'stopped' }),
        { once: true },
      );
      recorder.addEventListener(
        'error',
        (event) =>
          resolve({
            status: 'error',
            error: event.error ?? namedError('MediaRecorderError', 'MediaRecorder failed'),
          }),
        { once: true },
      );
    });

    const startedAt = now();
    recorder.start();
    const firstOutcome = await Promise.race([
      wait(durationMs).then(() => ({ status: 'deadline' })),
      settled,
    ]);

    let finalOutcome = firstOutcome;
    let stopError = null;
    let captureEndedAt;
    if (firstOutcome.status === 'deadline') {
      stopError = stopRecorderSafely(recorder);
      // Do not await recorder finalization before releasing the microphone.
      cleanup = stopTracksSafely(stream);
      captureEndedAt = now();
      finalOutcome = await Promise.race([
        settled,
        wait(finalizationTimeoutMs).then(() => ({ status: 'timeout' })),
      ]);
    } else {
      cleanup = stopTracksSafely(stream);
      captureEndedAt = now();
    }

    if (stopError) throw stopError;
    if (finalOutcome.status === 'error') throw finalOutcome.error;
    if (finalOutcome.status === 'timeout') {
      throw namedError(
        'MediaRecorderStopTimeout',
        'MediaRecorder did not emit a stop event before the finalization timeout',
      );
    }
    if (!cleanup.allEnded) {
      throw namedError(
        'TrackCleanupError',
        `Microphone track cleanup did not finish (${cleanup.stopErrors} stop errors)`,
      );
    }

    return {
      recordingMs: Math.round(captureEndedAt - startedAt),
      audioBytes,
      mimeType: recorder.mimeType || null,
      recorderFinalization: finalOutcome.status,
      tracksEndedAfterCleanup: cleanup.allEnded,
      trackStopErrors: cleanup.stopErrors,
    };
  } finally {
    // Cleanup is deliberately non-throwing and track-first. A recorder failure
    // must never prevent release of an already acquired microphone stream.
    cleanup = stopTracksSafely(stream);
    stopRecorderSafely(recorder);
  }
}
