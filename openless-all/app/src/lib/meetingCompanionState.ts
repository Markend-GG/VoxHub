import type { MeetingRecordingSnapshot } from './types';

export type MeetingCompanionVisualState =
  | 'hidden'
  | 'idle'
  | 'recording'
  | 'quiet'
  | 'paused'
  | 'processing'
  | 'completed';

export type MeetingCompanionErrorKind =
  | 'transcribing_interrupted'
  | 'summary_failed'
  | 'command_failed';

export interface MeetingCompanionErrorOverlay {
  kind: MeetingCompanionErrorKind;
  message: string | null;
}

export interface MeetingCompanionState {
  meetingId: string | null;
  snapshot: MeetingRecordingSnapshot | null;
  visualState: MeetingCompanionVisualState;
  quiet: boolean;
  idlePending: boolean;
  stopAccepted: boolean;
  summaryOutcome: 'completed' | 'failed' | null;
  hiddenByUser: boolean;
  error: MeetingCompanionErrorOverlay | null;
}

export type MeetingCompanionAction =
  | { type: 'snapshot'; snapshot: MeetingRecordingSnapshot | null }
  | { type: 'idle-finished'; meetingId: string }
  | { type: 'quiet-changed'; meetingId: string; quiet: boolean }
  | { type: 'stop-accepted'; meetingId: string }
  | { type: 'summary-succeeded'; meetingId: string }
  | { type: 'summary-failed'; meetingId: string; message?: string | null }
  | { type: 'command-failed'; meetingId: string; message: string }
  | { type: 'clear-error'; meetingId: string }
  | { type: 'hide'; meetingId: string }
  | { type: 'show'; meetingId: string };

export const initialMeetingCompanionState: MeetingCompanionState = {
  meetingId: null,
  snapshot: null,
  visualState: 'hidden',
  quiet: false,
  idlePending: false,
  stopAccepted: false,
  summaryOutcome: null,
  hiddenByUser: false,
  error: null,
};

function isRecordingSnapshot(snapshot: MeetingRecordingSnapshot): boolean {
  return snapshot.phase === 'starting'
    || snapshot.phase === 'recording'
    || snapshot.phase === 'transcribing_interrupted';
}

function deriveVisualState(state: MeetingCompanionState): MeetingCompanionVisualState {
  if (state.hiddenByUser) return 'hidden';
  if (state.summaryOutcome === 'completed') return 'completed';
  if (state.summaryOutcome === 'failed' || state.stopAccepted) return 'processing';

  const snapshot = state.snapshot;
  if (!snapshot) return 'hidden';
  if (snapshot.meeting.status === 'summary_failed') return 'processing';
  if (snapshot.meeting.status === 'completed') return 'completed';
  if (snapshot.meeting.status === 'summarizing' || snapshot.phase === 'stopping') {
    return 'processing';
  }
  if (snapshot.phase === 'paused' || snapshot.meeting.status === 'paused') return 'paused';
  if (isRecordingSnapshot(snapshot)) {
    if (state.idlePending) return 'idle';
    return state.quiet ? 'quiet' : 'recording';
  }
  return 'hidden';
}

function withDerivedVisualState(state: MeetingCompanionState): MeetingCompanionState {
  return { ...state, visualState: deriveVisualState(state) };
}

function snapshotError(
  snapshot: MeetingRecordingSnapshot,
  previous: MeetingCompanionErrorOverlay | null,
): MeetingCompanionErrorOverlay | null {
  if (snapshot.meeting.status === 'summary_failed') {
    return { kind: 'summary_failed', message: null };
  }
  if (snapshot.phase === 'transcribing_interrupted' || snapshot.asrInterrupted) {
    return { kind: 'transcribing_interrupted', message: null };
  }
  if (previous?.kind === 'command_failed') return previous;
  return null;
}

function matchesMeeting(state: MeetingCompanionState, meetingId: string): boolean {
  return state.meetingId === meetingId;
}

export function meetingCompanionReducer(
  state: MeetingCompanionState,
  action: MeetingCompanionAction,
): MeetingCompanionState {
  if (action.type === 'snapshot') {
    if (!action.snapshot) {
      if (state.stopAccepted || state.summaryOutcome) {
        return withDerivedVisualState({ ...state, snapshot: null });
      }
      return initialMeetingCompanionState;
    }

    const snapshot = action.snapshot;
    const meetingId = snapshot.meeting.id;
    const isNewMeeting = meetingId !== state.meetingId;
    const previousError = isNewMeeting ? null : state.error;
    const summaryOutcome = snapshot.meeting.status === 'completed'
      ? 'completed'
      : snapshot.meeting.status === 'summary_failed'
        ? 'failed'
        : null;
    const next: MeetingCompanionState = {
      ...(isNewMeeting ? initialMeetingCompanionState : state),
      meetingId,
      snapshot,
      quiet: isNewMeeting || !isRecordingSnapshot(snapshot) ? false : state.quiet,
      idlePending: isNewMeeting ? isRecordingSnapshot(snapshot) : state.idlePending && isRecordingSnapshot(snapshot),
      stopAccepted: false,
      summaryOutcome,
      hiddenByUser: snapshot.meeting.status === 'summary_failed'
        ? false
        : isNewMeeting
          ? false
          : state.hiddenByUser,
      error: snapshotError(snapshot, previousError),
    };
    return withDerivedVisualState(next);
  }

  if (!matchesMeeting(state, action.meetingId)) return state;

  switch (action.type) {
    case 'idle-finished':
      return withDerivedVisualState({ ...state, idlePending: false });
    case 'quiet-changed':
      return withDerivedVisualState({ ...state, quiet: action.quiet });
    case 'stop-accepted':
      return withDerivedVisualState({ ...state, stopAccepted: true, summaryOutcome: null });
    case 'summary-succeeded':
      return withDerivedVisualState({
        ...state,
        stopAccepted: false,
        summaryOutcome: 'completed',
        hiddenByUser: false,
        error: null,
      });
    case 'summary-failed':
      return withDerivedVisualState({
        ...state,
        stopAccepted: false,
        summaryOutcome: 'failed',
        hiddenByUser: false,
        error: { kind: 'summary_failed', message: action.message ?? null },
      });
    case 'command-failed':
      return withDerivedVisualState({
        ...state,
        stopAccepted: false,
        error: { kind: 'command_failed', message: action.message },
      });
    case 'clear-error':
      return { ...state, error: null };
    case 'hide':
      return withDerivedVisualState({ ...state, hiddenByUser: true });
    case 'show':
      return withDerivedVisualState({ ...state, hiddenByUser: false });
  }
}
