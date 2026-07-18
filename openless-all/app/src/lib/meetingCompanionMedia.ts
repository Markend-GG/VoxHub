import runtimeManifest from '../assets/meeting-companion/manifest.json';
import type { MeetingCompanionVisualState } from './meetingCompanionState';

export type MeetingCompanionMediaState = Exclude<MeetingCompanionVisualState, 'hidden'>;

export interface MeetingCompanionMediaConfig {
  state: MeetingCompanionMediaState;
  webm: string;
  poster: string;
  loop: boolean;
  durationMs: number;
}

const MEDIA_STATES = [
  'idle',
  'recording',
  'quiet',
  'paused',
  'processing',
  'completed',
] as const satisfies readonly MeetingCompanionMediaState[];

function loadRuntimeConfig(): Readonly<Record<MeetingCompanionMediaState, MeetingCompanionMediaConfig>> {
  if (runtimeManifest.states.length !== MEDIA_STATES.length) {
    throw new Error('meeting companion runtime manifest must contain exactly six states');
  }

  const entries = MEDIA_STATES.map(state => {
    const matches = runtimeManifest.states.filter(entry => entry.state === state);
    if (matches.length !== 1) {
      throw new Error(`meeting companion runtime manifest must contain one ${state} state`);
    }
    const entry = matches[0];
    if (
      !entry.webm
      || !entry.poster
      || typeof entry.loop !== 'boolean'
      || !Number.isFinite(entry.durationMs)
      || entry.durationMs <= 0
    ) {
      throw new Error(`meeting companion runtime manifest contains invalid ${state} media metadata`);
    }
    return [state, Object.freeze({ ...entry, state })] as const;
  });

  return Object.freeze(Object.fromEntries(entries)) as Readonly<
    Record<MeetingCompanionMediaState, MeetingCompanionMediaConfig>
  >;
}

export const MEETING_COMPANION_MEDIA = loadRuntimeConfig();

export function shouldPlayMeetingCompanionVideo(
  visualState: MeetingCompanionVisualState,
  reducedMotion: boolean,
  documentVisible: boolean,
): visualState is MeetingCompanionMediaState {
  return visualState !== 'hidden' && !reducedMotion && documentVisible;
}

export function shouldShowMeetingCompanionMinimalFallback(
  posterFailed: boolean,
  videoReady: boolean,
): boolean {
  return posterFailed && !videoReady;
}

export type MeetingCompanionMediaFallbackReason =
  | 'missing-source'
  | 'load-timeout'
  | 'media-error'
  | 'play-rejected';

export interface MeetingCompanionVideoElement {
  src: string;
  loop: boolean;
  muted: boolean;
  playsInline: boolean;
  preload: string;
  currentTime: number;
  addEventListener(type: string, listener: EventListenerOrEventListenerObject): void;
  removeEventListener(type: string, listener: EventListenerOrEventListenerObject): void;
  pause(): void;
  play(): Promise<void>;
  load(): void;
  removeAttribute(name: string): void;
}

export interface MeetingCompanionTimeoutScheduler {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

const defaultScheduler: MeetingCompanionTimeoutScheduler = {
  setTimeout: (callback, delayMs) => globalThis.setTimeout(callback, delayMs),
  clearTimeout: handle => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

interface ActiveMediaSession {
  generation: number;
  video: MeetingCompanionVideoElement;
  canPlayListener: EventListener;
  errorListener: EventListener;
  timeoutHandle: unknown | null;
  playRequested: boolean;
  onReady: () => void;
  onFallback: (reason: MeetingCompanionMediaFallbackReason) => void;
}

export interface StartMeetingCompanionMediaInput {
  video: MeetingCompanionVideoElement;
  source: string;
  loop: boolean;
  onReady: () => void;
  onFallback: (reason: MeetingCompanionMediaFallbackReason) => void;
}

export class MeetingCompanionMediaController {
  private generation = 0;
  private active: ActiveMediaSession | null = null;

  constructor(
    private readonly scheduler: MeetingCompanionTimeoutScheduler = defaultScheduler,
    private readonly loadTimeoutMs = 2000,
  ) {}

  start(input: StartMeetingCompanionMediaInput): void {
    this.stop();
    const generation = this.generation;
    const session = {} as ActiveMediaSession;

    session.generation = generation;
    session.video = input.video;
    session.timeoutHandle = null;
    session.playRequested = false;
    session.onReady = input.onReady;
    session.onFallback = input.onFallback;
    session.canPlayListener = () => this.handleCanPlay(session);
    session.errorListener = () => this.fallback(session, 'media-error');
    this.active = session;

    const { video } = session;
    video.loop = input.loop;
    video.muted = true;
    video.playsInline = true;
    video.preload = 'auto';
    video.addEventListener('canplay', session.canPlayListener);
    video.addEventListener('error', session.errorListener);

    if (!input.source) {
      this.fallback(session, 'missing-source');
      return;
    }

    video.src = input.source;
    session.timeoutHandle = this.scheduler.setTimeout(
      () => this.fallback(session, 'load-timeout'),
      this.loadTimeoutMs,
    );
    try {
      video.load();
    } catch {
      this.fallback(session, 'media-error');
    }
  }

  stop(): void {
    this.generation += 1;
    const session = this.active;
    this.active = null;
    if (session) this.releaseSession(session);
  }

  private handleCanPlay(session: ActiveMediaSession): void {
    if (!this.isCurrent(session) || session.playRequested) return;
    session.playRequested = true;
    this.clearLoadTimeout(session);
    session.video.removeEventListener('canplay', session.canPlayListener);

    let playResult: Promise<void>;
    try {
      playResult = session.video.play();
    } catch {
      this.fallback(session, 'play-rejected');
      return;
    }

    void Promise.resolve(playResult).then(
      () => {
        if (this.isCurrent(session)) session.onReady();
      },
      () => this.fallback(session, 'play-rejected'),
    );
  }

  private fallback(
    session: ActiveMediaSession,
    reason: MeetingCompanionMediaFallbackReason,
  ): void {
    if (!this.isCurrent(session)) return;
    this.active = null;
    this.generation += 1;
    this.releaseSession(session);
    session.onFallback(reason);
  }

  private isCurrent(session: ActiveMediaSession): boolean {
    return this.active === session && session.generation === this.generation;
  }

  private clearLoadTimeout(session: ActiveMediaSession): void {
    if (session.timeoutHandle === null) return;
    this.scheduler.clearTimeout(session.timeoutHandle);
    session.timeoutHandle = null;
  }

  private releaseSession(session: ActiveMediaSession): void {
    this.clearLoadTimeout(session);
    session.video.removeEventListener('canplay', session.canPlayListener);
    session.video.removeEventListener('error', session.errorListener);
    releaseMeetingCompanionVideo(session.video);
  }
}

export function releaseMeetingCompanionVideo(video: MeetingCompanionVideoElement): void {
  video.pause();
  try {
    video.currentTime = 0;
  } catch {
    // WebView can reject seeks before metadata is available; removing src still releases decoding.
  }
  video.removeAttribute('src');
  video.load();
}
