import { useCallback, useEffect, useReducer, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  CircleAlert,
  ExternalLink,
  EyeOff,
  Lock,
  Pause,
  Play,
  Square,
  TriangleAlert,
  Unlock,
} from 'lucide-react';
import completedPoster from '../assets/meeting-companion/completed-poster.png';
import completedWebm from '../assets/meeting-companion/completed.webm';
import idlePoster from '../assets/meeting-companion/idle-poster.png';
import idleWebm from '../assets/meeting-companion/idle.webm';
import pausedPoster from '../assets/meeting-companion/paused-poster.png';
import pausedWebm from '../assets/meeting-companion/paused.webm';
import processingPoster from '../assets/meeting-companion/processing-poster.png';
import processingWebm from '../assets/meeting-companion/processing.webm';
import quietPoster from '../assets/meeting-companion/quiet-poster.png';
import quietWebm from '../assets/meeting-companion/quiet.webm';
import recordingPoster from '../assets/meeting-companion/recording-poster.png';
import recordingWebm from '../assets/meeting-companion/recording.webm';
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
  clampMeetingCompanionMenuPosition,
  executeMeetingCompanionCommand,
  MeetingCompanionCommandGate,
  MeetingCompanionControlsVisibility,
  meetingCompanionControlActions,
  meetingCompanionMenuActions,
  nextMeetingCompanionDialogFocusIndex,
  shouldStartMeetingCompanionDrag,
  type MeetingCompanionCommandAction,
  type MeetingCompanionControlAction,
  type MeetingCompanionMenuAction,
  type MeetingCompanionMenuPosition,
} from '../lib/meetingCompanionControls';
import {
  MEETING_COMPANION_MEDIA,
  MeetingCompanionMediaController,
  shouldPlayMeetingCompanionVideo,
  shouldShowMeetingCompanionMinimalFallback,
  type MeetingCompanionMediaState,
} from '../lib/meetingCompanionMedia';
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

const POSTER_URLS: Readonly<Record<string, string>> = {
  'idle-poster.png': idlePoster,
  'recording-poster.png': recordingPoster,
  'quiet-poster.png': quietPoster,
  'paused-poster.png': pausedPoster,
  'processing-poster.png': processingPoster,
  'completed-poster.png': completedPoster,
};

const VIDEO_URLS: Readonly<Record<string, string>> = {
  'idle.webm': idleWebm,
  'recording.webm': recordingWebm,
  'quiet.webm': quietWebm,
  'paused.webm': pausedWebm,
  'processing.webm': processingWebm,
  'completed.webm': completedWebm,
};

export interface MeetingCompanionProps {
  visualState?: MeetingCompanionVisualState;
  timerInput?: MeetingCompanionTimerInput | null;
  errorOverlay?: MeetingCompanionErrorOverlay | null;
  animationPaused?: boolean;
}

