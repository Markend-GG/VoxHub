import type { MeetingAudioLevelEvent, MeetingRecordingSnapshot } from './types';

export const MEETING_COMPANION_QUIET_ENTER_LEVEL = 0.035;
export const MEETING_COMPANION_QUIET_ENTER_MS = 2_000;
export const MEETING_COMPANION_QUIET_EXIT_LEVEL = 0.06;
export const MEETING_COMPANION_QUIET_EXIT_MS = 150;
export const MEETING_COMPANION_COMPLETED_HOLD_MS = 3_000;

export class MeetingCompanionEventGate {
  private revision = 0;
  private meetingId: string | null = null;
  private meetingStartedAtMs: number | null = null;
  private readonly retiredMeetingIds = new Set<string>();

  beginInitialization(): number {
    return this.revision;
  }

  acceptInitialization(token: number, snapshot: MeetingRecordingSnapshot | null): boolean {
    if (token !== this.revision) return false;
    if (!snapshot) return this.meetingId === null;
    const meetingId = snapshot.meeting.id;
    if (this.retiredMeetingIds.has(meetingId)) return false;
    if (this.meetingId === null) {
      this.meetingId = meetingId;
      this.meetingStartedAtMs = snapshotStartedAtMs(snapshot);
      return true;
    }
    if (this.meetingId !== meetingId) return false;
    this.meetingStartedAtMs ??= snapshotStartedAtMs(snapshot);
    return true;
  }

  acceptSnapshotEvent(snapshot: MeetingRecordingSnapshot): boolean {
    const meetingId = snapshot.meeting.id;
    if (this.retiredMeetingIds.has(meetingId)) return false;
    if (this.meetingId === meetingId) {
      this.meetingStartedAtMs ??= snapshotStartedAtMs(snapshot);
      this.revision += 1;
      return true;
    }
    if (this.meetingId !== null) {
      const candidateStartedAtMs = snapshotStartedAtMs(snapshot);
      if (
        candidateStartedAtMs === null
        || this.meetingStartedAtMs === null
        || candidateStartedAtMs <= this.meetingStartedAtMs
      ) {
        return false;
      }
      this.retiredMeetingIds.add(this.meetingId);
      this.meetingStartedAtMs = candidateStartedAtMs;
    } else {
      this.meetingStartedAtMs = snapshotStartedAtMs(snapshot);
    }
    this.meetingId = meetingId;
    this.revision += 1;
    return true;
  }

  acceptRelatedEvent(
    meetingId: string,
    establishCurrent = false,
    meetingStartedAt: string | null = null,
  ): boolean {
    if (this.retiredMeetingIds.has(meetingId)) return false;
    const candidateStartedAtMs = parseStartedAtMs(meetingStartedAt);
    if (this.meetingId === meetingId) {
      this.meetingStartedAtMs ??= candidateStartedAtMs;
      this.revision += 1;
      return true;
    }
    if (this.meetingId === null && establishCurrent) {
      this.meetingId = meetingId;
      this.meetingStartedAtMs = candidateStartedAtMs;
      this.revision += 1;
      return true;
    }
    if (
      !establishCurrent
      || candidateStartedAtMs === null
      || this.meetingStartedAtMs === null
      || candidateStartedAtMs <= this.meetingStartedAtMs
    ) {
      return false;
    }
    if (this.meetingId === null) return false;
    this.retiredMeetingIds.add(this.meetingId);
    this.meetingId = meetingId;
    this.meetingStartedAtMs = candidateStartedAtMs;
    this.revision += 1;
    return true;
  }

  isCurrent(meetingId: string): boolean {
    return this.meetingId === meetingId && !this.retiredMeetingIds.has(meetingId);
  }

  currentMeetingId(): string | null {
    return this.meetingId;
  }
}

function snapshotStartedAtMs(snapshot: MeetingRecordingSnapshot): number | null {
  return parseStartedAtMs(snapshot.meeting.startedAt);
}

