import { useEffect, useRef, useState } from 'react';
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
import { saveMeetingCompanionPosition, startMeetingCompanionDrag } from '../lib/ipc';
import {
  MEETING_COMPANION_MEDIA,
  MeetingCompanionMediaController,
  shouldPlayMeetingCompanionVideo,
  shouldShowMeetingCompanionMinimalFallback,
  type MeetingCompanionMediaState,
} from '../lib/meetingCompanionMedia';
import type { MeetingCompanionVisualState } from '../lib/meetingCompanionState';
import {
  formatMeetingCompanionElapsed,
  MeetingCompanionElapsedClock,
  type MeetingCompanionTimerInput,
} from '../lib/meetingCompanionTimer';

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
}

export function MeetingCompanion({
  visualState = 'idle',
  timerInput = null,
}: MeetingCompanionProps) {
  if (visualState === 'hidden') return null;
  return <VisibleMeetingCompanion visualState={visualState} timerInput={timerInput} />;
}

interface VisibleMeetingCompanionProps {
  visualState: MeetingCompanionMediaState;
  timerInput: MeetingCompanionTimerInput | null;
}

function VisibleMeetingCompanion({ visualState, timerInput }: VisibleMeetingCompanionProps) {
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

  const mediaConfig = MEETING_COMPANION_MEDIA[visualState];
  const posterUrl = POSTER_URLS[mediaConfig.poster] ?? '';
  const videoUrl = VIDEO_URLS[mediaConfig.webm] ?? '';
  const videoReady = videoReadyState === visualState;
  const posterFailed = !posterUrl || posterFailedState === visualState;
  const videoEnabled = shouldPlayMeetingCompanionVideo(visualState, reducedMotion, documentVisible)
    && mediaFailedState !== visualState;
  const videoVisible = videoReady && videoEnabled;
  const minimalFallback = shouldShowMeetingCompanionMinimalFallback(posterFailed, videoVisible);

  useEffect(() => {
    setVideoReadyState(null);
    setMediaFailedState(null);
    setPosterFailedState(null);
  }, [visualState]);

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
      data-meeting-companion-loop={mediaConfig.loop ? 'true' : 'false'}
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
