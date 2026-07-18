import {
  MEETING_COMPANION_MEDIA,
  MeetingCompanionMediaController,
  shouldPlayMeetingCompanionVideo,
  shouldShowMeetingCompanionMinimalFallback,
  type MeetingCompanionMediaFallbackReason,
  type MeetingCompanionTimeoutScheduler,
  type MeetingCompanionVideoElement,
} from './meetingCompanionMedia';
import {
  formatMeetingCompanionElapsed,
  MeetingCompanionElapsedClock,
} from './meetingCompanionTimer';

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function equal<T>(actual: T, expected: T, message: string): void {
  assert(Object.is(actual, expected), `${message}: expected ${String(expected)}, got ${String(actual)}`);
}

class FakeScheduler implements MeetingCompanionTimeoutScheduler {
  private nowMs = 0;
  private nextId = 1;
  private tasks = new Map<number, { dueAt: number; callback: () => void }>();

  setTimeout(callback: () => void, delayMs: number): unknown {
    const id = this.nextId++;
    this.tasks.set(id, { dueAt: this.nowMs + delayMs, callback });
    return id;
  }

  clearTimeout(handle: unknown): void {
    this.tasks.delete(handle as number);
  }

  advanceBy(deltaMs: number): void {
    this.nowMs += deltaMs;
    const due = [...this.tasks.entries()]
      .filter(([, task]) => task.dueAt <= this.nowMs)
      .sort((left, right) => left[1].dueAt - right[1].dueAt);
    for (const [id, task] of due) {
      if (!this.tasks.delete(id)) continue;
      task.callback();
    }
  }

  pendingCount(): number {
    return this.tasks.size;
  }
}

class FakeVideo implements MeetingCompanionVideoElement {
  src = '';
  loop = false;
  muted = false;
  playsInline = false;
  preload = '';
  currentTime = 12;
  pauseCalls = 0;
  playCalls = 0;
  loadCalls = 0;
  removeSrcCalls = 0;
  playImplementation: () => Promise<void> = () => Promise.resolve();
  private listeners = new Map<string, Set<EventListenerOrEventListenerObject>>();

  addEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
    this.listeners.get(type)?.delete(listener);
  }

  pause(): void {
    this.pauseCalls += 1;
  }

  play(): Promise<void> {
    this.playCalls += 1;
    return this.playImplementation();
  }

  load(): void {
    this.loadCalls += 1;
  }

  removeAttribute(name: string): void {
    if (name !== 'src') return;
    this.removeSrcCalls += 1;
    this.src = '';
  }

  emit(type: string): void {
    for (const listener of [...(this.listeners.get(type) ?? [])]) {
      if (typeof listener === 'function') listener({ type } as Event);
      else listener.handleEvent({ type } as Event);
    }
  }
}

function startMedia(
  controller: MeetingCompanionMediaController,
  video: FakeVideo,
  callbacks: { ready: number; fallbacks: MeetingCompanionMediaFallbackReason[] },
  source = 'recording.webm',
): void {
  controller.start({
    video,
    source,
    loop: true,
    onReady: () => { callbacks.ready += 1; },
    onFallback: reason => { callbacks.fallbacks.push(reason); },
  });
}