function parseStartedAtMs(startedAt: string | null | undefined): number | null {
  if (!startedAt) return null;
  const parsed = Date.parse(startedAt);
  return Number.isFinite(parsed) ? parsed : null;
}

export interface MeetingCompanionQuietResult {
  accepted: boolean;
  changed: boolean;
  quiet: boolean;
}

export class MeetingCompanionQuietDetector {
  private meetingId: string | null = null;
  private active = false;
  private quiet = false;
  private lowSinceMs: number | null = null;
  private highSinceMs: number | null = null;

  setContext(meetingId: string | null, active: boolean): MeetingCompanionQuietResult {
    const normalizedActive = Boolean(meetingId && active);
    if (this.meetingId === meetingId && this.active === normalizedActive) {
      return { accepted: true, changed: false, quiet: this.quiet };
    }
    const changed = this.quiet;
    this.meetingId = meetingId;
    this.active = normalizedActive;
    this.quiet = false;
    this.lowSinceMs = null;
    this.highSinceMs = null;
    return { accepted: true, changed, quiet: false };
  }

  sample(event: MeetingAudioLevelEvent, nowMs: number): MeetingCompanionQuietResult {
    if (
      !this.active
      || event.meetingId !== this.meetingId
      || !Number.isFinite(event.level)
      || !Number.isFinite(nowMs)
    ) {
      return { accepted: false, changed: false, quiet: this.quiet };
    }

    const level = Math.max(0, Math.min(1, event.level));
    if (this.quiet) {
      this.lowSinceMs = null;
      if (level >= MEETING_COMPANION_QUIET_EXIT_LEVEL) {
        if (this.highSinceMs === null || nowMs < this.highSinceMs) this.highSinceMs = nowMs;
        if (nowMs - this.highSinceMs >= MEETING_COMPANION_QUIET_EXIT_MS) {
          this.quiet = false;
          this.highSinceMs = null;
          return { accepted: true, changed: true, quiet: false };
        }
      } else {
        this.highSinceMs = null;
      }
      return { accepted: true, changed: false, quiet: true };
    }

    this.highSinceMs = null;
    if (level < MEETING_COMPANION_QUIET_ENTER_LEVEL) {
      if (this.lowSinceMs === null || nowMs < this.lowSinceMs) this.lowSinceMs = nowMs;
      if (nowMs - this.lowSinceMs >= MEETING_COMPANION_QUIET_ENTER_MS) {
        this.quiet = true;
        this.lowSinceMs = null;
        return { accepted: true, changed: true, quiet: true };
      }
    } else {
      this.lowSinceMs = null;
    }
    return { accepted: true, changed: false, quiet: false };
  }
}

export interface MeetingCompanionTransitionScheduler {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

const defaultTransitionScheduler: MeetingCompanionTransitionScheduler = {
  setTimeout: (callback, delayMs) => globalThis.setTimeout(callback, delayMs),
  clearTimeout: handle => globalThis.clearTimeout(handle as number),
};

export class MeetingCompanionTransitionTimer {
  private handle: unknown = null;
  private generation = 0;

  constructor(
    private readonly scheduler: MeetingCompanionTransitionScheduler = defaultTransitionScheduler,
  ) {}

  schedule(delayMs: number, callback: () => void): void {
    this.clear();
    const generation = this.generation;
    this.handle = this.scheduler.setTimeout(() => {
      if (generation !== this.generation) return;
      this.handle = null;
      callback();
    }, Math.max(0, delayMs));
  }

  clear(): void {
    this.generation += 1;
    if (this.handle !== null) this.scheduler.clearTimeout(this.handle);
    this.handle = null;
  }
}

export function isMeetingCompanionQuietEligible(snapshot: MeetingRecordingSnapshot): boolean {
  return snapshot.phase === 'starting'
    || snapshot.phase === 'recording'
    || snapshot.phase === 'transcribing_interrupted';
}
