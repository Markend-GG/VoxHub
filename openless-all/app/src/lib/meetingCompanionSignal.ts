import type {
  MeetingCompanionErrorKind,
  MeetingCompanionVisualState,
} from './meetingCompanionState';

export const MEETING_SIGNAL_IDLE_MS = 900;
export const MEETING_SIGNAL_COMPLETE_ANIMATION_MS = 720;
export const MEETING_SIGNAL_MAX_FPS = 30;

export type MeetingSignalMode = 'idle' | 'live' | 'paused' | 'processing' | 'completed' | 'error';

export function meetingSignalMode(
  state: MeetingCompanionVisualState,
  errorKind: MeetingCompanionErrorKind | null,
): MeetingSignalMode {
  if (errorKind === 'summary_failed') return 'error';
  if (state === 'recording' || state === 'quiet') return 'live';
  if (state === 'paused') return 'paused';
  if (state === 'processing') return 'processing';
  if (state === 'completed') return 'completed';
  return 'idle';
}

export function normalizeMeetingSignalLevel(rawLevel: number): number {
  if (!Number.isFinite(rawLevel)) return 0;
  const gate = 0.012;
  const ceiling = 0.34;
  const gated = Math.min(1, Math.max(0, (rawLevel - gate) / (ceiling - gate)));
  const eased = gated * gated * (3 - 2 * gated);
  return Math.pow(eased, 0.42);
}

export function meetingSignalModeCode(mode: MeetingSignalMode): number {
  if (mode === 'live') return 1;
  if (mode === 'paused') return 2;
  if (mode === 'processing') return 3;
  if (mode === 'completed') return 4;
  if (mode === 'error') return 5;
  return 0;
}

export function meetingSignalFallbackColor(mode: MeetingSignalMode): string {
  if (mode === 'paused') return '#f0ad4e';
  if (mode === 'processing') return '#77d6df';
  if (mode === 'completed') return '#6fe39a';
  if (mode === 'error') return '#ff6b63';
  return '#78dd91';
}