export function MeetingCompanion(props: MeetingCompanionProps) {
  const hasExplicitPresentation = props.visualState !== undefined
    || props.timerInput !== undefined
    || props.errorOverlay !== undefined
    || props.animationPaused !== undefined;
  if (isTauri && !hasExplicitPresentation) return <ConnectedMeetingCompanion />;
  return (
    <MeetingCompanionView
      visualState={props.visualState ?? 'idle'}
      timerInput={props.timerInput ?? null}
      errorOverlay={props.errorOverlay ?? null}
      animationPaused={props.animationPaused ?? false}
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
  interaction: MeetingCompanionInteraction | null;
  onCompletedPlaybackFinished?: () => void;
}

function MeetingCompanionView({
  visualState,
  timerInput,
  errorOverlay,
  animationPaused,
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
      timer.schedule(MEETING_COMPANION_MEDIA.idle.durationMs, () => {
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
  visualState: MeetingCompanionMediaState;
  timerInput: MeetingCompanionTimerInput | null;
  errorOverlay: MeetingCompanionErrorOverlay | null;
  animationPaused: boolean;
  interaction: MeetingCompanionInteraction | null;
  onCompletedPlaybackFinished?: () => void;
}

function VisibleMeetingCompanion({
  visualState,
  timerInput,
  errorOverlay,
  animationPaused,
  interaction,
  onCompletedPlaybackFinished,
}: VisibleMeetingCompanionProps) {
  const { t } = useTranslation();
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const mediaControllerRef = useRef<MeetingCompanionMediaController | null>(null);
  if (!mediaControllerRef.current) {
    mediaControllerRef.current = new MeetingCompanionMediaController();
  }

  const reducedMotion = useReducedMotion();
  const documentVisible = useDocumentVisibility();
  const elapsedText = useMeetingCompanionElapsed(timerInput);
  const [videoReadyState, setVideoReadyState] = useState<MeetingCompanionMediaState | null>(null);
  const [mediaFailedState, setMediaFailedState] = useState<MeetingCompanionMediaState | null>(null);
  const [posterFailedState, setPosterFailedState] = useState<MeetingCompanionMediaState | null>(null);
  const [controlsVisible, setControlsVisible] = useState(false);
  const [contextMenu, setContextMenu] = useState<MeetingCompanionMenuPosition | null>(null);
  const [stopConfirmOpen, setStopConfirmOpen] = useState(false);
  const completionNotifiedRef = useRef(false);
  const controlsVisibilityRef = useRef<MeetingCompanionControlsVisibility | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const cancelStopRef = useRef<HTMLButtonElement | null>(null);
  const dragGestureRef = useRef<{
    pointerId: number;
    startX: number;
    startY: number;
    started: boolean;
  } | null>(null);
  if (!controlsVisibilityRef.current) {
    controlsVisibilityRef.current = new MeetingCompanionControlsVisibility(setControlsVisible);
  }

  const mediaConfig = MEETING_COMPANION_MEDIA[visualState];
  const posterUrl = POSTER_URLS[mediaConfig.poster] ?? '';
  const videoUrl = VIDEO_URLS[mediaConfig.webm] ?? '';
  const videoReady = videoReadyState === visualState;
  const posterFailed = !posterUrl || posterFailedState === visualState;
  const videoEnabled = !animationPaused
    && shouldPlayMeetingCompanionVideo(visualState, reducedMotion, documentVisible)
    && mediaFailedState !== visualState;
  const videoVisible = videoReady && videoEnabled;
  const minimalFallback = shouldShowMeetingCompanionMinimalFallback(posterFailed, videoVisible);
  const errorKind = errorOverlay?.kind ?? null;
  const controlActions = interaction
    ? meetingCompanionControlActions(visualState, errorKind)
    : [];
  const menuActions = interaction
    ? meetingCompanionMenuActions(visualState, errorKind, interaction.positionLocked)
    : [];

  useEffect(() => () => controlsVisibilityRef.current?.dispose(), []);

  useEffect(() => {
    if (contextMenu || stopConfirmOpen || errorKind === 'summary_failed') {
      controlsVisibilityRef.current?.show();
    }
  }, [contextMenu, errorKind, stopConfirmOpen]);

  useEffect(() => {
    if (!stopConfirmOpen) return;
    const stopAllowed = controlActions.some(action => action.action === 'stop');
    if (!stopAllowed) setStopConfirmOpen(false);
  }, [controlActions, stopConfirmOpen]);

  useEffect(() => {
    if (!contextMenu) return;
    const closeOnPointerDown = (event: PointerEvent) => {
      if (menuRef.current?.contains(event.target as Node)) return;
      setContextMenu(null);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setContextMenu(null);
    };
    document.addEventListener('pointerdown', closeOnPointerDown, true);
    document.addEventListener('keydown', closeOnEscape, true);
    return () => {
      document.removeEventListener('pointerdown', closeOnPointerDown, true);
      document.removeEventListener('keydown', closeOnEscape, true);
    };
  }, [contextMenu]);

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
    setVideoReadyState(null);
    setMediaFailedState(null);
    setPosterFailedState(null);
    completionNotifiedRef.current = false;
  }, [visualState]);

  const notifyCompletedPlaybackFinished = useCallback(() => {
    if (visualState !== 'completed' || completionNotifiedRef.current) return;
    completionNotifiedRef.current = true;
    onCompletedPlaybackFinished?.();
  }, [onCompletedPlaybackFinished, visualState]);

  useEffect(() => {
    if (visualState === 'completed' && !videoEnabled) {
      notifyCompletedPlaybackFinished();
    }
  }, [notifyCompletedPlaybackFinished, videoEnabled, visualState]);

  useEffect(() => {
    const controller = mediaControllerRef.current;
    if (!controller || !videoEnabled || !videoRef.current) {
      controller?.stop();
      setVideoReadyState(null);
      return;
    }

    const stateAtStart = visualState;
    setVideoReadyState(null);
    controller.start({
      video: videoRef.current,
      source: videoUrl,
      loop: mediaConfig.loop,
      onReady: () => setVideoReadyState(stateAtStart),
      onFallback: reason => {
        console.warn(`[meeting-companion] ${stateAtStart} video fallback: ${reason}`);
        setVideoReadyState(null);
        setMediaFailedState(stateAtStart);
      },
    });
    return () => controller.stop();
  }, [mediaConfig.loop, videoEnabled, videoUrl, visualState]);

  useEffect(() => () => mediaControllerRef.current?.stop(), []);

  const startDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (
      event.button !== 0
      || interaction?.positionLocked
      || stopConfirmOpen
      || contextMenu
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

  const mediaMode = reducedMotion
    ? 'poster-reduced-motion'
    : minimalFallback
      ? 'minimal-fallback'
      : videoVisible
        ? 'video'
        : 'poster';

  const errorLabel = errorKind === 'summary_failed'
    ? t('meetingCompanion.summaryFailed')
    : errorKind === 'transcribing_interrupted'
      ? t('meetingCompanion.asrInterrupted')
      : errorKind === 'command_failed'
        ? t('meetingCompanion.commandFailed')
        : null;

  const showControls = () => controlsVisibilityRef.current?.show();
  const scheduleControlsHide = () => {
    if (!contextMenu && !stopConfirmOpen && errorKind !== 'summary_failed') {
      controlsVisibilityRef.current?.scheduleHide();
    }
  };

  const runControlAction = (action: MeetingCompanionControlAction) => {
    if (!interaction) return;
    if (action === 'stop') {
      setContextMenu(null);
      setStopConfirmOpen(true);
      showControls();
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
    setContextMenu(null);
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
    showControls();
    const width = 174;
    const height = menuActions.length * 34 + 8;
    setContextMenu(clampMeetingCompanionMenuPosition(
      event.clientX,
      event.clientY,
      width,
      height,
      window.innerWidth,
      window.innerHeight,
    ));
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
      data-meeting-companion-media-mode={mediaMode}
      data-meeting-companion-loop={mediaConfig.loop && !animationPaused ? 'true' : 'false'}
      data-meeting-companion-duration-ms={mediaConfig.durationMs}
      onPointerEnter={showControls}
      onPointerLeave={scheduleControlsHide}
      onClick={showControls}
      onContextMenu={openContextMenu}
      style={{
        width: 350,
        height: 324,
        flex: '0 0 350px',
        position: 'relative',
        overflow: 'hidden',
        display: 'flex',
        flexDirection: 'column',
        userSelect: 'none',
      }}
    >
      <div
        data-meeting-companion-stage
        onPointerDown={startDrag}
        onPointerMove={continueDrag}
        onPointerUp={finishDrag}
        onPointerCancel={cancelDrag}
        style={{
          width: 350,
          height: 280,
          flex: '0 0 280px',
          position: 'relative',
          overflow: 'hidden',
          cursor: interaction?.positionLocked ? 'default' : 'grab',
          touchAction: 'none',
        }}
      >
        {!posterFailed && (
          <img
            data-meeting-companion-poster
            data-meeting-companion-poster-state={visualState}
            src={posterUrl}
            alt=""
            draggable={false}
            onError={() => setPosterFailedState(visualState)}
            style={{
              display: 'block',
              position: 'absolute',
              inset: 0,
              width: '100%',
              height: '100%',
              objectFit: 'contain',
              opacity: videoVisible ? 0 : 1,
              pointerEvents: 'none',
            }}
          />
        )}

        {videoEnabled && (
          <video
            ref={videoRef}
            data-meeting-companion-video
            data-meeting-companion-video-state={visualState}
            aria-hidden="true"
            muted
            playsInline
            onEnded={notifyCompletedPlaybackFinished}
            style={{
              display: 'block',
              position: 'absolute',
              inset: 0,
              width: '100%',
              height: '100%',
              objectFit: 'contain',
              opacity: videoVisible ? 1 : 0,
              pointerEvents: 'none',
            }}
          />
        )}

        {minimalFallback ? (
          <div
            data-meeting-companion-minimal-fallback
            style={{
              position: 'absolute',
              left: '50%',
              top: '50%',
              transform: 'translate(-50%, -50%)',
              minWidth: 108,
              height: 38,
              padding: '0 12px',
              boxSizing: 'border-box',
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              gap: 8,
              border: '1px solid rgba(31, 41, 55, 0.18)',
              borderRadius: 8,
              background: 'rgba(255, 255, 255, 0.94)',
              color: '#1f2937',
              boxShadow: '0 4px 16px rgba(31, 41, 55, 0.14)',
              pointerEvents: 'none',
            }}
          >
            <span
              aria-hidden="true"
              style={{
                width: 8,
                height: 8,
                flex: '0 0 8px',
                borderRadius: '50%',
                background: statusColor(visualState),
              }}
            />
            <span style={{ font: '600 13px ui-monospace, SFMono-Regular, Consolas, monospace' }}>
              {elapsedText}
            </span>
          </div>
        ) : (
          <div
            data-meeting-companion-timer
            style={{
              position: 'absolute',
              left: 268,
              top: 192,
              width: 31,
              height: 12,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              overflow: 'hidden',
              color: '#f8d6dc',
              fontFamily: 'ui-monospace, SFMono-Regular, Consolas, monospace',
              fontSize: elapsedText.length > 5 ? 6 : 8,
              fontWeight: 700,
              fontVariantNumeric: 'tabular-nums',
              letterSpacing: 0,
              lineHeight: 1,
              whiteSpace: 'nowrap',
              textShadow: '0 1px rgba(31, 20, 24, 0.65)',
              pointerEvents: 'none',
            }}
          >
            {elapsedText}
          </div>
        )}
        {errorOverlay && (
          <>
            <div
              data-meeting-companion-error={errorOverlay.kind}
              aria-hidden="true"
              style={{
                position: 'absolute',
                right: 18,
                top: 18,
                width: 28,
                height: 28,
                display: 'grid',
                placeItems: 'center',
                border: '1px solid rgba(255, 255, 255, 0.88)',
                borderRadius: '50%',
                background: errorOverlay.kind === 'summary_failed' ? '#c2413b' : '#d97706',
                color: '#fff',
                boxShadow: '0 3px 10px rgba(31, 41, 55, 0.24)',
                pointerEvents: 'none',
              }}
            >
              {errorOverlay.kind === 'summary_failed'
                ? <CircleAlert size={17} strokeWidth={2.4} />
                : <TriangleAlert size={17} strokeWidth={2.4} />}
            </div>
            <div
              data-meeting-companion-error-message={errorOverlay.kind}
              role="status"
              aria-live="polite"
              style={{
                position: 'absolute',
                left: '50%',
                top: 16,
                transform: 'translateX(-50%)',
                maxWidth: 224,
                minHeight: 28,
                padding: '6px 10px',
                boxSizing: 'border-box',
                borderRadius: 8,
                background: errorOverlay.kind === 'summary_failed'
                  ? 'rgba(194, 65, 59, 0.94)'
                  : 'rgba(180, 83, 9, 0.94)',
                color: '#fff',
                fontSize: 11,
                fontWeight: 650,
                lineHeight: 1.35,
                textAlign: 'center',
                pointerEvents: 'none',
              }}
            >
              {errorLabel}
            </div>
          </>
        )}
      </div>

      <div
        data-meeting-companion-controls
        data-meeting-companion-controls-visible={controlsVisible ? 'true' : 'false'}
        style={{
          width: 350,
          height: 44,
          flex: '0 0 44px',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          gap: 8,
          pointerEvents: controlsVisible && controlActions.length > 0 ? 'auto' : 'none',
        }}
      >
        {controlsVisible && controlActions.map(action => {
          const disabled = Boolean(interaction?.busyAction);
          const label = t(action.labelKey);
          const openMeeting = action.action === 'open-meeting';
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
              onClick={event => {
                event.stopPropagation();
                runControlAction(action.action);
              }}
              style={{
                width: openMeeting ? 116 : 34,
                height: 34,
                padding: openMeeting ? '0 12px' : 0,
                display: 'inline-flex',
                alignItems: 'center',
                justifyContent: 'center',
                gap: 7,
                border: action.danger
                  ? '1px solid rgba(194, 65, 59, 0.42)'
                  : '1px solid rgba(31, 41, 55, 0.18)',
                borderRadius: 8,
                background: action.danger ? 'rgba(255, 241, 240, 0.96)' : 'rgba(255, 255, 255, 0.96)',
                color: action.danger ? '#b42318' : '#1f2937',
                boxShadow: '0 3px 12px rgba(31, 41, 55, 0.16)',
                fontFamily: 'inherit',
                fontSize: 12,
                fontWeight: 650,
                letterSpacing: 0,
                cursor: disabled ? 'not-allowed' : 'pointer',
                opacity: disabled ? 0.58 : 1,
              }}
            >
              {meetingCompanionActionIcon(action.action, 17)}
              {openMeeting && <span>{label}</span>}
            </button>
          );
        })}
      </div>

      {contextMenu && interaction && (
        <div
          ref={menuRef}
          data-meeting-companion-context-menu
          role="menu"
          onPointerDown={event => event.stopPropagation()}
          onContextMenu={event => event.preventDefault()}
          style={{
            position: 'absolute',
            left: contextMenu.x,
            top: contextMenu.y,
            width: 174,
            padding: 4,
            boxSizing: 'border-box',
            display: 'flex',
            flexDirection: 'column',
            border: '1px solid rgba(31, 41, 55, 0.18)',
            borderRadius: 8,
            background: 'rgba(255, 255, 255, 0.98)',
            color: '#1f2937',
            boxShadow: '0 8px 24px rgba(31, 41, 55, 0.22)',
            zIndex: 20,
          }}
        >
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
                aria-label={label}
                disabled={disabled}
                onClick={() => runMenuAction(action.action)}
                style={{
                  width: '100%',
                  height: 34,
                  padding: '0 9px',
                  display: 'flex',
                  alignItems: 'center',
                  gap: 9,
                  border: 0,
                  borderRadius: 6,
                  background: 'transparent',
                  color: action.danger ? '#b42318' : '#1f2937',
                  fontFamily: 'inherit',
                  fontSize: 12,
                  fontWeight: 550,
                  letterSpacing: 0,
                  textAlign: 'left',
                  cursor: disabled ? 'not-allowed' : 'pointer',
                  opacity: disabled ? 0.52 : 1,
                }}
              >
                {meetingCompanionActionIcon(action.action, 16, interaction.positionLocked)}
                <span>{label}</span>
              </button>
            );
          })}
        </div>
      )}

      {stopConfirmOpen && interaction && (
        <div
          data-meeting-companion-stop-dialog-backdrop
          onPointerDown={event => event.stopPropagation()}
          style={{
            position: 'absolute',
            inset: 0,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            background: 'rgba(17, 24, 39, 0.28)',
            zIndex: 30,
          }}
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
              width: 286,
              padding: 16,
              boxSizing: 'border-box',
              border: '1px solid rgba(31, 41, 55, 0.18)',
              borderRadius: 8,
              background: '#fff',
              color: '#1f2937',
              boxShadow: '0 12px 32px rgba(17, 24, 39, 0.28)',
            }}
          >
            <div
              id="meeting-companion-stop-title"
              style={{ fontSize: 15, fontWeight: 700, lineHeight: 1.35 }}
            >
              {t('meetingCompanion.stopConfirmTitle')}
            </div>
            <div
              id="meeting-companion-stop-body"
              style={{ marginTop: 7, fontSize: 12, lineHeight: 1.5, color: '#4b5563' }}
            >
              {t('meetingCompanion.stopConfirmBody')}
            </div>
            <div
              style={{
                marginTop: 14,
                display: 'flex',
                justifyContent: 'flex-end',
                gap: 8,
              }}
            >
              <button
                ref={cancelStopRef}
                type="button"
                className="ol-focus-ring"
                aria-label={t('meetingCompanion.cancel')}
                disabled={interaction.busyAction === 'stop'}
                onClick={() => setStopConfirmOpen(false)}
                style={dialogButtonStyle(false, interaction.busyAction === 'stop')}
              >
                {t('meetingCompanion.cancel')}
              </button>
              <button
                type="button"
                className="ol-focus-ring"
                data-meeting-companion-confirm-stop
                aria-label={t('meetingCompanion.confirmStop')}
                disabled={interaction.busyAction !== null}
                onClick={confirmStop}
                style={dialogButtonStyle(true, interaction.busyAction !== null)}
              >
                {t('meetingCompanion.confirmStop')}
              </button>
            </div>
          </div>
        </div>
      )}
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

function dialogButtonStyle(danger: boolean, disabled: boolean): React.CSSProperties {
  return {
    minWidth: 84,
    height: 32,
    padding: '0 12px',
    border: danger ? '1px solid #b42318' : '1px solid rgba(31, 41, 55, 0.22)',
    borderRadius: 7,
    background: danger ? '#b42318' : '#fff',
    color: danger ? '#fff' : '#1f2937',
    fontFamily: 'inherit',
    fontSize: 12,
    fontWeight: 650,
    letterSpacing: 0,
    cursor: disabled ? 'not-allowed' : 'pointer',
    opacity: disabled ? 0.58 : 1,
  };
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

function statusColor(visualState: MeetingCompanionMediaState): string {
  if (visualState === 'paused') return '#d97706';
  if (visualState === 'completed') return '#16a34a';
  if (visualState === 'processing') return '#64748b';
  return '#dc2626';
}
