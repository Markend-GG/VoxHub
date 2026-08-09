import { useCallback, useEffect, useReducer, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  CircleAlert,
  ExternalLink,
  EyeOff,
  Lock,
  MoreHorizontal,
  Pause,
  Play,
  Square,
  TriangleAlert,
  Unlock,
  X,
} from 'lucide-react';
import { MeetingSignalRail } from './MeetingSignalRail';
import {
  dismissCompletedMeetingCompanion,
  getActiveMeetingRecording,
  hideMeetingCompanion,
  isTauri,
  openMeetingFromCompanion,
  pauseMeetingRecording,
  resumeMeetingRecording,
  saveMeetingCompanionPosition,
  setMeetingCompanionPositionLocked,
  startMeetingCompanionDrag,
  stopMeetingRecording,
} from '../lib/ipc';
import { getSettings } from '../lib/ipc/settings';
import {
  executeMeetingCompanionCommand,
  MeetingCompanionCommandGate,
  meetingCompanionControlActions,
  meetingCompanionMenuActions,
  nextMeetingCompanionDialogFocusIndex,
  shouldStartMeetingCompanionDrag,
  type MeetingCompanionCommandAction,
  type MeetingCompanionControlAction,
  type MeetingCompanionMenuAction,
} from '../lib/meetingCompanionControls';
import {
  MEETING_SIGNAL_COMPLETE_ANIMATION_MS,
  MEETING_SIGNAL_IDLE_MS,
} from '../lib/meetingCompanionSignal';
import {
  initialMeetingCompanionState,
  meetingCompanionReducer,
  type MeetingCompanionErrorOverlay,
  type MeetingCompanionVisualState,
} from '../lib/meetingCompanionState';
import {
  isMeetingCompanionQuietEligible,
  MEETING_COMPANION_COMPLETED_HOLD_MS,
  MeetingCompanionEventGate,
  MeetingCompanionQuietDetector,
  MeetingCompanionTransitionTimer,
} from '../lib/meetingCompanionSync';
import {
  formatMeetingCompanionElapsed,
  MeetingCompanionElapsedClock,
  type MeetingCompanionTimerInput,
} from '../lib/meetingCompanionTimer';
import type {
  MeetingAudioLevelEvent,
  MeetingErrorEvent,
  MeetingRecordingSnapshot,
  MeetingSummaryEvent,
  UserPreferences,
} from '../lib/types';

const MEETING_COMPANION_WIDTH = 320;
const MEETING_COMPANION_HEIGHT = 64;

export interface MeetingCompanionProps {
  visualState?: MeetingCompanionVisualState;
  timerInput?: MeetingCompanionTimerInput | null;
  errorOverlay?: MeetingCompanionErrorOverlay | null;
  animationPaused?: boolean;
  audioLevel?: number;
}

export function MeetingCompanion(props: MeetingCompanionProps) {
  const hasExplicitPresentation = props.visualState !== undefined
    || props.timerInput !== undefined
    || props.errorOverlay !== undefined
    || props.animationPaused !== undefined
    || props.audioLevel !== undefined;
  if (isTauri && !hasExplicitPresentation) return <ConnectedMeetingCompanion />;
  return (
    <MeetingCompanionView
      visualState={props.visualState ?? 'idle'}
      timerInput={props.timerInput ?? null}
      errorOverlay={props.errorOverlay ?? null}
      animationPaused={props.animationPaused ?? false}
      audioLevel={props.audioLevel ?? 0.08}
      interaction={null}
    />
  );
}

interface MeetingCompanionInteraction {
  meetingId: string;
  positionLocked: boolean;
  positionLockBusy: boolean;
  busyAction: MeetingCompanionCommandAction | null;
  runCommand(action: MeetingCompanionCommandAction): Promise<unknown>;
  hide(): Promise<void>;
  togglePositionLock(): Promise<void>;
  openMeeting(): Promise<void>;
}

interface MeetingCompanionViewProps {
  visualState: MeetingCompanionVisualState;
  timerInput: MeetingCompanionTimerInput | null;
  errorOverlay: MeetingCompanionErrorOverlay | null;
  animationPaused: boolean;
  audioLevel: number;
  interaction: MeetingCompanionInteraction | null;
  onCompletedPlaybackFinished?: () => void;
}

function MeetingCompanionView({
  visualState,
  timerInput,
  errorOverlay,
  animationPaused,
  audioLevel,
  interaction,
  onCompletedPlaybackFinished,
}: MeetingCompanionViewProps) {
  if (visualState === 'hidden') return null;
  return (
    <VisibleMeetingCompanion
      visualState={visualState}
      timerInput={timerInput}
      errorOverlay={errorOverlay}
      animationPaused={animationPaused}
      audioLevel={audioLevel}
      interaction={interaction}
      onCompletedPlaybackFinished={onCompletedPlaybackFinished}
    />
  );
}

