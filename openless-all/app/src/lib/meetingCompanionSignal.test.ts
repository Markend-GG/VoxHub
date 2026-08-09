import {
  MEETING_SIGNAL_COMPLETE_ANIMATION_MS,
  MEETING_SIGNAL_IDLE_MS,
  MEETING_SIGNAL_MAX_FPS,
  meetingSignalFallbackColor,
  meetingSignalMode,
  meetingSignalModeCode,
  normalizeMeetingSignalLevel,
} from './meetingCompanionSignal';

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function equal(actual: unknown, expected: unknown, message: string): void {
  assert(actual === expected, `${message}: expected ${String(expected)}, got ${String(actual)}`);
}

equal(meetingSignalMode('idle', null), 'idle', 'idle mode');
equal(meetingSignalMode('recording', null), 'live', 'recording mode');
equal(meetingSignalMode('quiet', null), 'live', 'quiet mode');
equal(meetingSignalMode('paused', null), 'paused', 'paused mode');
equal(meetingSignalMode('processing', null), 'processing', 'processing mode');
equal(meetingSignalMode('completed', null), 'completed', 'completed mode');
equal(meetingSignalMode('processing', 'summary_failed'), 'error', 'summary failure overrides processing');

equal(normalizeMeetingSignalLevel(Number.NaN), 0, 'invalid level is silent');
equal(normalizeMeetingSignalLevel(-1), 0, 'negative level is clamped');
equal(normalizeMeetingSignalLevel(0.012), 0, 'noise gate stays dark');
equal(normalizeMeetingSignalLevel(1), 1, 'high level is clamped');
assert(normalizeMeetingSignalLevel(0.08) > 0, 'normal speech produces a visible signal');
assert(normalizeMeetingSignalLevel(0.2) > normalizeMeetingSignalLevel(0.08), 'signal is monotonic');

for (const mode of ['idle', 'live', 'paused', 'processing', 'completed', 'error'] as const) {
  assert(Number.isInteger(meetingSignalModeCode(mode)), `${mode} mode code must be an integer`);
  assert(meetingSignalFallbackColor(mode).startsWith('#'), `${mode} fallback must have a color`);
}

equal(MEETING_SIGNAL_MAX_FPS, 30, 'render cap');
assert(MEETING_SIGNAL_IDLE_MS > 0, 'idle transition has a finite duration');
assert(MEETING_SIGNAL_COMPLETE_ANIMATION_MS > 0, 'completion animation has a finite duration');

console.log('meetingCompanionSignal tests: OK');
