import type { MeetingRecord, MeetingRecordingSnapshot } from './types';
import type {
  MeetingCompanionErrorKind,
  MeetingCompanionVisualState,
} from './meetingCompanionState';

export const MEETING_COMPANION_CONTROLS_HIDE_DELAY_MS = 800;
export const MEETING_COMPANION_DRAG_THRESHOLD_PX = 3;

export type MeetingCompanionCommandAction = 'pause' | 'resume' | 'stop';
export type MeetingCompanionControlAction =
  | MeetingCompanionCommandAction
  | 'open-meeting';
export type MeetingCompanionMenuAction =
  | MeetingCompanionCommandAction
  | 'open-meeting'
  | 'hide'
  | 'toggle-position-lock';

export interface MeetingCompanionActionDefinition<TAction extends string> {
  action: TAction;
  labelKey: string;
  danger?: boolean;
}

const PAUSE = {
  action: 'pause',
  labelKey: 'meetingCompanion.pause',
} as const;
const RESUME = {
  action: 'resume',
  labelKey: 'meetingCompanion.resume',
} as const;
const STOP = {
  action: 'stop',
  labelKey: 'meetingCompanion.stop',
  danger: true,
} as const;
const OPEN_MEETING = {
  action: 'open-meeting',
  labelKey: 'meetingCompanion.openMeeting',
} as const;
const HIDE = {
  action: 'hide',
  labelKey: 'meetingCompanion.hide',
} as const;

export function meetingCompanionControlActions(
  visualState: MeetingCompanionVisualState,
  errorKind: MeetingCompanionErrorKind | null,
): ReadonlyArray<MeetingCompanionActionDefinition<MeetingCompanionControlAction>> {
  if (errorKind === 'summary_failed') return [OPEN_MEETING];
  if (visualState === 'recording' || visualState === 'quiet') return [PAUSE, STOP];
  if (visualState === 'paused') return [RESUME, STOP];
  return [];
}

export function meetingCompanionMenuActions(
  visualState: MeetingCompanionVisualState,
  errorKind: MeetingCompanionErrorKind | null,
  positionLocked: boolean,
): ReadonlyArray<MeetingCompanionActionDefinition<MeetingCompanionMenuAction>> {
  const lockAction = {
    action: 'toggle-position-lock',
    labelKey: positionLocked
      ? 'meetingCompanion.unlockPosition'
      : 'meetingCompanion.lockPosition',
  } as const;
  const common = [HIDE, lockAction] as const;
  if (errorKind === 'summary_failed') return [OPEN_MEETING, ...common];
  if (visualState === 'recording' || visualState === 'quiet') {
    return [PAUSE, STOP, ...common];
  }
  if (visualState === 'paused') return [RESUME, STOP, ...common];
  if (visualState === 'processing' || visualState === 'completed') {
    return [OPEN_MEETING, ...common];
  }
  return common;
}

export interface MeetingCompanionScheduler {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

const defaultScheduler: MeetingCompanionScheduler = {
  setTimeout: (callback, delayMs) => globalThis.setTimeout(callback, delayMs),
  clearTimeout: handle => globalThis.clearTimeout(handle as number),
};

export class MeetingCompanionControlsVisibility {
  private visible = false;
  private hideHandle: unknown = null;

  constructor(
    private readonly onChange: (visible: boolean) => void,
    private readonly scheduler: MeetingCompanionScheduler = defaultScheduler,
  ) {}

  show(): void {
    this.clearHide();
    if (this.visible) return;
    this.visible = true;
    this.onChange(true);
  }

  scheduleHide(): void {
    this.clearHide();
    this.hideHandle = this.scheduler.setTimeout(() => {
      this.hideHandle = null;
      if (!this.visible) return;
      this.visible = false;
      this.onChange(false);
    }, MEETING_COMPANION_CONTROLS_HIDE_DELAY_MS);
  }

  dispose(): void {
    this.clearHide();
  }

  private clearHide(): void {
    if (this.hideHandle !== null) this.scheduler.clearTimeout(this.hideHandle);
    this.hideHandle = null;
  }
}

interface MeetingCompanionCommandTicket {
  readonly id: number;
  readonly generation: number;
  readonly meetingId: string;
  readonly action: MeetingCompanionCommandAction;
}

export class MeetingCompanionCommandGate {
  private nextId = 0;
  private generation = 0;
  private pending: MeetingCompanionCommandTicket | null = null;

  begin(
    meetingId: string,
    action: MeetingCompanionCommandAction,
  ): MeetingCompanionCommandTicket | null {
    if (this.pending) return null;
    const ticket = {
      id: ++this.nextId,
      generation: this.generation,
      meetingId,
      action,
    };
    this.pending = ticket;
    return ticket;
  }

  invalidate(): void {
    this.generation += 1;
  }

  cancel(): void {
    this.generation += 1;
    this.pending = null;
  }

