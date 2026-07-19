import { en } from '../i18n/en';
import { ja } from '../i18n/ja';
import { ko } from '../i18n/ko';
import { zhCN } from '../i18n/zh-CN';
import { zhTW } from '../i18n/zh-TW';
import {
  clampMeetingCompanionMenuPosition,
  executeMeetingCompanionCommand,
  MEETING_COMPANION_CONTROLS_HIDE_DELAY_MS,
  MEETING_COMPANION_I18N_KEYS,
  MeetingCompanionCommandGate,
  MeetingCompanionControlsVisibility,
  meetingCompanionControlActions,
  meetingCompanionMenuActions,
  nextMeetingCompanionDialogFocusIndex,
  shouldStartMeetingCompanionDrag,
  type MeetingCompanionCommandCallbacks,
  type MeetingCompanionCommandDependencies,
  type MeetingCompanionScheduler,
} from './meetingCompanionControls';
import type { MeetingRecord, MeetingRecordingPhase, MeetingRecordingSnapshot } from './types';

const assert = {
  equal(actual: unknown, expected: unknown, message = 'values must be equal') {
    if (actual !== expected) throw new Error(`${message}: ${String(actual)} !== ${String(expected)}`);
  },
  notEqual(actual: unknown, expected: unknown, message = 'values must differ') {
    if (actual === expected) throw new Error(`${message}: ${String(actual)} === ${String(expected)}`);
  },
  deepEqual(actual: unknown, expected: unknown, message = 'values must be deeply equal') {
    const actualJson = JSON.stringify(actual);
    const expectedJson = JSON.stringify(expected);
    if (actualJson !== expectedJson) throw new Error(`${message}: ${actualJson} !== ${expectedJson}`);
  },
};

function snapshot(
  meetingId: string,
  phase: MeetingRecordingPhase = 'recording',
): MeetingRecordingSnapshot {
  const status = phase === 'paused' ? 'paused' : 'recording';
  return {
    meeting: {
      id: meetingId,
      title: 'Meeting',
      status,
      startedAt: '2026-07-18T00:00:00.000Z',
      endedAt: null,
      durationMs: null,
      transcriptSegments: [],
      summary: {
        overview: '',
        keyDecisions: [],
        todos: [],
        risksAndOpenQuestions: [],
      },
      audio: { state: 'temporary', retained: false, path: null },
      createdAt: '2026-07-18T00:00:00.000Z',
      updatedAt: '2026-07-18T00:00:00.000Z',
    },
    phase,
    elapsedMs: 10_000,
    activeAsrProvider: 'mock',
    activeProviderSessionId: 'session',
    asrInterrupted: false,
  };
}

function record(meetingId: string): MeetingRecord {
  return {
    ...snapshot(meetingId).meeting,
    status: 'summarizing',
    endedAt: '2026-07-18T00:01:00.000Z',
    durationMs: 60_000,
  };
}

function actions(values: ReadonlyArray<{ action: string }>): string[] {
  return values.map(value => value.action);
}

assert.deepEqual(actions(meetingCompanionControlActions('recording', null)), ['pause', 'stop']);
assert.deepEqual(actions(meetingCompanionControlActions('quiet', null)), ['pause', 'stop']);
assert.deepEqual(actions(meetingCompanionControlActions('paused', null)), ['resume', 'stop']);
assert.deepEqual(actions(meetingCompanionControlActions('processing', null)), []);
assert.deepEqual(actions(meetingCompanionControlActions('completed', null)), []);
assert.deepEqual(
  actions(meetingCompanionControlActions('processing', 'summary_failed')),
  ['open-meeting'],
);

assert.deepEqual(
  actions(meetingCompanionMenuActions('recording', null, false)),
  ['pause', 'stop', 'hide', 'toggle-position-lock'],
);
assert.deepEqual(
  actions(meetingCompanionMenuActions('paused', null, true)),
  ['resume', 'stop', 'hide', 'toggle-position-lock'],
);
assert.deepEqual(
  actions(meetingCompanionMenuActions('processing', 'summary_failed', false)),
  ['open-meeting', 'hide', 'toggle-position-lock'],
);
const lockedMenu = meetingCompanionMenuActions('paused', null, true);
assert.equal(
  lockedMenu[lockedMenu.length - 1]?.labelKey,
  'meetingCompanion.unlockPosition',
);

class FakeScheduler implements MeetingCompanionScheduler {
  callbacks = new Map<number, () => void>();
  delays = new Map<number, number>();
  nextId = 0;

  setTimeout(callback: () => void, delayMs: number): number {
    const id = ++this.nextId;
    this.callbacks.set(id, callback);
    this.delays.set(id, delayMs);
    return id;
  }

  clearTimeout(handle: unknown): void {
    this.callbacks.delete(handle as number);
  }

  fireAll(): void {
    const callbacks = [...this.callbacks.values()];
    this.callbacks.clear();
    callbacks.forEach(callback => callback());
  }
}

const scheduler = new FakeScheduler();
const visibilityChanges: boolean[] = [];
const visibility = new MeetingCompanionControlsVisibility(
  value => visibilityChanges.push(value),
  scheduler,
);
visibility.show();
visibility.scheduleHide();
assert.deepEqual([...scheduler.delays.values()], [MEETING_COMPANION_CONTROLS_HIDE_DELAY_MS]);
visibility.show();
scheduler.fireAll();
assert.deepEqual(visibilityChanges, [true], 'moving into controls cancels pending hide');
visibility.scheduleHide();
scheduler.fireAll();
assert.deepEqual(visibilityChanges, [true, false]);

