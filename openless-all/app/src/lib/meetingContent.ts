import type { MeetingRecord, TranscriptSegment } from './types';

export const MEETING_CONTENT_TABS = ['summary', 'organized', 'transcript'] as const;
export type MeetingContentTab = typeof MEETING_CONTENT_TABS[number];

export type MeetingTabNavigationKey = 'ArrowLeft' | 'ArrowRight' | 'Home' | 'End';

export function moveMeetingContentTab(
  current: MeetingContentTab,
  key: MeetingTabNavigationKey,
): MeetingContentTab {
  if (key === 'Home') return MEETING_CONTENT_TABS[0];
  if (key === 'End') return MEETING_CONTENT_TABS[MEETING_CONTENT_TABS.length - 1];
  const currentIndex = MEETING_CONTENT_TABS.indexOf(current);
  const offset = key === 'ArrowRight' ? 1 : -1;
  return MEETING_CONTENT_TABS[
    (currentIndex + offset + MEETING_CONTENT_TABS.length) % MEETING_CONTENT_TABS.length
  ];
}

export type MeetingTranscriptViewKind =
  | 'realtime_revision'
  | 'realtime_segments'
  | 'postprocessed_revision'
  | 'legacy'
  | 'imported_empty'
  | 'empty';

export interface MeetingTranscriptView {
  segments: TranscriptSegment[];
  kind: MeetingTranscriptViewKind;
  historical: boolean;
}

export function meetingTranscriptView(record: MeetingRecord): MeetingTranscriptView {
  const realtimeRevision = [...(record.transcriptRevisions ?? [])]
    .filter(revision => revision.source === 'realtime')
    .sort((left, right) => right.revision - left.revision)[0];
  if (realtimeRevision) {
    return {
      segments: realtimeRevision.segments,
      kind: 'realtime_revision',
      historical: false,
    };
  }

  const realtimeSegments = record.transcriptSegments.filter(
    segment => segment.source === 'realtime_asr',
  );
  if (realtimeSegments.length > 0) {
    return {
      segments: realtimeSegments,
      kind: 'realtime_segments',
      historical: false,
    };
  }

  if (record.importConfig) {
    return { segments: [], kind: 'imported_empty', historical: false };
  }

  const activeRevision = record.activeTranscriptRevision == null
    ? null
    : (record.transcriptRevisions ?? []).find(
      revision => revision.revision === record.activeTranscriptRevision,
    ) ?? null;
  if (activeRevision && activeRevision.source !== 'realtime') {
    return {
      segments: activeRevision.segments,
      kind: 'postprocessed_revision',
      historical: false,
    };
  }

  if (record.transcriptSegments.length > 0) {
    return {
      segments: record.transcriptSegments,
      kind: 'legacy',
      historical: true,
    };
  }

  return { segments: [], kind: 'empty', historical: false };
}

export type OrganizedDraftViewState =
  | 'waiting_transcript'
  | 'missing'
  | 'pending'
  | 'running'
  | 'failed'
  | 'stale'
  | 'completed';

export interface OrganizedDraftView {
  state: OrganizedDraftViewState;
  hasDraft: boolean;
  stale: boolean;
}

export function organizedDraftView(record: MeetingRecord): OrganizedDraftView {
  const draft = record.organizedDraft ?? null;
  const task = record.organizedDraftState ?? null;
  const stale = Boolean(
    draft
      && (draft.sourceTranscriptRevision ?? null) !== (record.activeTranscriptRevision ?? null),
  );
  const transcriptFinal = ![
    'draft',
    'recording',
    'paused',
    'transcribing_interrupted',
  ].includes(record.status)
    && (!record.postProcessing || [
      'completed',
      'realtime_accepted',
    ].includes(record.postProcessing.status));

  if (!transcriptFinal) return { state: 'waiting_transcript', hasDraft: Boolean(draft), stale };
  if (task?.status === 'pending') return { state: 'pending', hasDraft: Boolean(draft), stale };
  if (task?.status === 'running') return { state: 'running', hasDraft: Boolean(draft), stale };
  if (task?.status === 'failed') return { state: 'failed', hasDraft: Boolean(draft), stale };
  if (draft && stale) return { state: 'stale', hasDraft: true, stale: true };
  if (draft) return { state: 'completed', hasDraft: true, stale: false };
  return { state: 'missing', hasDraft: false, stale: false };
}
