import test from 'node:test';
import assert from 'node:assert/strict';
import {
  CAPTURE_WINDOW_MS,
  recordStreamForProbe,
} from '../capture-lifecycle.mjs';

class FakeTrack {
  readyState = 'live';

  stop() {
    this.readyState = 'ended';
  }
}

class FakeStream {
  constructor(track = new FakeTrack()) {
    this.track = track;
  }

  getTracks() {
    return [this.track];
  }
}

class BaseRecorder extends EventTarget {
  state = 'inactive';
  mimeType = 'audio/webm';

  start() {
    this.state = 'recording';
  }

  emitDataAndStop() {
    const data = new Event('dataavailable');
    data.data = { size: 321 };
    this.dispatchEvent(data);
    this.state = 'inactive';
    this.dispatchEvent(new Event('stop'));
  }
}

test('deadline stops tracks before waiting for recorder finalization', async () => {
  const stream = new FakeStream();
  let trackWasEndedWhenStopReturned = false;

  class Recorder extends BaseRecorder {
    stop() {
      this.state = 'inactive';
      queueMicrotask(() => {
        trackWasEndedWhenStopReturned = stream.track.readyState === 'ended';
        this.emitDataAndStop();
      });
    }
  }

  const result = await recordStreamForProbe({
    stream,
    MediaRecorderConstructor: Recorder,
    durationMs: 0,
    finalizationTimeoutMs: 20,
  });

  assert.equal(trackWasEndedWhenStopReturned, true);
  assert.equal(result.audioBytes, 321);
  assert.equal(result.tracksEndedAfterCleanup, true);
});

test('missing recorder stop event times out after tracks are released', async () => {
  const stream = new FakeStream();

  class Recorder extends BaseRecorder {
    stop() {
      this.state = 'inactive';
    }
  }

  await assert.rejects(
    recordStreamForProbe({
      stream,
      MediaRecorderConstructor: Recorder,
      durationMs: 0,
      finalizationTimeoutMs: 0,
    }),
    { name: 'MediaRecorderStopTimeout' },
  );
  assert.equal(stream.track.readyState, 'ended');
});

test('recorder stop exceptions cannot skip track cleanup', async () => {
  const stream = new FakeStream();

  class Recorder extends BaseRecorder {
    stop() {
      throw new Error('stop exploded');
    }
  }

  await assert.rejects(
    recordStreamForProbe({
      stream,
      MediaRecorderConstructor: Recorder,
      durationMs: 0,
      finalizationTimeoutMs: 0,
    }),
    { name: 'MediaRecorderStopError' },
  );
  assert.equal(stream.track.readyState, 'ended');
});

test('capture duration cannot exceed the probe hard window', async () => {
  await assert.rejects(
    recordStreamForProbe({
      stream: new FakeStream(),
      MediaRecorderConstructor: BaseRecorder,
      durationMs: CAPTURE_WINDOW_MS + 1,
    }),
    RangeError,
  );
});
