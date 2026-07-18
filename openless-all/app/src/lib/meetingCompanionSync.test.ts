import { MEETING_COMPANION_MEDIA } from './meetingCompanionMedia';
import {
  initialMeetingCompanionState,
  meetingCompanionReducer,
} from './meetingCompanionState';
import {
  MEETING_COMPANION_COMPLETED_HOLD_MS,
  MeetingCompanionEventGate,
  MeetingCompanionQuietDetector,
  MeetingCompanionTransitionTimer,
  type MeetingCompanionTransitionScheduler,
} from './meetingCompanionSync';
import type {
  MeetingAudioLevelEvent,
  MeetingRecord,
  MeetingRecordingPhase,
  MeetingRecordingSnapshot,
  MeetingStatus,
} from './types';

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function equal<T>(actual: T, expected: T, message: string): void {
  assert(Object.is(actual, expected), `${message}: expected ${String(expected)}, got ${String(actual)}`);
}

function snapshot(
  meetingId: string,
  phase: MeetingRecordingPhase = 'recording',
  status: MeetingStatus = phase === 'paused' ? 'paused' : 'recording',
  startedAt = '2026-07-18T00:00:00.000Z',
): MeetingRecordingSnapshot {
  const meeting: MeetingRecord = {
    id: meetingId,
    title: 'Test meeting',
    status,
    startedAt,
    endedAt: null,
    durationMs: null,
    transcriptSegments: [],
    summary: { overview: '', keyDecisions: [], todos: [], risksAndOpenQuestions: [] },
    audio: { state: 'temporary', retained: false, path: null },
    createdAt: '2026-07-18T00:00:00.000Z',
    updatedAt: '2026-07-18T00:00:00.000Z',
  };
  return {
    meeting,
    phase,
    elapsedMs: 1_000,
    activeAsrProvider: 'test',
    activeProviderSessionId: null,
    asrInterrupted: phase === 'transcribing_interrupted',
  };
}

{
  const gate = new MeetingCompanionEventGate();
  const initialization = gate.beginInitialization();
  assert(
    gate.acceptInitialization(
      initialization,
      snapshot('meeting-b', 'recording', 'recording', '2026-07-18T00:01:00.000Z'),
    ),
    'initialization establishes the active meeting',
  );
  assert(
    !gate.acceptSnapshotEvent(
      snapshot('meeting-a', 'recording', 'recording', '2026-07-18T00:00:00.000Z'),
    ),
    'late snapshot from an older meeting is ignored',
  );
  equal(gate.currentMeetingId(), 'meeting-b', 'older snapshot cannot replace current meeting');
  assert(
    gate.acceptSnapshotEvent(
      snapshot('meeting-c', 'recording', 'recording', '2026-07-18T00:02:00.000Z'),
    ),
    'newer meeting snapshot can replace current meeting',
  );
  assert(!gate.acceptRelatedEvent('meeting-b'), 'retired meeting events stay rejected');
}

function level(meetingId: string, value: number): MeetingAudioLevelEvent {
  return { meetingId, level: value };
}

{
  const detector = new MeetingCompanionQuietDetector();
  detector.setContext('meeting-a', true);
  equal(detector.sample(level('meeting-a', 0.02), 0).quiet, false, 'low level starts enter window');
  equal(detector.sample(level('meeting-a', 0.02), 1_999).quiet, false, 'quiet waits 2000ms');
  const entered = detector.sample(level('meeting-a', 0.02), 2_000);
  assert(entered.changed && entered.quiet, 'continuous low level enters quiet at 2000ms');

  equal(detector.sample(level('meeting-a', 0.06), 2_100).quiet, true, 'high level starts exit window');
  equal(detector.sample(level('meeting-a', 0.06), 2_249).quiet, true, 'quiet waits 150ms to exit');
  const exited = detector.sample(level('meeting-a', 0.06), 2_250);
  assert(exited.changed && !exited.quiet, 'continuous high level exits quiet at 150ms');
}

{
  const detector = new MeetingCompanionQuietDetector();
  detector.setContext('meeting-a', true);
  detector.sample(level('meeting-a', 0.02), 0);
  detector.sample(level('meeting-a', 0.04), 1_500);
  equal(detector.sample(level('meeting-a', 0.02), 2_100).quiet, false, 'middle band resets non-quiet low window');
  equal(detector.sample(level('meeting-a', 0.02), 4_100).quiet, true, 'new continuous low window enters quiet');
  equal(detector.sample(level('meeting-a', 0.04), 4_200).quiet, true, 'middle band keeps quiet state');
  detector.sample(level('meeting-a', 0.07), 4_300);
  detector.sample(level('meeting-a', 0.05), 4_400);
  equal(detector.sample(level('meeting-a', 0.07), 4_500).quiet, true, 'threshold jitter resets high exit window');
  equal(detector.sample(level('meeting-a', 0.07), 4_650).quiet, false, 'stable high window exits after jitter');
}

