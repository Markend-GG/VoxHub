export interface MeetingCompanionTimerInput {
  meetingId: string;
  elapsedMs: number;
  running: boolean;
}

function normalizeElapsedMs(elapsedMs: number): number {
  if (!Number.isFinite(elapsedMs)) return 0;
  return Math.max(0, elapsedMs);
}

export class MeetingCompanionElapsedClock {
  private meetingId: string | null = null;
  private baselineElapsedMs = 0;
  private baselineNowMs = 0;
  private running = false;

  align(input: MeetingCompanionTimerInput | null, nowMs: number): number {
    if (!input) {
      this.clear();
      return 0;
    }
    this.meetingId = input.meetingId;
    this.baselineElapsedMs = normalizeElapsedMs(input.elapsedMs);
    this.baselineNowMs = nowMs;
    this.running = input.running;
    return this.read(nowMs);
  }

  read(nowMs: number): number {
    if (!this.meetingId) return 0;
    if (!this.running) return this.baselineElapsedMs;
    return this.baselineElapsedMs + Math.max(0, nowMs - this.baselineNowMs);
  }

  currentMeetingId(): string | null {
    return this.meetingId;
  }

  clear(): void {
    this.meetingId = null;
    this.baselineElapsedMs = 0;
    this.baselineNowMs = 0;
    this.running = false;
  }
}

export function formatMeetingCompanionElapsed(elapsedMs: number): string {
  const totalSeconds = Math.floor(normalizeElapsedMs(elapsedMs) / 1000);
  const seconds = totalSeconds % 60;
  const totalMinutes = Math.floor(totalSeconds / 60);
  const minutes = totalMinutes % 60;
  const hours = Math.floor(totalMinutes / 60);
  const twoDigits = (value: number) => value.toString().padStart(2, '0');
  if (hours > 0) return `${twoDigits(hours)}:${twoDigits(minutes)}:${twoDigits(seconds)}`;
  return `${twoDigits(totalMinutes)}:${twoDigits(seconds)}`;
}