function ConnectedMeetingCompanion() {
  const [state, dispatch] = useReducer(meetingCompanionReducer, initialMeetingCompanionState);
  const [busyAction, setBusyAction] = useState<MeetingCompanionCommandAction | null>(null);
  const [positionLocked, setPositionLocked] = useState(false);
  const [positionLockBusy, setPositionLockBusy] = useState(false);
  const [audioLevel, setAudioLevel] = useState(0);
  const gateRef = useRef<MeetingCompanionEventGate | null>(null);
  const commandGateRef = useRef<MeetingCompanionCommandGate | null>(null);
  const quietDetectorRef = useRef<MeetingCompanionQuietDetector | null>(null);
  const transitionTimerRef = useRef<MeetingCompanionTransitionTimer | null>(null);
  if (!gateRef.current) gateRef.current = new MeetingCompanionEventGate();
  if (!commandGateRef.current) commandGateRef.current = new MeetingCompanionCommandGate();
  if (!quietDetectorRef.current) quietDetectorRef.current = new MeetingCompanionQuietDetector();
  if (!transitionTimerRef.current) transitionTimerRef.current = new MeetingCompanionTransitionTimer();

  const documentVisible = useDocumentVisibility();

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const handle = await listen<{ meetingId: string }>('meeting-companion:show', event => {
          if (!cancelled && event.payload.meetingId) {
            dispatch({ type: 'show', meetingId: event.payload.meetingId });
          }
        });
        if (cancelled) handle();
        else unlisten = handle;
      } catch (error) {
        console.warn('[meeting-companion] show listener setup failed', error);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const applyAuthoritativeSnapshot = useCallback((
    snapshot: MeetingRecordingSnapshot,
    source: 'event' | 'command',
  ): boolean => {
    const gate = gateRef.current!;
    if (!gate.acceptSnapshotEvent(snapshot)) return false;
    if (source === 'event') commandGateRef.current?.invalidate();
    const quietContext = quietDetectorRef.current!.setContext(
      snapshot.meeting.id,
      isMeetingCompanionQuietEligible(snapshot),
    );
    dispatch({ type: 'snapshot', snapshot });
    if (quietContext.changed) {
      dispatch({ type: 'quiet-changed', meetingId: snapshot.meeting.id, quiet: false });
    }
    return true;
  }, []);

  useEffect(() => {
    let cancelled = false;
    let revision = 0;
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const handle = await listen<UserPreferences>('prefs:changed', event => {
          if (cancelled) return;
          revision += 1;
          setPositionLocked(event.payload.meetingCompanionPositionLocked);
        });
        if (cancelled) {
          handle();
          return;
        }
        unlisten = handle;
        const initializationRevision = revision;
        const prefs = await getSettings();
        if (!cancelled && initializationRevision === revision) {
          setPositionLocked(prefs.meetingCompanionPositionLocked);
        }
      } catch (error) {
        console.warn('[meeting-companion] preference synchronization failed', error);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    commandGateRef.current?.cancel();
    setBusyAction(null);
    setPositionLockBusy(false);
    setAudioLevel(0);
  }, [state.meetingId]);

  useEffect(() => {
    const detector = quietDetectorRef.current!;
    const quietActive = Boolean(
      documentVisible
      && state.snapshot
      && isMeetingCompanionQuietEligible(state.snapshot),
    );
    const result = detector.setContext(state.meetingId, quietActive);
    if (result.changed && state.meetingId) {
      dispatch({ type: 'quiet-changed', meetingId: state.meetingId, quiet: false });
    }
  }, [documentVisible, state.meetingId, state.snapshot?.meeting.status, state.snapshot?.phase]);

  useEffect(() => {
    if (!documentVisible) return;
    const gate = gateRef.current!;
    const detector = quietDetectorRef.current!;
    let cancelled = false;
    let unlisteners: Array<() => void> = [];

    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const register = async <T,>(
          eventName: string,
          handler: (event: { payload: T }) => void,
        ): Promise<boolean> => {
          const unlisten = await listen<T>(eventName, handler);
          if (cancelled) {
            unlisten();
            return false;
          }
          unlisteners.push(unlisten);
          return true;
        };
        if (!await register<MeetingRecordingSnapshot>('meeting:state', event => {
            if (!cancelled) applyAuthoritativeSnapshot(event.payload, 'event');
          })) return;
        if (!await register<MeetingSummaryEvent>('meeting:summary', event => {
            if (cancelled) return;
            const payload = event.payload;
            if (!gate.acceptRelatedEvent(
              payload.meetingId,
              true,
              payload.meeting?.startedAt ?? null,
            )) return;
            commandGateRef.current?.invalidate();
            const quietContext = detector.setContext(payload.meetingId, false);
            if (quietContext.changed) {
              dispatch({ type: 'quiet-changed', meetingId: payload.meetingId, quiet: false });
            }
            if (payload.status === 'summarizing') {
              dispatch({ type: 'stop-accepted', meetingId: payload.meetingId });
            } else if (payload.status === 'completed') {
              dispatch({ type: 'summary-succeeded', meetingId: payload.meetingId });
            } else if (payload.status === 'summary_failed') {
              dispatch({
                type: 'summary-failed',
                meetingId: payload.meetingId,
                message: payload.error?.message ?? null,
              });
            }
          })) return;
        if (!await register<MeetingErrorEvent>('meeting:error', event => {
            if (cancelled) return;
            const payload = event.payload;
            if (!payload.meetingId || !gate.acceptRelatedEvent(payload.meetingId)) return;
            commandGateRef.current?.invalidate();
            if (
              payload.code.startsWith('summary')
              || payload.code === 'emptyTranscript'
            ) {
              detector.setContext(payload.meetingId, false);
              dispatch({
                type: 'summary-failed',
                meetingId: payload.meetingId,
                message: payload.message,
              });
            } else if (payload.code === 'asrInterrupted' || payload.code === 'recorderInterrupted') {
              dispatch({
                type: 'transcribing-interrupted',
                meetingId: payload.meetingId,
                message: payload.message,
              });
            }
          })) return;
        if (!await register<MeetingAudioLevelEvent>('meeting:audio-level', event => {
            if (cancelled) return;
            const payload = event.payload;
            if (!gate.acceptRelatedEvent(payload.meetingId)) return;
            setAudioLevel(Math.max(0, Math.min(1, payload.level)));
            const result = detector.sample(payload, performance.now());
            if (result.changed) {
              dispatch({
                type: 'quiet-changed',
                meetingId: payload.meetingId,
                quiet: result.quiet,
              });
            }
          })) return;

        const initialization = gate.beginInitialization();
        const snapshot = await getActiveMeetingRecording();
        if (cancelled || !gate.acceptInitialization(initialization, snapshot)) return;
        if (!snapshot) {
          dispatch({ type: 'snapshot', snapshot: null });
          return;
        }
        const quietContext = detector.setContext(
          snapshot.meeting.id,
          isMeetingCompanionQuietEligible(snapshot),
        );
        dispatch({ type: 'snapshot', snapshot });
        if (quietContext.changed) {
          dispatch({ type: 'quiet-changed', meetingId: snapshot.meeting.id, quiet: false });
        }
      } catch (error) {
        unlisteners.splice(0).forEach(unlisten => unlisten());
        if (!cancelled) console.warn('[meeting-companion] state synchronization failed', error);
      }
    })();

    return () => {
      cancelled = true;
      unlisteners.splice(0).forEach(unlisten => unlisten());
    };
  }, [applyAuthoritativeSnapshot, documentVisible]);

  useEffect(() => {
    const timer = transitionTimerRef.current!;
    timer.clear();
    const meetingId = state.meetingId;
    if (!documentVisible || !meetingId) return;

    if (state.visualState === 'idle') {
      timer.schedule(MEETING_SIGNAL_IDLE_MS, () => {
        if (gateRef.current?.isCurrent(meetingId)) {
          dispatch({ type: 'idle-finished', meetingId });
        }
      });
    }
    return () => timer.clear();
  }, [documentVisible, state.meetingId, state.visualState]);

  const handleCompletedPlaybackFinished = useCallback(() => {
    const meetingId = state.meetingId;
    if (
      !documentVisible
      || state.visualState !== 'completed'
      || !meetingId
      || !gateRef.current?.isCurrent(meetingId)
    ) return;
    transitionTimerRef.current?.schedule(MEETING_COMPANION_COMPLETED_HOLD_MS, () => {
      if (!gateRef.current?.isCurrent(meetingId)) return;
      dispatch({ type: 'hide', meetingId });
      void dismissCompletedMeetingCompanion(meetingId).catch(error => {
        console.warn('[meeting-companion] completed dismissal failed', error);
      });
    });
  }, [documentVisible, state.meetingId, state.visualState]);

  const runCommand = useCallback((action: MeetingCompanionCommandAction) => {
    const meetingId = gateRef.current?.currentMeetingId();
    if (!meetingId || meetingId !== state.meetingId) return Promise.resolve('stale');
    dispatch({ type: 'clear-error', meetingId });
    return executeMeetingCompanionCommand(
      commandGateRef.current!,
      action,
      meetingId,
      {
        pause: pauseMeetingRecording,
        resume: resumeMeetingRecording,
        stop: stopMeetingRecording,
        getActive: getActiveMeetingRecording,
      },
      {
        currentMeetingId: () => gateRef.current?.currentMeetingId() ?? null,
        setBusy: setBusyAction,
        applySnapshot: snapshot => {
          applyAuthoritativeSnapshot(snapshot, 'command');
        },
        acceptStop: acceptedMeetingId => {
          if (gateRef.current?.isCurrent(acceptedMeetingId)) {
            dispatch({ type: 'stop-accepted', meetingId: acceptedMeetingId });
          }
        },
        recoverFailure: (snapshot, error) => {
          if (snapshot) applyAuthoritativeSnapshot(snapshot, 'command');
          if (gateRef.current?.isCurrent(meetingId)) {
            console.warn(`[meeting-companion] ${action} failed`, error);
            dispatch({
              type: 'command-failed',
              meetingId,
              message: errorMessage(error),
            });
          }
        },
      },
    );
  }, [applyAuthoritativeSnapshot, state.meetingId]);

  const hide = useCallback(async () => {
    const meetingId = gateRef.current?.currentMeetingId();
    if (!meetingId) return;
    try {
      await hideMeetingCompanion();
      if (gateRef.current?.isCurrent(meetingId)) dispatch({ type: 'hide', meetingId });
    } catch (error) {
      console.warn('[meeting-companion] hide failed', error);
      if (gateRef.current?.isCurrent(meetingId)) {
        dispatch({ type: 'command-failed', meetingId, message: errorMessage(error) });
      }
    }
  }, []);

  const togglePositionLock = useCallback(async () => {
    const meetingId = gateRef.current?.currentMeetingId();
    if (!meetingId || positionLockBusy) return;
    setPositionLockBusy(true);
    try {
      await setMeetingCompanionPositionLocked(!positionLocked);
    } catch (error) {
      console.warn('[meeting-companion] position lock update failed', error);
      if (gateRef.current?.isCurrent(meetingId)) {
        dispatch({ type: 'command-failed', meetingId, message: errorMessage(error) });
      }
    } finally {
      if (gateRef.current?.isCurrent(meetingId)) setPositionLockBusy(false);
    }
  }, [positionLockBusy, positionLocked]);

  const openMeeting = useCallback(async () => {
    const meetingId = gateRef.current?.currentMeetingId();
    if (!meetingId) return;
    try {
      await openMeetingFromCompanion(meetingId);
    } catch (error) {
      console.warn('[meeting-companion] open meeting failed', error);
      if (gateRef.current?.isCurrent(meetingId)) {
        dispatch({ type: 'command-failed', meetingId, message: errorMessage(error) });
      }
    }
  }, []);

  useEffect(() => () => {
    transitionTimerRef.current?.clear();
    quietDetectorRef.current?.setContext(null, false);
    commandGateRef.current?.cancel();
  }, []);

  if (!documentVisible) return null;
  const snapshot = state.snapshot;
  const timerInput = snapshot && state.meetingId === snapshot.meeting.id
    ? {
        meetingId: snapshot.meeting.id,
        elapsedMs: snapshot.elapsedMs,
        running: isMeetingCompanionQuietEligible(snapshot),
      }
    : null;
  return (
    <MeetingCompanionView
      visualState={state.visualState}
      timerInput={timerInput}
      errorOverlay={state.error}
      animationPaused={state.error?.kind === 'summary_failed'}
      audioLevel={audioLevel}
      interaction={state.meetingId ? {
        meetingId: state.meetingId,
        positionLocked,
        positionLockBusy,
        busyAction,
        runCommand,
        hide,
        togglePositionLock,
        openMeeting,
      } : null}
      onCompletedPlaybackFinished={handleCompletedPlaybackFinished}
    />
  );
}