  canApply(
    ticket: MeetingCompanionCommandTicket,
    currentMeetingId: string | null,
    resultMeetingId: string,
  ): boolean {
    return this.pending?.id === ticket.id
      && ticket.generation === this.generation
      && ticket.meetingId === currentMeetingId
      && ticket.meetingId === resultMeetingId;
  }

  finish(ticket: MeetingCompanionCommandTicket): boolean {
    if (this.pending?.id !== ticket.id) return false;
    this.pending = null;
    return true;
  }
}

export interface MeetingCompanionCommandDependencies {
  pause(meetingId: string): Promise<MeetingRecordingSnapshot>;
  resume(meetingId: string): Promise<MeetingRecordingSnapshot>;
  stop(meetingId: string): Promise<MeetingRecord>;
  getActive(): Promise<MeetingRecordingSnapshot | null>;
}

export interface MeetingCompanionCommandCallbacks {
  currentMeetingId(): string | null;
  setBusy(action: MeetingCompanionCommandAction | null): void;
  applySnapshot(snapshot: MeetingRecordingSnapshot): void;
  acceptStop(meetingId: string): void;
  recoverFailure(snapshot: MeetingRecordingSnapshot | null, error: unknown): void;
}

export type MeetingCompanionCommandOutcome =
  | 'success'
  | 'failure'
  | 'stale'
  | 'duplicate';

export async function executeMeetingCompanionCommand(
  gate: MeetingCompanionCommandGate,
  action: MeetingCompanionCommandAction,
  meetingId: string,
  dependencies: MeetingCompanionCommandDependencies,
  callbacks: MeetingCompanionCommandCallbacks,
): Promise<MeetingCompanionCommandOutcome> {
  const ticket = gate.begin(meetingId, action);
  if (!ticket) return 'duplicate';
  callbacks.setBusy(action);
  try {
    if (action === 'stop') {
      const record = await dependencies.stop(meetingId);
      if (!gate.canApply(ticket, callbacks.currentMeetingId(), record.id)) return 'stale';
      callbacks.acceptStop(meetingId);
      return 'success';
    }

    const snapshot = action === 'pause'
      ? await dependencies.pause(meetingId)
      : await dependencies.resume(meetingId);
    if (!gate.canApply(ticket, callbacks.currentMeetingId(), snapshot.meeting.id)) {
      return 'stale';
    }
    callbacks.applySnapshot(snapshot);
    return 'success';
  } catch (error) {
    if (!gate.canApply(ticket, callbacks.currentMeetingId(), meetingId)) return 'stale';
    let snapshot: MeetingRecordingSnapshot | null = null;
    try {
      snapshot = await dependencies.getActive();
    } catch {
      // The translated command error remains actionable even if recovery also fails.
    }
    if (!gate.canApply(ticket, callbacks.currentMeetingId(), meetingId)) return 'stale';
    if (snapshot && snapshot.meeting.id !== meetingId) return 'stale';
    callbacks.recoverFailure(snapshot, error);
    return 'failure';
  } finally {
    if (gate.finish(ticket)) callbacks.setBusy(null);
  }
}

export interface MeetingCompanionMenuPosition {
  x: number;
  y: number;
}

export function clampMeetingCompanionMenuPosition(
  x: number,
  y: number,
  menuWidth: number,
  menuHeight: number,
  viewportWidth: number,
  viewportHeight: number,
): MeetingCompanionMenuPosition {
  return {
    x: Math.max(0, Math.min(x, Math.max(0, viewportWidth - menuWidth))),
    y: Math.max(0, Math.min(y, Math.max(0, viewportHeight - menuHeight))),
  };
}

export function shouldStartMeetingCompanionDrag(
  startX: number,
  startY: number,
  currentX: number,
  currentY: number,
  primaryButtonPressed: boolean,
  threshold = MEETING_COMPANION_DRAG_THRESHOLD_PX,
): boolean {
  if (!primaryButtonPressed) return false;
  const deltaX = currentX - startX;
  const deltaY = currentY - startY;
  return deltaX * deltaX + deltaY * deltaY >= threshold * threshold;
}

export function nextMeetingCompanionDialogFocusIndex(
  currentIndex: number,
  focusableCount: number,
  backwards: boolean,
): number {
  if (focusableCount <= 0) return -1;
  if (backwards) return currentIndex <= 0 ? focusableCount - 1 : currentIndex - 1;
  return currentIndex < 0 || currentIndex >= focusableCount - 1 ? 0 : currentIndex + 1;
}

export const MEETING_COMPANION_I18N_KEYS = [
  'pause',
  'resume',
  'stop',
  'stopConfirmTitle',
  'stopConfirmBody',
  'confirmStop',
  'cancel',
  'hide',
  'lockPosition',
  'unlockPosition',
  'openMeeting',
  'commandFailed',
  'summaryFailed',
  'asrInterrupted',
] as const;
