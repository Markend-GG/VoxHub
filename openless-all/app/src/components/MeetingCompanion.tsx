import { useCallback, useEffect, useReducer, useRef, useState } from 'react';
import { CircleAlert, TriangleAlert } from 'lucide-react';
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
  isTauri,
  saveMeetingCompanionPosition,
  startMeetingCompanionDrag,
} from '../lib/ipc';
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
    />
  );
}

interface MeetingCompanionViewProps {
  visualState: MeetingCompanionVisualState;
  timerInput: MeetingCompanionTimerInput | null;
  errorOverlay: MeetingCompanionErrorOverlay | null;
  animationPaused: boolean;
  onCompletedPlaybackFinished?: () => void;
}

function MeetingCompanionView({
  visualState,
  timerInput,
  errorOverlay,
  animationPaused,
  onCompletedPlaybackFinished,
}: MeetingCompanionViewProps) {
  if (visualState === 'hidden') return null;
  return (
    <VisibleMeetingCompanion
      visualState={visualState}
      timerInput={timerInput}
      errorOverlay={errorOverlay}
      animationPaused={animationPaused}
      onCompletedPlaybackFinished={onCompletedPlaybackFinished}
    />
  );
}

function ConnectedMeetingCompanion() {
  const [state, dispatch] = useReducer(meetingCompanionReducer, initialMeetingCompanionState);
  const gateRef = useRef<MeetingCompanionEventGate | null>(null);
  const quietDetectorRef = useRef<MeetingCompanionQuietDetector | null>(null);
  const transitionTimerRef = useRef<MeetingCompanionTransitionTimer | null>(null);
  if (!gateRef.current) gateRef.current = new MeetingCompanionEventGate();
  if (!quietDetectorRef.current) quietDetectorRef.current = new MeetingCompanionQuietDetector();
  if (!transitionTimerRef.current) transitionTimerRef.current = new MeetingCompanionTransitionTimer();

  const documentVisible = useDocumentVisibility();

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

    const applySnapshot = (snapshot: MeetingRecordingSnapshot) => {
      if (!gate.acceptSnapshotEvent(snapshot)) return;
      const quietContext = detector.setContext(
        snapshot.meeting.id,
        isMeetingCompanionQuietEligible(snapshot),
      );
      dispatch({ type: 'snapshot', snapshot });
      if (quietContext.changed) {
        dispatch({ type: 'quiet-changed', meetingId: snapshot.meeting.id, quiet: false });
      }
    };

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
            if (!cancelled) applySnapshot(event.payload);
          })) return;
        if (!await register<MeetingSummaryEvent>('meeting:summary', event => {
            if (cancelled) return;
            const payload = event.payload;
            if (!gate.acceptRelatedEvent(
              payload.meetingId,
              true,
              payload.meeting?.startedAt ?? null,
            )) return;
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
  }, [documentVisible]);

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

  useEffect(() => () => {
    transitionTimerRef.current?.clear();
    quietDetectorRef.current?.setContext(null, false);
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
      onCompletedPlaybackFinished={handleCompletedPlaybackFinished}
    />
  );
}

interface VisibleMeetingCompanionProps {
  visualState: MeetingCompanionMediaState;
  timerInput: MeetingCompanionTimerInput | null;
  errorOverlay: MeetingCompanionErrorOverlay | null;
  animationPaused: boolean;
  onCompletedPlaybackFinished?: () => void;
}

function VisibleMeetingCompanion({
  visualState,
  timerInput,
  errorOverlay,
  animationPaused,
  onCompletedPlaybackFinished,
}: VisibleMeetingCompanionProps) {
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
  const completionNotifiedRef = useRef(false);

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
    if (event.button !== 0) return;
    event.preventDefault();
    void startMeetingCompanionDrag().catch(error => {
      console.warn('[meeting-companion] start drag failed', error);
    });
  };

  const finishDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    void saveMeetingCompanionPosition().catch(error => {
      console.warn('[meeting-companion] save position failed', error);
    });
  };

  const mediaMode = reducedMotion
    ? 'poster-reduced-motion'
    : minimalFallback
      ? 'minimal-fallback'
      : videoVisible
        ? 'video'
        : 'poster';

  return (
    <div
      data-meeting-companion-root
      data-meeting-companion-state={visualState}
      data-meeting-companion-media-mode={mediaMode}
      data-meeting-companion-loop={mediaConfig.loop && !animationPaused ? 'true' : 'false'}
      data-meeting-companion-duration-ms={mediaConfig.durationMs}
      onPointerDown={startDrag}
      onPointerUp={finishDrag}
      onPointerCancel={() => {
        void saveMeetingCompanionPosition().catch(error => {
          console.warn('[meeting-companion] save cancelled drag position failed', error);
        });
      }}
      style={{
        width: 350,
        height: 280,
        flex: '0 0 350px',
        position: 'relative',
        overflow: 'hidden',
        cursor: 'grab',
        userSelect: 'none',
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
      )}
    </div>
  );
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