interface VisibleMeetingCompanionProps {
  visualState: MeetingCompanionVisualState;
  timerInput: MeetingCompanionTimerInput | null;
  errorOverlay: MeetingCompanionErrorOverlay | null;
  animationPaused: boolean;
  audioLevel: number;
  interaction: MeetingCompanionInteraction | null;
  onCompletedPlaybackFinished?: () => void;
}

function VisibleMeetingCompanion({
  visualState,
  timerInput,
  errorOverlay,
  animationPaused,
  audioLevel,
  interaction,
  onCompletedPlaybackFinished,
}: VisibleMeetingCompanionProps) {
  const { t } = useTranslation();
  const reducedMotion = useReducedMotion();
  const elapsedText = useMeetingCompanionElapsed(timerInput);
  const [contextMenuOpen, setContextMenuOpen] = useState(false);
  const [stopConfirmOpen, setStopConfirmOpen] = useState(false);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const cancelStopRef = useRef<HTMLButtonElement | null>(null);
  const dragGestureRef = useRef<{
    pointerId: number;
    startX: number;
    startY: number;
    started: boolean;
  } | null>(null);
  const errorKind = errorOverlay?.kind ?? null;
  const controlActions = interaction
    ? meetingCompanionControlActions(visualState, errorKind)
    : [];
  const menuActions = interaction
    ? meetingCompanionMenuActions(visualState, errorKind, interaction.positionLocked)
    : [];

  useEffect(() => {
    if (!stopConfirmOpen) return;
    const stopAllowed = controlActions.some(action => action.action === 'stop');
    if (!stopAllowed) setStopConfirmOpen(false);
  }, [controlActions, stopConfirmOpen]);

  useEffect(() => {
    if (!contextMenuOpen) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setContextMenuOpen(false);
    };
    document.addEventListener('keydown', closeOnEscape, true);
    return () => document.removeEventListener('keydown', closeOnEscape, true);
  }, [contextMenuOpen]);

  useEffect(() => {
    if (!stopConfirmOpen) return;
    const previousFocus = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    const frame = requestAnimationFrame(() => cancelStopRef.current?.focus());
    return () => {
      cancelAnimationFrame(frame);
      previousFocus?.focus();
    };
  }, [stopConfirmOpen]);

  useEffect(() => {
    setContextMenuOpen(false);
  }, [visualState]);

  useEffect(() => {
    if (visualState !== 'completed') return;
    const timeout = globalThis.setTimeout(
      () => onCompletedPlaybackFinished?.(),
      reducedMotion ? 0 : MEETING_SIGNAL_COMPLETE_ANIMATION_MS,
    );
    return () => globalThis.clearTimeout(timeout);
  }, [onCompletedPlaybackFinished, reducedMotion, visualState]);

  const startDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (
      event.button !== 0
      || interaction?.positionLocked
      || stopConfirmOpen
      || contextMenuOpen
    ) return;
    dragGestureRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      started: false,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const continueDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    const gesture = dragGestureRef.current;
    if (
      !gesture
      || gesture.pointerId !== event.pointerId
      || gesture.started
      || !shouldStartMeetingCompanionDrag(
        gesture.startX,
        gesture.startY,
        event.clientX,
        event.clientY,
        (event.buttons & 1) === 1,
      )
    ) return;
    gesture.started = true;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    void startMeetingCompanionDrag().catch(error => {
      console.warn('[meeting-companion] start drag failed', error);
    });
  };

  const finishDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    const gesture = dragGestureRef.current;
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    dragGestureRef.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    if (!gesture.started) return;
    void saveMeetingCompanionPosition().catch(error => {
      console.warn('[meeting-companion] save position failed', error);
    });
  };

  const cancelDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (dragGestureRef.current?.pointerId === event.pointerId) {
      dragGestureRef.current = null;
    }
  };

  const errorLabel = errorKind === 'summary_failed'
    ? t('meetingCompanion.summaryFailed')
    : errorKind === 'transcribing_interrupted'
      ? t('meetingCompanion.asrInterrupted')
      : errorKind === 'command_failed'
        ? t('meetingCompanion.commandFailed')
        : null;
  const statusLabel = errorKind === 'summary_failed' || errorKind === 'command_failed'
    ? errorLabel
    : t(meetingCompanionStatusKey(visualState));

  const runControlAction = (action: MeetingCompanionControlAction) => {
    if (!interaction) return;
    if (action === 'stop') {
      setContextMenuOpen(false);
      setStopConfirmOpen(true);
      return;
    }
    if (action === 'open-meeting') {
      void interaction.openMeeting();
      return;
    }
    void interaction.runCommand(action);
  };

  const runMenuAction = (action: MeetingCompanionMenuAction) => {
    if (!interaction) return;
    setContextMenuOpen(false);
    if (action === 'hide') {
      void interaction.hide();
      return;
    }
    if (action === 'toggle-position-lock') {
      void interaction.togglePositionLock();
      return;
    }
    runControlAction(action);
  };

  const confirmStop = () => {
    if (!interaction || interaction.busyAction) return;
    void interaction.runCommand('stop');
  };

  const openContextMenu = (event: React.MouseEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.stopPropagation();
    if (!interaction || menuActions.length === 0 || stopConfirmOpen) return;
    setContextMenuOpen(true);
  };

  const handleStopDialogKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Escape' && interaction?.busyAction !== 'stop') {
      event.preventDefault();
      setStopConfirmOpen(false);
      return;
    }
    if (event.key !== 'Tab') return;
    const buttons = dialogRef.current
      ? [...dialogRef.current.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')]
      : [];
    if (buttons.length === 0) {
      event.preventDefault();
      return;
    }
    const currentIndex = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const nextIndex = nextMeetingCompanionDialogFocusIndex(
      currentIndex,
      buttons.length,
      event.shiftKey,
    );
    event.preventDefault();
    buttons[nextIndex]?.focus();
  };

  return (
    <div
      data-meeting-companion-root
      data-meeting-companion-state={visualState}
      data-meeting-companion-skin="signal-rail"
      onContextMenu={openContextMenu}
      style={{
        width: MEETING_COMPANION_WIDTH,
        height: MEETING_COMPANION_HEIGHT,
        flex: `0 0 ${MEETING_COMPANION_WIDTH}px`,
        position: 'relative',
        overflow: 'hidden',
        display: 'flex',
        alignItems: 'center',
        padding: 4,
        boxSizing: 'border-box',
        userSelect: 'none',
      }}
    >
      <div
        data-meeting-companion-capsule
        style={{
          width: '100%',
          height: '100%',
          position: 'relative',
          overflow: 'hidden',
          display: 'flex',
          alignItems: 'center',
          border: '1px solid rgba(255, 255, 255, 0.13)',
          borderRadius: 18,
          background: 'rgba(16, 18, 18, 0.97)',
          boxShadow: '0 7px 20px rgba(0, 0, 0, 0.32), inset 0 1px rgba(255, 255, 255, 0.04)',
          color: '#f5f7f6',
        }}
      >
        {!contextMenuOpen && !stopConfirmOpen && (
          <div
            data-meeting-companion-main
            style={{
              width: '100%',
              height: '100%',
              display: 'flex',
              alignItems: 'center',
              gap: 6,
              padding: '0 8px 0 10px',
              boxSizing: 'border-box',
            }}
          >
            <div
              data-meeting-companion-drag-region
              onPointerDown={startDrag}
              onPointerMove={continueDrag}
              onPointerUp={finishDrag}
              onPointerCancel={cancelDrag}
              style={{
                minWidth: 0,
                flex: '1 1 auto',
                height: 42,
                display: 'flex',
                alignItems: 'center',
                gap: 7,
                cursor: interaction?.positionLocked ? 'default' : 'grab',
                touchAction: 'none',
              }}
            >
              <div style={{ minWidth: 0, flex: '1 1 auto' }}>
                <div
                  data-meeting-companion-status
                  style={{
                    height: 14,
                    overflow: 'hidden',
                    color: errorKind ? '#ffaaa5' : '#a9b1ad',
                    fontSize: 9,
                    fontWeight: 650,
                    lineHeight: '14px',
                    letterSpacing: 0,
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                  }}
                >
                  {statusLabel}
                </div>
                <MeetingSignalRail
                  state={visualState}
                  level={audioLevel}
                  errorKind={errorKind}
                  reducedMotion={reducedMotion}
                  frozen={animationPaused}
                  style={{ width: '100%', height: 24 }}
                />
              </div>
              {errorOverlay && (
                <span
                  data-meeting-companion-error={errorOverlay.kind}
                  title={errorOverlay.message || errorLabel || undefined}
                  aria-label={errorLabel || undefined}
                  role="status"
                  style={{ color: errorKind === 'summary_failed' ? '#ff6b63' : '#f0ad4e', flex: '0 0 auto' }}
                >
                  {errorKind === 'summary_failed' || errorKind === 'command_failed'
                    ? <CircleAlert size={14} strokeWidth={2.2} />
                    : <TriangleAlert size={14} strokeWidth={2.2} />}
                </span>
              )}
            </div>

            <div
              data-meeting-companion-timer
              style={{
                width: 54,
                flex: '0 0 54px',
                color: '#f3f6f4',
                fontFamily: 'ui-monospace, SFMono-Regular, Consolas, monospace',
                fontSize: elapsedText.length > 5 ? 10 : 11,
                fontWeight: 650,
                fontVariantNumeric: 'tabular-nums',
                letterSpacing: 0,
                textAlign: 'center',
                whiteSpace: 'nowrap',
              }}
            >
              {elapsedText}
            </div>
            <div style={{ display: 'flex', alignItems: 'center', gap: 5, flex: '0 0 auto' }}>
              {controlActions.map(action => {
                const disabled = Boolean(interaction?.busyAction);
                const label = t(action.labelKey);
                return (
                  <button
                    key={action.action}
                    type="button"
                    className="ol-focus-ring"
                    data-meeting-companion-control={action.action}
                    title={label}
                    aria-label={label}
                    disabled={disabled}
                    onPointerDown={event => event.stopPropagation()}
                    onClick={() => runControlAction(action.action)}
                    style={compactActionButtonStyle(Boolean(action.danger), disabled)}
                  >
                    {meetingCompanionActionIcon(action.action, 15)}
                  </button>
                );
              })}
              {interaction && (
                <button
                  type="button"
                  className="ol-focus-ring"
                  data-meeting-companion-more
                  title={t('meetingCompanion.more')}
                  aria-label={t('meetingCompanion.more')}
                  onClick={() => setContextMenuOpen(true)}
                  style={compactActionButtonStyle(false, false)}
                >
                  <MoreHorizontal size={16} strokeWidth={2.1} />
                </button>
              )}
            </div>
          </div>
        )}

        {contextMenuOpen && interaction && (
          <div
            data-meeting-companion-context-menu
            role="menu"
            onContextMenu={event => event.preventDefault()}
            style={{
              width: '100%',
              height: '100%',
              padding: '0 8px 0 12px',
              boxSizing: 'border-box',
              display: 'flex',
              alignItems: 'center',
              gap: 5,
            }}
          >
            <span style={{ minWidth: 0, flex: '1 1 auto', color: '#a9b1ad', fontSize: 10, fontWeight: 650 }}>
              {t('meetingCompanion.options')}
            </span>
            {menuActions.map(action => {
              const disabled = (isMeetingCompanionCommandAction(action.action)
                && Boolean(interaction.busyAction))
                || (action.action === 'toggle-position-lock' && interaction.positionLockBusy);
              const label = t(action.labelKey);
              return (
                <button
                  key={action.action}
                  type="button"
                  className="ol-focus-ring"
                  role="menuitem"
                  data-meeting-companion-menu-action={action.action}
                  title={label}
                  aria-label={label}
                  disabled={disabled}
                  onClick={() => runMenuAction(action.action)}
                  style={compactActionButtonStyle(Boolean(action.danger), disabled)}
                >
                  {meetingCompanionActionIcon(action.action, 15, interaction.positionLocked)}
                </button>
              );
            })}
            <button
              type="button"
              className="ol-focus-ring"
              title={t('meetingCompanion.closeOptions')}
              aria-label={t('meetingCompanion.closeOptions')}
              onClick={() => setContextMenuOpen(false)}
              style={compactActionButtonStyle(false, false)}
            >
              <X size={15} strokeWidth={2.1} />
            </button>
          </div>
        )}

        {stopConfirmOpen && interaction && (
          <div
            data-meeting-companion-stop-dialog-backdrop
            style={{ width: '100%', height: '100%' }}
          >
            <div
              ref={dialogRef}
              data-meeting-companion-stop-dialog
              role="dialog"
              aria-modal="true"
              aria-labelledby="meeting-companion-stop-title"
              aria-describedby="meeting-companion-stop-body"
              onKeyDown={handleStopDialogKeyDown}
              style={{
                width: '100%',
                height: '100%',
                padding: '0 9px 0 13px',
                boxSizing: 'border-box',
                display: 'flex',
                alignItems: 'center',
                gap: 7,
              }}
            >
              <div
                id="meeting-companion-stop-title"
                style={{ minWidth: 0, flex: '1 1 auto', fontSize: 11, fontWeight: 700, lineHeight: 1.35 }}
              >
                {t('meetingCompanion.stopConfirmTitle')}
              </div>
              <span
                id="meeting-companion-stop-body"
                style={{ position: 'absolute', width: 1, height: 1, overflow: 'hidden', clipPath: 'inset(50%)' }}
              >
                {t('meetingCompanion.stopConfirmBody')}
              </span>
              <button
                ref={cancelStopRef}
                type="button"
                className="ol-focus-ring"
                title={t('meetingCompanion.cancel')}
                aria-label={t('meetingCompanion.cancel')}
                disabled={interaction.busyAction === 'stop'}
                onClick={() => setStopConfirmOpen(false)}
                style={compactActionButtonStyle(false, interaction.busyAction === 'stop')}
              >
                <X size={15} strokeWidth={2.2} />
              </button>
              <button
                type="button"
                className="ol-focus-ring"
                data-meeting-companion-confirm-stop
                title={t('meetingCompanion.confirmStop')}
                aria-label={t('meetingCompanion.confirmStop')}
                disabled={interaction.busyAction !== null}
                onClick={confirmStop}
                style={compactActionButtonStyle(true, interaction.busyAction !== null)}
              >
                <Square size={14} fill="currentColor" strokeWidth={1.8} />
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function meetingCompanionActionIcon(
  action: MeetingCompanionControlAction | MeetingCompanionMenuAction,
  size: number,
  positionLocked = false,
) {
  if (action === 'pause') return <Pause size={size} strokeWidth={2.2} />;
  if (action === 'resume') return <Play size={size} strokeWidth={2.2} />;
  if (action === 'stop') return <Square size={size} strokeWidth={2.2} />;
  if (action === 'open-meeting') return <ExternalLink size={size} strokeWidth={2.1} />;
  if (action === 'hide') return <EyeOff size={size} strokeWidth={2.1} />;
  return positionLocked
    ? <Unlock size={size} strokeWidth={2.1} />
    : <Lock size={size} strokeWidth={2.1} />;
}

function isMeetingCompanionCommandAction(
  action: MeetingCompanionMenuAction,
): action is MeetingCompanionCommandAction {
  return action === 'pause' || action === 'resume' || action === 'stop';
}

function compactActionButtonStyle(danger: boolean, disabled: boolean): React.CSSProperties {
  return {
    width: 30,
    height: 30,
    flex: '0 0 30px',
    padding: 0,
    display: 'inline-grid',
    placeItems: 'center',
    border: danger ? '1px solid rgba(255, 107, 99, 0.52)' : '1px solid rgba(255, 255, 255, 0.12)',
    borderRadius: 8,
    background: danger ? 'rgba(110, 29, 25, 0.76)' : 'rgba(255, 255, 255, 0.055)',
    color: danger ? '#ff8a83' : '#d9dfdc',
    cursor: disabled ? 'not-allowed' : 'pointer',
    opacity: disabled ? 0.46 : 1,
  };
}

function meetingCompanionStatusKey(visualState: MeetingCompanionVisualState): string {
  if (visualState === 'recording') return 'meetingCompanion.recording';
  if (visualState === 'quiet') return 'meetingCompanion.quiet';
  if (visualState === 'paused') return 'meetingCompanion.paused';
  if (visualState === 'processing') return 'meetingCompanion.processing';
  if (visualState === 'completed') return 'meetingCompanion.completed';
  return 'meetingCompanion.starting';
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

function useReducedMotion(): boolean {
  const [reducedMotion, setReducedMotion] = useState(() => (
    typeof window !== 'undefined'
    && typeof window.matchMedia === 'function'
    && window.matchMedia('(prefers-reduced-motion: reduce)').matches
  ));

  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return;
    const query = window.matchMedia('(prefers-reduced-motion: reduce)');
    const update = () => setReducedMotion(query.matches);
    update();
    query.addEventListener('change', update);
    return () => query.removeEventListener('change', update);
  }, []);

  return reducedMotion;
}

function useDocumentVisibility(): boolean {
  const [visible, setVisible] = useState(() => (
    typeof document === 'undefined' || document.visibilityState === 'visible'
  ));

  useEffect(() => {
    const update = () => setVisible(document.visibilityState === 'visible');
    document.addEventListener('visibilitychange', update);
    update();
    return () => document.removeEventListener('visibilitychange', update);
  }, []);

  return visible;
}

function useMeetingCompanionElapsed(input: MeetingCompanionTimerInput | null): string {
  const clockRef = useRef<MeetingCompanionElapsedClock | null>(null);
  if (!clockRef.current) clockRef.current = new MeetingCompanionElapsedClock();
  const [display, setDisplay] = useState(() => ({
    meetingId: input?.meetingId ?? null,
    elapsedMs: input?.elapsedMs ?? 0,
  }));

  useEffect(() => {
    const clock = clockRef.current!;
    const update = () => {
      setDisplay({
        meetingId: input?.meetingId ?? null,
        elapsedMs: clock.read(performance.now()),
      });
    };
    const aligned = clock.align(input, performance.now());
    setDisplay({ meetingId: input?.meetingId ?? null, elapsedMs: aligned });
    if (!input?.running) return;
    const interval = globalThis.setInterval(update, 1000);
    return () => globalThis.clearInterval(interval);
  }, [input]);

  useEffect(() => () => clockRef.current?.clear(), []);

  const elapsedMs = display.meetingId === (input?.meetingId ?? null)
    ? display.elapsedMs
    : input?.elapsedMs ?? 0;
  return formatMeetingCompanionElapsed(elapsedMs);
}