assert.deepEqual(
  clampMeetingCompanionMenuPosition(330, 310, 160, 140, 350, 324),
  { x: 190, y: 184 },
);
assert.equal(
  shouldStartMeetingCompanionDrag(10, 10, 20, 20, false),
  false,
  'drag requires the primary button to remain pressed',
);
assert.equal(
  shouldStartMeetingCompanionDrag(10, 10, 12, 12, true),
  false,
  'movement below the threshold remains a click',
);
assert.equal(
  shouldStartMeetingCompanionDrag(10, 10, 13, 10, true),
  true,
  'movement at the threshold starts dragging',
);
assert.equal(
  shouldStartMeetingCompanionDrag(10, 10, 10, 10, true),
  false,
  'a stationary click must not start dragging',
);
assert.equal(nextMeetingCompanionDialogFocusIndex(1, 2, false), 0);
assert.equal(nextMeetingCompanionDialogFocusIndex(0, 2, true), 1);

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function commandHarness(currentMeetingId = 'a') {
  const applied: MeetingRecordingSnapshot[] = [];
  const acceptedStops: string[] = [];
  const failures: Array<{ snapshot: MeetingRecordingSnapshot | null; error: unknown }> = [];
  const busy: Array<string | null> = [];
  let current = currentMeetingId;
  const callbacks: MeetingCompanionCommandCallbacks = {
    currentMeetingId: () => current,
    setBusy: value => busy.push(value),
    applySnapshot: value => applied.push(value),
    acceptStop: value => acceptedStops.push(value),
    recoverFailure: (value, error) => failures.push({ snapshot: value, error }),
  };
  return {
    applied,
    acceptedStops,
    failures,
    busy,
    callbacks,
    setCurrent: (value: string) => { current = value; },
  };
}

function commandDependencies(
  overrides: Partial<MeetingCompanionCommandDependencies> = {},
): MeetingCompanionCommandDependencies {
  return {
    pause: async id => snapshot(id, 'paused'),
    resume: async id => snapshot(id, 'recording'),
    stop: async id => record(id),
    getActive: async () => snapshot('a'),
    ...overrides,
  };
}

{
  const gate = new MeetingCompanionCommandGate();
  const harness = commandHarness();
  assert.equal(
    await executeMeetingCompanionCommand(gate, 'pause', 'a', commandDependencies(), harness.callbacks),
    'success',
  );
  assert.equal(harness.applied[0]?.phase, 'paused');
  assert.deepEqual(harness.busy, ['pause', null]);
}

{
  const pause = deferred<MeetingRecordingSnapshot>();
  const gate = new MeetingCompanionCommandGate();
  const harness = commandHarness();
  const dependencies = commandDependencies({ pause: () => pause.promise });
  const first = executeMeetingCompanionCommand(gate, 'pause', 'a', dependencies, harness.callbacks);
  assert.equal(
    await executeMeetingCompanionCommand(gate, 'pause', 'a', dependencies, harness.callbacks),
    'duplicate',
  );
  pause.resolve(snapshot('a', 'paused'));
  assert.equal(await first, 'success');
  assert.equal(harness.applied.length, 1);
}

{
  const pause = deferred<MeetingRecordingSnapshot>();
  const gate = new MeetingCompanionCommandGate();
  const harness = commandHarness();
  const running = executeMeetingCompanionCommand(
    gate,
    'pause',
    'a',
    commandDependencies({ pause: () => pause.promise }),
    harness.callbacks,
  );
  gate.invalidate();
  pause.resolve(snapshot('a', 'paused'));
  assert.equal(await running, 'stale', 'main-window state event invalidates a late result');
  assert.equal(harness.applied.length, 0);
}

{
  const pause = deferred<MeetingRecordingSnapshot>();
  const gate = new MeetingCompanionCommandGate();
  const harness = commandHarness();
  const running = executeMeetingCompanionCommand(
    gate,
    'pause',
    'a',
    commandDependencies({ pause: () => pause.promise }),
    harness.callbacks,
  );
  harness.setCurrent('b');
  pause.resolve(snapshot('a', 'paused'));
  assert.equal(await running, 'stale', 'old meeting result cannot overwrite a new meeting');
}

{
  const gate = new MeetingCompanionCommandGate();
  const harness = commandHarness();
  const failure = new Error('pause failed');
  const outcome = await executeMeetingCompanionCommand(
    gate,
    'pause',
    'a',
    commandDependencies({
      pause: async () => { throw failure; },
      getActive: async () => snapshot('a', 'recording'),
    }),
    harness.callbacks,
  );
  assert.equal(outcome, 'failure');
  assert.equal(harness.failures[0]?.snapshot?.phase, 'recording');
  assert.equal(harness.failures[0]?.error, failure);
}

{
  const gate = new MeetingCompanionCommandGate();
  const harness = commandHarness();
  const outcome = await executeMeetingCompanionCommand(
    gate,
    'stop',
    'a',
    commandDependencies({ stop: async () => { throw new Error('stop failed'); } }),
    harness.callbacks,
  );
  assert.equal(outcome, 'failure');
  assert.deepEqual(harness.acceptedStops, [], 'failed stop must not enter processing');
}

{
  const gate = new MeetingCompanionCommandGate();
  const harness = commandHarness();
  assert.equal(
    await executeMeetingCompanionCommand(gate, 'stop', 'a', commandDependencies(), harness.callbacks),
    'success',
  );
  assert.deepEqual(harness.acceptedStops, ['a']);
}

const locales = [zhCN, zhTW, en, ja, ko];
for (const locale of locales) {
  for (const key of MEETING_COMPANION_I18N_KEYS) {
    assert.equal(typeof locale.meetingCompanion[key], 'string', `missing meetingCompanion.${key}`);
    assert.notEqual(locale.meetingCompanion[key].trim(), '', `empty meetingCompanion.${key}`);
  }
}

console.log('meetingCompanionControls tests: OK');