for (const reason of ['paused', 'hidden', 'stopped', 'new meeting']) {
  const detector = new MeetingCompanionQuietDetector();
  detector.setContext('meeting-a', true);
  detector.sample(level('meeting-a', 0.01), 0);
  detector.sample(level('meeting-a', 0.01), 2_000);
  const reset = reason === 'new meeting'
    ? detector.setContext('meeting-b', true)
    : detector.setContext('meeting-a', false);
  assert(reset.changed && !reset.quiet, `${reason} resets quiet state`);
}

{
  const detector = new MeetingCompanionQuietDetector();
  detector.setContext('meeting-b', true);
  assert(!detector.sample(level('meeting-a', 0), 10_000).accepted, 'old meeting level is ignored');
}

{
  const gate = new MeetingCompanionEventGate();
  const initialization = gate.beginInitialization();
  assert(gate.acceptSnapshotEvent(snapshot('meeting-b')), 'new event establishes meeting B');
  assert(!gate.acceptInitialization(initialization, snapshot('meeting-a')), 'old initialization cannot overwrite event');
  assert(!gate.acceptRelatedEvent('meeting-a'), 'old meeting event is ignored after switch');
  assert(gate.acceptRelatedEvent('meeting-b'), 'current meeting event is accepted');
}

{
  const gate = new MeetingCompanionEventGate();
  const initialization = gate.beginInitialization();
  assert(!gate.acceptRelatedEvent('meeting-old'), 'unrelated event is rejected during initialization');
  assert(
    gate.acceptInitialization(initialization, snapshot('meeting-current', 'paused', 'paused')),
    'rejected old event cannot invalidate a valid paused initialization',
  );
}

{
  const gate = new MeetingCompanionEventGate();
  assert(
    gate.acceptSnapshotEvent(
      snapshot('meeting-current', 'recording', 'recording', '2026-07-18T00:02:00.000Z'),
    ),
    'current meeting is established',
  );
  const initialization = gate.beginInitialization();
  assert(
    !gate.acceptSnapshotEvent(
      snapshot('meeting-old', 'recording', 'recording', '2026-07-18T00:01:00.000Z'),
    ),
    'older snapshot is rejected',
  );
  assert(
    gate.acceptInitialization(
      initialization,
      snapshot('meeting-current', 'paused', 'paused', '2026-07-18T00:02:00.000Z'),
    ),
    'rejected older snapshot cannot invalidate current initialization',
  );
  assert(
    !gate.acceptRelatedEvent(
      'meeting-old',
      true,
      '2026-07-18T00:01:00.000Z',
    ),
    'older summary event cannot replace the current meeting',
  );
  assert(
    gate.acceptRelatedEvent(
      'meeting-new',
      true,
      '2026-07-18T00:03:00.000Z',
    ),
    'newer summary event can replace the current meeting',
  );
}

{
  let state = meetingCompanionReducer(initialMeetingCompanionState, { type: 'snapshot', snapshot: snapshot('a') });
  equal(state.visualState, 'idle', 'new meeting starts idle');
  state = meetingCompanionReducer(state, { type: 'idle-finished', meetingId: 'a' });
  equal(state.visualState, 'recording', 'idle advances to recording');
  state = meetingCompanionReducer(state, { type: 'snapshot', snapshot: snapshot('a', 'paused') });
  equal(state.visualState, 'paused', 'paused snapshot wins');
  state = meetingCompanionReducer(state, { type: 'stop-accepted', meetingId: 'a' });
  equal(state.visualState, 'processing', 'stop accepted starts processing');
  state = meetingCompanionReducer(state, { type: 'summary-succeeded', meetingId: 'a' });
  equal(state.visualState, 'completed', 'summary success completes');
  state = meetingCompanionReducer(initialMeetingCompanionState, {
    type: 'summary-failed',
    meetingId: 'failed',
    message: 'failed',
  });
  equal(state.visualState, 'processing', 'summary failure keeps processing poster');
  equal(state.error?.kind, 'summary_failed', 'summary failure exposes overlay');
}

class FakeScheduler implements MeetingCompanionTransitionScheduler {
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
    for (const [id, task] of [...this.tasks]) {
      if (task.dueAt > this.nowMs || !this.tasks.delete(id)) continue;
      task.callback();
    }
  }
}

{
  const scheduler = new FakeScheduler();
  const timer = new MeetingCompanionTransitionTimer(scheduler);
  let completed = 0;
  scheduler.advanceBy(MEETING_COMPANION_MEDIA.completed.durationMs + 2_000);
  timer.schedule(MEETING_COMPANION_COMPLETED_HOLD_MS, () => { completed += 1; });
  scheduler.advanceBy(MEETING_COMPANION_COMPLETED_HOLD_MS - 1);
  equal(completed, 0, 'completed holds for three seconds after playback finishes');
  scheduler.advanceBy(1);
  equal(completed, 1, 'completed timer fires after hold');
  timer.schedule(MEETING_COMPANION_COMPLETED_HOLD_MS, () => { completed += 1; });
  timer.clear();
  scheduler.advanceBy(MEETING_COMPANION_COMPLETED_HOLD_MS);
  equal(completed, 1, 'completed timer cleanup prevents stale dismissal');
}

console.log('meetingCompanionSync tests: OK');