async function flushPromises(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

const states = Object.values(MEETING_COMPANION_MEDIA);
equal(states.length, 6, 'runtime manifest state count');
equal(MEETING_COMPANION_MEDIA.idle.loop, false, 'idle must not loop');
equal(MEETING_COMPANION_MEDIA.completed.loop, false, 'completed must not loop');
for (const state of ['recording', 'quiet', 'paused', 'processing'] as const) {
  equal(MEETING_COMPANION_MEDIA[state].loop, true, `${state} must loop`);
}
equal(new Set(states.map(entry => entry.webm)).size, 6, 'each state must map to one WebM');
equal(new Set(states.map(entry => entry.poster)).size, 6, 'each state must map to one poster');

assert(shouldPlayMeetingCompanionVideo('recording', false, true), 'visible motion path must play video');
assert(!shouldPlayMeetingCompanionVideo('hidden', false, true), 'hidden state must not play video');
assert(!shouldPlayMeetingCompanionVideo('recording', true, true), 'reduced motion must not play video');
assert(!shouldPlayMeetingCompanionVideo('recording', false, false), 'hidden document must not play video');
assert(shouldShowMeetingCompanionMinimalFallback(true, false), 'poster failure must show fallback before video');
assert(!shouldShowMeetingCompanionMinimalFallback(true, true), 'ready video can replace failed poster');

{
  const scheduler = new FakeScheduler();
  const controller = new MeetingCompanionMediaController(scheduler);
  const video = new FakeVideo();
  const callbacks = { ready: 0, fallbacks: [] as MeetingCompanionMediaFallbackReason[] };
  startMedia(controller, video, callbacks);
  equal(video.loop, true, 'loop configuration is applied to video');
  video.emit('canplay');
  await flushPromises();
  equal(video.playCalls, 1, 'canplay starts playback once');
  equal(callbacks.ready, 1, 'canplay marks current media ready');
  equal(callbacks.fallbacks.length, 0, 'canplay success does not fall back');
  equal(scheduler.pendingCount(), 0, 'canplay clears load timeout');
  controller.stop();
  equal(video.pauseCalls, 1, 'stop pauses active video');
  equal(video.currentTime, 0, 'stop resets current time');
  equal(video.removeSrcCalls, 1, 'stop removes media src');
  equal(video.loadCalls, 2, 'stop reloads video after src removal');
}

{
  const scheduler = new FakeScheduler();
  const controller = new MeetingCompanionMediaController(scheduler);
  const video = new FakeVideo();
  const callbacks = { ready: 0, fallbacks: [] as MeetingCompanionMediaFallbackReason[] };
  controller.start({
    video,
    source: 'idle.webm',
    loop: MEETING_COMPANION_MEDIA.idle.loop,
    onReady: () => { callbacks.ready += 1; },
    onFallback: reason => { callbacks.fallbacks.push(reason); },
  });
  equal(video.loop, false, 'idle non-loop configuration is applied to video');
  controller.stop();
}

{
  const scheduler = new FakeScheduler();
  const controller = new MeetingCompanionMediaController(scheduler);
  const video = new FakeVideo();
  const callbacks = { ready: 0, fallbacks: [] as MeetingCompanionMediaFallbackReason[] };
  startMedia(controller, video, callbacks);
  scheduler.advanceBy(1999);
  equal(callbacks.fallbacks.length, 0, 'load timeout waits for two seconds');
  scheduler.advanceBy(1);
  equal(callbacks.fallbacks[0], 'load-timeout', 'two-second timeout falls back');
  equal(video.removeSrcCalls, 1, 'timeout releases decoder');
}

{
  const scheduler = new FakeScheduler();
  const controller = new MeetingCompanionMediaController(scheduler);
  const video = new FakeVideo();
  const callbacks = { ready: 0, fallbacks: [] as MeetingCompanionMediaFallbackReason[] };
  startMedia(controller, video, callbacks);
  video.emit('error');
  equal(callbacks.fallbacks[0], 'media-error', 'media error falls back');
  equal(video.removeSrcCalls, 1, 'media error releases decoder');
}

{
  const scheduler = new FakeScheduler();
  const controller = new MeetingCompanionMediaController(scheduler);
  const video = new FakeVideo();
  const callbacks = { ready: 0, fallbacks: [] as MeetingCompanionMediaFallbackReason[] };
  startMedia(controller, video, callbacks);
  video.emit('canplay');
  await flushPromises();
  video.emit('error');
  equal(callbacks.ready, 1, 'video becomes ready before runtime error');
  equal(callbacks.fallbacks[0], 'media-error', 'runtime playback error falls back');
  equal(video.removeSrcCalls, 1, 'runtime playback error releases decoder');
}

{
  const scheduler = new FakeScheduler();
  const controller = new MeetingCompanionMediaController(scheduler);
  const video = new FakeVideo();
  video.playImplementation = () => Promise.reject(new Error('autoplay denied'));
  const callbacks = { ready: 0, fallbacks: [] as MeetingCompanionMediaFallbackReason[] };
  startMedia(controller, video, callbacks);
  video.emit('canplay');
  await flushPromises();
  equal(callbacks.fallbacks[0], 'play-rejected', 'play rejection falls back');
  equal(video.removeSrcCalls, 1, 'play rejection releases decoder');
}

{
  const scheduler = new FakeScheduler();
  const controller = new MeetingCompanionMediaController(scheduler);
  const first = new FakeVideo();
  const second = new FakeVideo();
  let rejectFirst!: (error: Error) => void;
  first.playImplementation = () => new Promise((_, reject) => { rejectFirst = reject; });
  const firstCallbacks = { ready: 0, fallbacks: [] as MeetingCompanionMediaFallbackReason[] };
  const secondCallbacks = { ready: 0, fallbacks: [] as MeetingCompanionMediaFallbackReason[] };
  startMedia(controller, first, firstCallbacks, 'idle.webm');
  first.emit('canplay');
  startMedia(controller, second, secondCallbacks, 'recording.webm');
  rejectFirst(new Error('stale rejection'));
  second.emit('canplay');
  await flushPromises();
  equal(first.removeSrcCalls, 1, 'state switch releases old video');
  equal(firstCallbacks.ready, 0, 'old play result cannot mark new state ready');
  equal(firstCallbacks.fallbacks.length, 0, 'old play rejection cannot overwrite new state');
  equal(secondCallbacks.ready, 1, 'new state remains active');
  equal(second.src, 'recording.webm', 'only new video keeps a source');
  controller.stop();
  equal(second.removeSrcCalls, 1, 'unmount releases current video');
  equal(scheduler.pendingCount(), 0, 'unmount clears media timers');
}

const clock = new MeetingCompanionElapsedClock();
equal(clock.align({ meetingId: 'a', elapsedMs: 10_000, running: true }, 100), 10_000, 'timer aligns to snapshot');
equal(clock.read(1_600), 11_500, 'running timer interpolates with monotonic clock');
equal(clock.align({ meetingId: 'a', elapsedMs: 12_000, running: false }, 2_000), 12_000, 'pause realigns timer');
equal(clock.read(20_000), 12_000, 'paused timer freezes');
equal(clock.align({ meetingId: 'a', elapsedMs: 30_000, running: true }, 30_000), 30_000, 'new snapshot realigns baseline');
equal(clock.read(31_000), 31_000, 'realigned timer resumes interpolation');
equal(clock.align({ meetingId: 'a', elapsedMs: 31_500, running: false }, 31_500), 31_500, 'stopped timer keeps final snapshot');
equal(clock.read(39_000), 31_500, 'stopped timer does not continue locally');
equal(clock.align({ meetingId: 'b', elapsedMs: 2_000, running: false }, 40_000), 2_000, 'meeting id switch clears old elapsed time');
equal(clock.currentMeetingId(), 'b', 'timer tracks current meeting id');
equal(clock.align(null, 50_000), 0, 'hidden or unmounted timer clears');
equal(clock.currentMeetingId(), null, 'cleared timer drops meeting id');

equal(formatMeetingCompanionElapsed(0), '00:00', 'zero duration format');
equal(formatMeetingCompanionElapsed(3_599_000), '59:59', 'sub-hour duration format');
equal(formatMeetingCompanionElapsed(3_600_000), '01:00:00', 'one-hour duration format');
equal(formatMeetingCompanionElapsed(7_445_000), '02:04:05', 'multi-hour duration format');

console.log('meetingCompanionRuntime tests: OK');
