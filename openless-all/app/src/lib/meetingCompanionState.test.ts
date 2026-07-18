import {
  initialMeetingCompanionState,
  meetingCompanionReducer,
  type MeetingCompanionAction,
  type MeetingCompanionState,
  type MeetingCompanionVisualState,
} from './meetingCompanionState';
import type { MeetingRecord, MeetingRecordingPhase, MeetingRecordingSnapshot, MeetingStatus } from './types';

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function snapshot(
  meetingId: string,
  phase: MeetingRecordingPhase,
  status: MeetingStatus = phase === 'paused' ? 'paused' : 'recording',
  asrInterrupted = false,
): MeetingRecordingSnapshot {
  const meeting: MeetingRecord = {
    id: meetingId,
    title: 'Test meeting',
    status,
    startedAt: '2026-07-18T00:00:00.000Z',
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
    elapsedMs: 1000,
    activeAsrProvider: 'test-asr',
    activeProviderSessionId: null,
    asrInterrupted,
  };
}

function reduce(
  state: MeetingCompanionState,
  action: MeetingCompanionAction,
  expected: MeetingCompanionVisualState,
  label: string,
): MeetingCompanionState {
  const next = meetingCompanionReducer(state, action);
  assert(next.visualState === expected, `${label}: expected ${expected}, got ${next.visualState}`);
  return next;
}

assert(initialMeetingCompanionState.visualState === 'hidden', 'initial state must be hidden');

let state = reduce(
  initialMeetingCompanionState,
  { type: 'snapshot', snapshot: snapshot('meeting-a', 'recording') },
  'idle',
  'new recording starts with idle',
);
state = reduce(state, { type: 'idle-finished', meetingId: 'meeting-a' }, 'recording', 'idle completes');
state = reduce(state, { type: 'quiet-changed', meetingId: 'meeting-a', quiet: true }, 'quiet', 'quiet visual');
state = reduce(
  state,
  { type: 'snapshot', snapshot: snapshot('meeting-a', 'paused') },
  'paused',
  'backend pause overrides quiet',
);
assert(state.quiet === false, 'paused backend snapshot must reset quiet state');

state = reduce(state, { type: 'stop-accepted', meetingId: 'meeting-a' }, 'processing', 'accepted stop');
state = reduce(
  state,
  { type: 'snapshot', snapshot: snapshot('meeting-a', 'paused') },
  'paused',
  'backend snapshot overrides local stop state',
);
assert(state.stopAccepted === false, 'authoritative snapshot must clear local stop state');

state = reduce(
  state,
  { type: 'snapshot', snapshot: snapshot('meeting-a', 'stopping', 'summarizing') },
  'processing',
  'summarizing snapshot',
);
state = reduce(state, { type: 'summary-succeeded', meetingId: 'meeting-a' }, 'completed', 'summary success');

state = reduce(
  state,
  { type: 'snapshot', snapshot: snapshot('meeting-b', 'transcribing_interrupted', 'transcribing_interrupted', true) },
  'idle',
  'new interrupted meeting starts with idle',
);
state = reduce(state, { type: 'idle-finished', meetingId: 'meeting-b' }, 'recording', 'interrupted recording');
assert(state.error?.kind === 'transcribing_interrupted', 'ASR interruption must be an overlay, not a seventh visual state');

const beforeStaleAction = state;
state = meetingCompanionReducer(state, { type: 'summary-succeeded', meetingId: 'meeting-a' });
assert(state === beforeStaleAction, 'events from an old meeting must be ignored');

state = reduce(state, { type: 'summary-failed', meetingId: 'meeting-b', message: 'failed' }, 'processing', 'summary failure');
assert(state.error?.kind === 'summary_failed', 'summary failure must expose an error overlay');
state = reduce(state, { type: 'hide', meetingId: 'meeting-b' }, 'hidden', 'manual hide');
state = reduce(state, { type: 'show', meetingId: 'meeting-b' }, 'processing', 'manual show restores state');

state = reduce(state, { type: 'snapshot', snapshot: null }, 'processing', 'summary outcome survives empty active snapshot');
state = reduce(
  state,
  { type: 'snapshot', snapshot: snapshot('meeting-c', 'recording') },
  'idle',
  'new backend meeting replaces previous outcome',
);
assert(state.meetingId === 'meeting-c', 'new backend snapshot must become authoritative');
state = meetingCompanionReducer(state, {
  type: 'command-failed',
  meetingId: 'meeting-c',
  message: 'failed',
});
assert(state.error?.kind === 'command_failed', 'command failure must be visible');
state = meetingCompanionReducer(state, {
  type: 'snapshot',
  snapshot: snapshot('meeting-c', 'recording'),
});
assert(state.error === null, 'a newer authoritative snapshot must clear a stale command error');

console.log('meetingCompanionState tests: OK');
