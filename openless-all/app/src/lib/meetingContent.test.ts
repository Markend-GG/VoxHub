import {
  meetingTranscriptView,
  moveMeetingContentTab,
  organizedDraftView,
} from './meetingContent';
import type { MeetingRecord } from './types';

function assertEqual<T>(actual: T, expected: T, message: string) {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, got ${String(actual)}`);
  }
}

function assertDeepEqual(actual: unknown, expected: unknown, message: string) {
  const actualJson = JSON.stringify(actual);
  const expectedJson = JSON.stringify(expected);
  if (actualJson !== expectedJson) {
    throw new Error(`${message}: expected ${expectedJson}, got ${actualJson}`);
  }
}

function record(overrides: Partial<MeetingRecord> = {}): MeetingRecord {
  return {
    id: 'meeting-1',
    title: 'Meeting',
    status: 'completed',
    startedAt: '2026-08-25T08:00:00Z',
    endedAt: '2026-08-25T09:00:00Z',
    durationMs: 3_600_000,
    transcriptSegments: [],
    summary: {
      overview: '',
      keyDecisions: [],
      todos: [],
      risksAndOpenQuestions: [],
    },
    audio: { state: 'unavailable', retained: false, path: null },
    transcriptRevisions: [],
    activeTranscriptRevision: null,
    createdAt: '2026-08-25T08:00:00Z',
    updatedAt: '2026-08-25T09:00:00Z',
    ...overrides,
  };
}

assertEqual(moveMeetingContentTab('summary', 'ArrowRight'), 'organized', 'moves to organized tab');
assertEqual(moveMeetingContentTab('organized', 'ArrowRight'), 'transcript', 'moves to transcript tab');
assertEqual(moveMeetingContentTab('transcript', 'ArrowRight'), 'summary', 'wraps to summary tab');
assertEqual(moveMeetingContentTab('summary', 'ArrowLeft'), 'transcript', 'wraps backwards');
assertEqual(moveMeetingContentTab('organized', 'Home'), 'summary', 'Home selects first tab');
assertEqual(moveMeetingContentTab('organized', 'End'), 'transcript', 'End selects last tab');

const realtimeSegment = {
  id: 'rt-1',
  speakerId: null,
  speakerLabel: 'Speaker',
  startMs: 0,
  endMs: 1000,
  text: 'Realtime',
  source: 'realtime_asr' as const,
};
const postProcessedSegment = {
  ...realtimeSegment,
  id: 'post-1',
  text: 'Post processed',
  source: 'retranscribed_asr' as const,
};
const newerRealtimeSegment = {
  ...realtimeSegment,
  id: 'rt-2',
  text: 'Newest realtime',
};
assertDeepEqual(
  meetingTranscriptView(record({
    transcriptSegments: [postProcessedSegment],
    transcriptRevisions: [
      {
        revision: 0,
        source: 'realtime',
        status: 'rejected',
        segments: [realtimeSegment],
        createdAt: '2026-08-25T08:00:00Z',
      },
      {
        revision: 3,
        source: 'realtime',
        status: 'active',
        segments: [newerRealtimeSegment],
        createdAt: '2026-08-25T08:30:00Z',
      },
    ],
  })),
  { segments: [newerRealtimeSegment], kind: 'realtime_revision', historical: false },
  'uses the latest realtime revision instead of the active post-processed transcript',
);
assertDeepEqual(
  meetingTranscriptView(record({ transcriptSegments: [realtimeSegment] })),
  { segments: [realtimeSegment], kind: 'realtime_segments', historical: false },
  'uses realtime ASR segments when no realtime revision exists',
);
assertDeepEqual(
  meetingTranscriptView(record({ transcriptSegments: [postProcessedSegment] })),
  { segments: [postProcessedSegment], kind: 'legacy', historical: true },
  'uses legacy retranscribed segments for non-imported meetings',
);
assertDeepEqual(
  meetingTranscriptView(record({ importConfig: {} as MeetingRecord['importConfig'], transcriptSegments: [postProcessedSegment] })),
  { segments: [], kind: 'imported_empty', historical: false },
  'imported meetings without realtime segments show an empty transcript',
);
assertDeepEqual(
  meetingTranscriptView(record()),
  { segments: [], kind: 'empty', historical: false },
  'meetings without transcript data show an empty transcript',
);
assertDeepEqual(
  meetingTranscriptView(record({
    transcriptSegments: [postProcessedSegment],
    activeTranscriptRevision: 2,
    transcriptRevisions: [{
      revision: 2,
      source: 'cloud_postprocess',
      status: 'active',
      segments: [postProcessedSegment],
      createdAt: '2026-08-25T08:00:00Z',
    }],
  })),
  { segments: [postProcessedSegment], kind: 'postprocessed_revision', historical: false },
  'shows a newly activated post-processed revision when no realtime data exists',
);

assertEqual(organizedDraftView(record({ status: 'recording' })).state, 'waiting_transcript', 'recording waits for final transcript');
assertEqual(organizedDraftView(record()).state, 'missing', 'legacy meeting allows manual generation');
assertDeepEqual(organizedDraftView(record({ organizedDraftState: {
  status: 'pending',
  jobId: 'job-pending',
  processingRevision: 1,
  sourceTranscriptRevision: 1,
  attempt: 1,
  createdAt: '2026-08-25T09:00:00Z',
  updatedAt: '2026-08-25T09:00:00Z',
} })), { state: 'pending', hasDraft: false, stale: false }, 'pending task has a pending view');
assertEqual(organizedDraftView(record({ organizedDraftState: {
  status: 'running',
  jobId: 'job-1',
  processingRevision: 1,
  sourceTranscriptRevision: 1,
  attempt: 1,
  createdAt: '2026-08-25T09:00:00Z',
  updatedAt: '2026-08-25T09:00:00Z',
} })).state, 'running', 'running task has a running view');
assertDeepEqual(organizedDraftView(record({
  activeTranscriptRevision: 1,
  organizedDraft: {
    sourceTranscriptRevision: 1,
    providerId: 'provider',
    modelId: 'model',
    items: [],
    generatedAt: '2026-08-25T09:00:00Z',
  },
})), { state: 'completed', hasDraft: true, stale: false }, 'matching draft revision is completed');
assertDeepEqual(organizedDraftView(record({
  activeTranscriptRevision: 2,
  organizedDraft: {
    sourceTranscriptRevision: 1,
    providerId: 'provider',
    modelId: 'model',
    items: [],
    generatedAt: '2026-08-25T09:00:00Z',
  },
})), { state: 'stale', hasDraft: true, stale: true }, 'older draft revision is stale');
assertDeepEqual(organizedDraftView(record({
  activeTranscriptRevision: 2,
  organizedDraft: {
    sourceTranscriptRevision: 1,
    providerId: 'provider',
    modelId: 'model',
    items: [],
    generatedAt: '2026-08-25T09:00:00Z',
  },
  organizedDraftState: {
    status: 'failed',
    jobId: 'job-2',
    processingRevision: 2,
    sourceTranscriptRevision: 2,
    attempt: 2,
    createdAt: '2026-08-25T09:10:00Z',
    updatedAt: '2026-08-25T09:11:00Z',
  },
})), { state: 'failed', hasDraft: true, stale: true }, 'failed regeneration preserves and marks the old draft stale');

console.log('meetingContent tests passed');
