import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type MutableRefObject,
  type ReactNode,
} from 'react';
import { Volume2 } from 'lucide-react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { useVirtualizer } from '@tanstack/react-virtual';
import { useTranslation } from 'react-i18next';
import { Icon } from '../components/Icon';
import {
  type BinaryPayload,
  deleteMeetingRecord,
  exportMeetingMarkdown,
  generateMeetingSummary,
  getActiveMeetingRecording,
  getMeeting,
  retryMeetingSummary,
  listMeetings,
  pauseMeetingRecording,
  prepareMeetingAudioPlayback,
  resumeMeetingRecording,
  retranscribeMeeting,
  showMeetingCompanion,
  startMeetingRecording,
  stopMeetingRecording,
  updateMeetingRecord,
  isDesktop,
} from '../lib/ipc';
import type {
  MeetingCloseRequestEvent,
  MeetingAudioState,
  MeetingErrorEvent,
  MeetingListItem,
  MeetingRecord,
  MeetingRecordingPhase,
  MeetingRecordingSnapshot,
  MeetingStatus,
  MeetingSummaryEvent,
  MeetingTranscriptDraftEvent,
  MeetingTranscriptSegmentEvent,
  TranscriptSegment,
  TranscriptSegmentSource,
} from '../lib/types';
import { normalizeMeetingCloseRequest } from '../lib/types';
import { useMobileLayout } from '../lib/useMobileLayout';
import { useHotkeySettings } from '../state/HotkeySettingsContext';
import { Btn, Card, PageHeader, Pill, type PillTone } from './_atoms';

type ActionLoading = 'start' | 'pause' | 'resume' | 'stop' | 'summary' | 'save' | 'delete' | 'export' | 'retranscribe' | null;
type ActiveControlMode = 'recording' | 'paused';
const PLAYBACK_SPEEDS = [0.75, 1, 1.25, 1.5, 2] as const;
type PlaybackSpeed = typeof PLAYBACK_SPEEDS[number];

interface MeetingEditDraft {
  id: string;
  title: string;
  overview: string;
  keyDecisions: string;
  todos: MeetingTodoDraft[];
  risksAndOpenQuestions: string;
}

interface MeetingTodoDraft {
  id: string;
  content: string;
  owner: string;
  dueDate: string;
  sourceQuote: string;
  sourceSegmentIds: string[];
}

interface MeetingsProps {
  requestedMeetingId?: string | null;
  onRequestedMeetingHandled?: (meetingId: string) => void;
}

export function Meetings({
  requestedMeetingId = null,
  onRequestedMeetingHandled,
}: MeetingsProps = {}) {
  const { t } = useTranslation();
  const { prefs } = useHotkeySettings();
  const mobile = useMobileLayout();
  const [meetings, setMeetings] = useState<MeetingListItem[]>([]);
  const [meetingDetails, setMeetingDetails] = useState<Record<string, MeetingRecord>>({});
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [activeSnapshot, setActiveSnapshot] = useState<MeetingRecordingSnapshot | null>(null);
  const [query, setQuery] = useState('');
  const [loading, setLoading] = useState(true);
  const [actionLoading, setActionLoading] = useState<ActionLoading>(null);
  const [companionLoading, setCompanionLoading] = useState(false);
  const [activeControlMode, setActiveControlMode] = useState<{ meetingId: string; mode: ActiveControlMode } | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<string | null>(null);
  const [detailRetryNonce, setDetailRetryNonce] = useState(0);
  const [actionError, setActionError] = useState<string | null>(null);
  const [eventError, setEventError] = useState<MeetingErrorEvent | null>(null);
  const [draftByMeetingId, setDraftByMeetingId] = useState<Record<string, MeetingTranscriptDraftEvent>>({});
  const [editDraft, setEditDraft] = useState<MeetingEditDraft | null>(null);
  const [deleteConfirmId, setDeleteConfirmId] = useState<string | null>(null);
  const [rewriteConfirmId, setRewriteConfirmId] = useState<string | null>(null);
  const [mobileDetailOpen, setMobileDetailOpen] = useState(false);
  const transcriptScrollRef = useRef<HTMLDivElement | null>(null);
  const transcriptStickToBottomRef = useRef(true);
  const meetingsRef = useRef<MeetingListItem[]>([]);
  const meetingDetailsRef = useRef<Record<string, MeetingRecord>>({});
  const detailRequestRef = useRef(0);
  const activeSnapshotRef = useRef<MeetingRecordingSnapshot | null>(null);
  const requestedMeetingIdRef = useRef<string | null>(requestedMeetingId);

  useEffect(() => {
    meetingsRef.current = meetings;
  }, [meetings]);

  useEffect(() => {
    meetingDetailsRef.current = meetingDetails;
  }, [meetingDetails]);

  useEffect(() => {
    activeSnapshotRef.current = activeSnapshot;
  }, [activeSnapshot]);

  useEffect(() => {
    requestedMeetingIdRef.current = requestedMeetingId;
    if (!requestedMeetingId) return;
    setQuery('');
    setSelectedId(requestedMeetingId);
    setActionError(null);
    setDetailError(null);
    setEditDraft(null);
    setDeleteConfirmId(null);
    setRewriteConfirmId(null);
    if (mobile) setMobileDetailOpen(true);
    if (meetings.some(meeting => meeting.id === requestedMeetingId)) {
      onRequestedMeetingHandled?.(requestedMeetingId);
    }
  }, [meetings, mobile, onRequestedMeetingHandled, requestedMeetingId]);

  const cacheMeetingRecord = useCallback((record: MeetingRecord) => {
    setMeetings(prev => upsertMeetingListItem(prev, meetingListItemFromRecord(record)));
    setMeetingDetails(prev => ({ ...prev, [record.id]: record }));
  }, []);

  const syncActiveSnapshot = useCallback(async (expectedMeetingId?: string) => {
    try {
      const snapshot = await getActiveMeetingRecording();
      if (!snapshot) {
        setActiveSnapshot(null);
        setActiveControlMode(null);
        return;
      }
      if (expectedMeetingId && snapshot.meeting.id !== expectedMeetingId) return;
      setActiveSnapshot(snapshot);
      cacheMeetingRecord(snapshot.meeting);
      setSelectedId(prev => prev ?? snapshot.meeting.id);
      setActiveControlMode(prev => {
        const nextMode = controlModeForPhase(snapshot.phase);
        if (nextMode) return { meetingId: snapshot.meeting.id, mode: nextMode };
        return prev?.meetingId === snapshot.meeting.id ? prev : null;
      });
    } catch (error) {
      console.warn('[meetings] active snapshot refresh failed', error);
    }
  }, [cacheMeetingRecord]);

  const refresh = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const [active, records] = await Promise.all([
        getActiveMeetingRecording(),
        listMeetings(),
      ]);
      const nextRecords = active
        ? upsertMeetingListItem(records, meetingListItemFromRecord(active.meeting))
        : records;
      setActiveSnapshot(active);
      setActiveControlMode(active ? controlModeForSnapshot(active) : null);
      setMeetings(nextRecords);
      if (active) {
        setMeetingDetails(prev => ({ ...prev, [active.meeting.id]: active.meeting }));
      }
      setSelectedId(prev => {
        const requested = requestedMeetingIdRef.current;
        if (requested && nextRecords.some(record => record.id === requested)) return requested;
        if (active) return active.meeting.id;
        if (prev && nextRecords.some(record => record.id === prev)) return prev;
        return nextRecords[0]?.id ?? null;
      });
      setActionError(null);
    } catch (error) {
      console.error('[meetings] failed to load meetings', error);
      setLoadError(errorMessage(error));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let cancelled = false;
    let unlistenState: (() => void) | undefined;
    let unlistenDraft: (() => void) | undefined;
    let unlistenSegment: (() => void) | undefined;
    let unlistenError: (() => void) | undefined;
    let unlistenSummary: (() => void) | undefined;
    let unlistenClose: (() => void) | undefined;

    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const stateHandle = await listen<MeetingRecordingSnapshot>('meeting:state', event => {
          if (cancelled) return;
          const snapshot = event.payload;
          const finalSnapshot = snapshot.phase === 'stopping' || snapshot.meeting.endedAt != null;
          setActiveSnapshot(finalSnapshot ? null : snapshot);
          setActiveControlMode(prev => {
            if (finalSnapshot) return null;
            return controlModeForSnapshot(snapshot) ?? (prev?.meetingId === snapshot.meeting.id ? prev : null);
          });
          cacheMeetingRecord(snapshot.meeting);
          setSelectedId(prev => prev ?? snapshot.meeting.id);
          if (finalSnapshot) {
            setDraftByMeetingId(prev => removeDraft(prev, snapshot.meeting.id));
          }
        });
        const draftHandle = await listen<MeetingTranscriptDraftEvent>('meeting:transcript-draft', event => {
          if (cancelled) return;
          const payload = event.payload;
          const snapshot = activeSnapshotRef.current;
          const staleForActiveSession =
            snapshot?.meeting.id === payload.meetingId &&
            !!snapshot.activeProviderSessionId &&
            payload.providerSessionId !== snapshot.activeProviderSessionId;
          if (staleForActiveSession) return;
          if (payload.clear) {
            setDraftByMeetingId(prev => removeDraft(prev, payload.meetingId));
            return;
          }
          if (!snapshot || snapshot.meeting.id !== payload.meetingId) return;
          if (snapshot.activeProviderSessionId && payload.providerSessionId !== snapshot.activeProviderSessionId) return;
          setDraftByMeetingId(prev => ({
            ...prev,
            [payload.meetingId]: payload,
          }));
        });
        const segmentHandle = await listen<MeetingTranscriptSegmentEvent>('meeting:transcript-segment', event => {
          if (cancelled) return;
          const payload = event.payload;
          const knownMeeting = meetingsRef.current.some(record => record.id === payload.meetingId);
          const snapshot = activeSnapshotRef.current;
          const segmentProviderSessionId = payload.segment.metadata?.providerSessionId ?? null;
          const staleForActiveSession =
            snapshot?.meeting.id === payload.meetingId &&
            !!snapshot.activeProviderSessionId &&
            segmentProviderSessionId !== snapshot.activeProviderSessionId;
          if (!staleForActiveSession) {
            setDraftByMeetingId(prev => removeDraft(prev, payload.meetingId));
          }
          const knownRecord = meetingDetailsRef.current[payload.meetingId]
            ?? (snapshot?.meeting.id === payload.meetingId ? snapshot.meeting : null);
          if (!knownRecord || !hasSegment(knownRecord, payload.segment.id)) {
            setMeetings(prev => appendSegmentToMeetingList(prev, payload.meetingId, payload.segment));
          }
          setMeetingDetails(prev => {
            const record = prev[payload.meetingId];
            if (!record || hasSegment(record, payload.segment.id)) return prev;
            return {
              ...prev,
              [payload.meetingId]: {
                ...record,
                transcriptSegments: [...record.transcriptSegments, payload.segment],
              },
            };
          });
          setActiveSnapshot(prev => {
            if (!prev || prev.meeting.id !== payload.meetingId) return prev;
            if (hasSegment(prev.meeting, payload.segment.id)) return prev;
            return {
              ...prev,
              meeting: {
                ...prev.meeting,
                transcriptSegments: [...prev.meeting.transcriptSegments, payload.segment],
              },
            };
          });
          if (!knownMeeting) void syncActiveSnapshot(payload.meetingId);
        });
        const errorHandle = await listen<MeetingErrorEvent>('meeting:error', event => {
          if (cancelled) return;
          setEventError(event.payload);
        });
        const summaryHandle = await listen<MeetingSummaryEvent>('meeting:summary', event => {
          if (cancelled) return;
          const payload = event.payload;
          if (payload.meeting) {
            cacheMeetingRecord(payload.meeting);
            setSelectedId(prev => prev ?? payload.meeting!.id);
          }
          if (payload.error) setEventError(payload.error);
        });
        const closeHandle = await listen<MeetingCloseRequestEvent | MeetingRecordingSnapshot>('meeting:close-requested', event => {
          if (cancelled) return;
          const { snapshot } = normalizeMeetingCloseRequest(event.payload);
          setActiveSnapshot(snapshot);
          setActiveControlMode(controlModeForSnapshot(snapshot));
          cacheMeetingRecord(snapshot.meeting);
          setSelectedId(snapshot.meeting.id);
          setActionError(t('meetings.closeGuard.message'));
          if (mobile) setMobileDetailOpen(true);
        });

        if (cancelled) {
          stateHandle();
          draftHandle();
          segmentHandle();
          errorHandle();
          summaryHandle();
          closeHandle();
        } else {
          unlistenState = stateHandle;
          unlistenDraft = draftHandle;
          unlistenSegment = segmentHandle;
          unlistenError = errorHandle;
          unlistenSummary = summaryHandle;
          unlistenClose = closeHandle;
        }
      } catch (error) {
        console.warn('[meetings] event listener setup failed', error);
      }
    })();

    return () => {
      cancelled = true;
      unlistenState?.();
      unlistenDraft?.();
      unlistenSegment?.();
      unlistenError?.();
      unlistenSummary?.();
      unlistenClose?.();
    };
  }, [cacheMeetingRecord, mobile, syncActiveSnapshot, t]);

  const filteredMeetings = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return meetings;
    return meetings.filter(record => meetingSearchText(record).includes(q));
  }, [meetings, query]);

  const selectedMeeting = useMemo(() => {
    const visibleSelected = filteredMeetings.find(record => record.id === selectedId);
    return visibleSelected ?? filteredMeetings[0] ?? null;
  }, [filteredMeetings, selectedId]);

  const cachedDetail = selectedMeeting ? meetingDetails[selectedMeeting.id] : null;
  const detailMeeting = activeSnapshot && selectedMeeting?.id === activeSnapshot.meeting.id
    ? activeSnapshot.meeting
    : selectedMeeting && cachedDetail?.updatedAt === selectedMeeting.updatedAt
      ? cachedDetail
      : null;
  const selectedActiveSnapshot = activeSnapshot && detailMeeting?.id === activeSnapshot.meeting.id
    ? activeSnapshot
    : null;
  const selectedControlMode = activeControlMode && detailMeeting?.id === activeControlMode.meetingId
    ? activeControlMode.mode
    : null;
  const selectedDraft = detailMeeting ? draftByMeetingId[detailMeeting.id] ?? null : null;
  const transcriptCount = detailMeeting?.transcriptSegments.length ?? 0;

  useEffect(() => {
    const meetingId = selectedMeeting?.id;
    if (!meetingId || activeSnapshot?.meeting.id === meetingId) {
      detailRequestRef.current += 1;
      setDetailLoading(false);
      setDetailError(null);
      return;
    }
    const cached = meetingDetailsRef.current[meetingId];
    if (cached?.updatedAt === selectedMeeting.updatedAt) {
      setDetailLoading(false);
      setDetailError(null);
      return;
    }

    const requestId = ++detailRequestRef.current;
    setDetailLoading(true);
    setDetailError(null);
    void getMeeting(meetingId)
      .then(record => {
        if (detailRequestRef.current !== requestId) return;
        cacheMeetingRecord(record);
      })
      .catch(error => {
        if (detailRequestRef.current !== requestId) return;
        console.error('[meetings] failed to load meeting detail', error);
        setDetailError(t('meetings.detailLoadFailed', { err: errorMessage(error) }));
      })
      .finally(() => {
        if (detailRequestRef.current === requestId) setDetailLoading(false);
      });
  }, [activeSnapshot?.meeting.id, cacheMeetingRecord, detailRetryNonce, selectedMeeting?.id, selectedMeeting?.updatedAt, t]);

  const markMeetingAudioMissing = useCallback((meetingId: string) => {
    setMeetings(prev => prev.map(item => (
      item.id === meetingId
        ? { ...item, audio: missingMeetingAudio() }
        : item
    )));
    setMeetingDetails(prev => {
      const record = prev[meetingId];
      return record
        ? { ...prev, [meetingId]: withMissingAudio(record) }
        : prev;
    });
  }, []);

  useEffect(() => {
    if (!transcriptStickToBottomRef.current) return;
    const el = transcriptScrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [detailMeeting?.id, selectedDraft?.text, transcriptCount]);

  const selectMeeting = (id: string) => {
    setSelectedId(id);
    setActionError(null);
    setDetailError(null);
    setEditDraft(null);
    setDeleteConfirmId(null);
    setRewriteConfirmId(null);
    if (mobile) setMobileDetailOpen(true);
  };

  const runStart = async () => {
    setActionLoading('start');
    setActionError(null);
    setEventError(null);
    try {
      const snapshot = await startMeetingRecording();
      setActiveSnapshot(snapshot);
      setActiveControlMode({ meetingId: snapshot.meeting.id, mode: 'recording' });
      cacheMeetingRecord(snapshot.meeting);
      setSelectedId(snapshot.meeting.id);
      if (mobile) setMobileDetailOpen(true);
    } catch (error) {
      console.error('[meetings] start failed', error);
      setActionError(t('meetings.actionFailed', { err: errorMessage(error) }));
    } finally {
      setActionLoading(null);
    }
  };

  const runShowCompanion = async () => {
    setCompanionLoading(true);
    setActionError(null);
    try {
      await showMeetingCompanion();
    } catch (error) {
      console.error('[meetings] show companion failed', error);
      setActionError(t('meetings.actionFailed', { err: errorMessage(error) }));
    } finally {
      setCompanionLoading(false);
    }
  };

  const runSaveEdit = async (record: MeetingRecord) => {
    if (!editDraft || editDraft.id !== record.id || !canEditMeeting(record, selectedActiveSnapshot)) return;
    setActionLoading('save');
    setActionError(null);
    try {
      const updated = await updateMeetingRecord({
        ...record,
        title: editDraft.title.trim() || t('meetings.untitled'),
        summary: {
          overview: editDraft.overview.trim(),
          keyDecisions: linesFromDraft(editDraft.keyDecisions),
          todos: todosFromDraft(editDraft.todos, record),
          risksAndOpenQuestions: linesFromDraft(editDraft.risksAndOpenQuestions),
        },
      });
      cacheMeetingRecord(updated);
      setSelectedId(updated.id);
      setEditDraft(null);
      setActionError(null);
    } catch (error) {
      console.error('[meetings] save edit failed', error);
      setActionError(t('meetings.edit.saveFailed', { err: errorMessage(error) }));
    } finally {
      setActionLoading(null);
    }
  };

  const runDelete = async (record: MeetingRecord) => {
    if (!canDeleteMeeting(record, activeSnapshot)) return;
    if (deleteConfirmId !== record.id) {
      setDeleteConfirmId(record.id);
      setActionError(null);
      return;
    }
    setActionLoading('delete');
    setActionError(null);
    try {
      await deleteMeetingRecord(record.id);
      const remaining = meetingsRef.current.filter(item => item.id !== record.id);
      setMeetings(remaining);
      setMeetingDetails(prev => {
        const next = { ...prev };
        delete next[record.id];
        return next;
      });
      setEditDraft(null);
      setDeleteConfirmId(null);
      setEventError(prev => (prev?.meetingId === record.id ? null : prev));
      setSelectedId(current => {
        if (current !== record.id) return current;
        return remaining[0]?.id ?? null;
      });
      if (mobile && remaining.length === 0) setMobileDetailOpen(false);
    } catch (error) {
      console.error('[meetings] delete failed', error);
      setActionError(t('meetings.deleteFailed', { err: errorMessage(error) }));
    } finally {
      setActionLoading(null);
    }
  };

  const runExportMarkdown = async (record: MeetingRecord) => {
    setActionLoading('export');
    setActionError(null);
    try {
      const targetPath = await chooseMarkdownExportPath(record);
      if (!targetPath) return;
      await exportMeetingMarkdown(record.id, targetPath);
      setActionError(t('meetings.exportSuccess', { path: targetPath }));
    } catch (error) {
      console.error('[meetings] export failed', error);
      setActionError(t('meetings.exportFailed', { err: errorMessage(error) }));
    } finally {
      setActionLoading(null);
    }
  };

  const runRetranscribe = async (record: MeetingRecord) => {
    if (!canRetranscribeMeeting(record, activeSnapshot)) return;
    setActionLoading('retranscribe');
    setActionError(null);
    setEventError(null);
    try {
      const updated = await retranscribeMeeting(record.id);
      cacheMeetingRecord(updated);
      setSelectedId(updated.id);
      setActionError(t('meetings.retranscribeSuccess'));
    } catch (error) {
      console.error('[meetings] retranscribe failed', error);
      setActionError(t('meetings.retranscribeFailed', { err: errorMessage(error) }));
    } finally {
      setActionLoading(null);
    }
  };

  const runPause = async (id: string) => {
    setActionLoading('pause');
    setActionError(null);
    try {
      const snapshot = await pauseMeetingRecording(id);
      setActiveSnapshot(snapshot.meeting.endedAt == null ? snapshot : null);
      setActiveControlMode(snapshot.meeting.endedAt == null ? { meetingId: snapshot.meeting.id, mode: 'paused' } : null);
      cacheMeetingRecord(snapshot.meeting);
      setDraftByMeetingId(prev => removeDraft(prev, id));
    } catch (error) {
      console.error('[meetings] pause failed', error);
      setActionError(t('meetings.actionFailed', { err: errorMessage(error) }));
    } finally {
      setActionLoading(null);
    }
  };

  const runResume = async (id: string) => {
    setActionLoading('resume');
    setActionError(null);
    try {
      const snapshot = await resumeMeetingRecording(id);
      setActiveSnapshot(snapshot.meeting.endedAt == null ? snapshot : null);
      setActiveControlMode(snapshot.meeting.endedAt == null ? { meetingId: snapshot.meeting.id, mode: 'recording' } : null);
      cacheMeetingRecord(snapshot.meeting);
    } catch (error) {
      console.error('[meetings] resume failed', error);
      setActionError(t('meetings.actionFailed', { err: errorMessage(error) }));
    } finally {
      setActionLoading(null);
    }
  };

  const runStop = async (id: string) => {
    setActionLoading('stop');
    setActionError(null);
    try {
      const record = await stopMeetingRecording(id);
      setActiveSnapshot(null);
      setActiveControlMode(null);
      cacheMeetingRecord(record);
      setDraftByMeetingId(prev => removeDraft(prev, id));
      setSelectedId(record.id);
      if (mobile) setMobileDetailOpen(true);
      const fresh = await listMeetings();
      setMeetings(fresh);
    } catch (error) {
      console.error('[meetings] stop failed', error);
      setActionError(t('meetings.actionFailed', { err: errorMessage(error) }));
    } finally {
      setActionLoading(null);
    }
  };

  const runRetrySummary = async (id: string) => {
    setActionLoading('summary');
    setActionError(null);
    setEventError(null);
    try {
      const record = await retryMeetingSummary(id);
      cacheMeetingRecord(record);
      setSelectedId(record.id);
    } catch (error) {
      console.error('[meetings] retry summary failed', error);
      setActionError(t('meetings.actionFailed', { err: errorMessage(error) }));
    } finally {
      setActionLoading(null);
    }
  };

  const runGenerateSummary = async (id: string) => {
    if (rewriteConfirmId !== id) {
      setRewriteConfirmId(id);
      setActionError(null);
      return;
    }
    setActionLoading('summary');
    setActionError(null);
    setEventError(null);
    try {
      const record = await generateMeetingSummary(id);
      cacheMeetingRecord(record);
      setSelectedId(record.id);
      setRewriteConfirmId(null);
    } catch (error) {
      console.error('[meetings] generate summary failed', error);
      setActionError(t('meetings.actionFailed', { err: errorMessage(error) }));
    } finally {
      setActionLoading(null);
    }
  };

  const activePill = activeSnapshot ? (
    <Pill tone={statusTone(activeSnapshot.meeting.status)} size="sm">
      {phaseLabel(activeSnapshot.phase, t)}
    </Pill>
  ) : null;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}>
      <PageHeader
        kicker={t('meetings.kicker')}
        title={t('meetings.title')}
        desc={t('meetings.desc')}
        titleRight={activePill}
        right={
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', justifyContent: 'flex-end' }}>
            {isDesktop() && activeSnapshot && prefs?.meetingCompanionEnabled && (
              <Btn icon="sparkle" variant="ghost" size="sm" onClick={() => void runShowCompanion()} disabled={companionLoading}>
                {t('meetings.actions.showCompanion')}
              </Btn>
            )}
            <Btn icon="refresh" variant="ghost" size="sm" onClick={() => void refresh()} disabled={loading}>
              {t('common.refresh')}
            </Btn>
            <Btn icon="mic" variant="blue" size="sm" onClick={() => void runStart()} disabled={Boolean(activeSnapshot) || actionLoading !== null}>
              {actionLoading === 'start' ? t('meetings.actions.starting') : t('meetings.actions.start')}
            </Btn>
          </div>
        }
      />

      <div style={{ display: 'grid', gridTemplateColumns: mobile ? '1fr' : '320px 1fr', gap: 14, flex: 1, minHeight: 0 }}>
        {(!mobile || !mobileDetailOpen) && (
          <Card padding={0} style={{ display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
            <MeetingListHeader
              query={query}
              total={meetings.length}
              shown={filteredMeetings.length}
              onQueryChange={setQuery}
            />
            <div className="ol-thinscroll" style={{ flex: 1, minHeight: 0, overflow: 'auto', padding: 6 }}>
              {actionError && (
                <ErrorBanner tone="error">{actionError}</ErrorBanner>
              )}
              {loading && (
                <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)' }}>
                  {t('common.loading')}
                </div>
              )}
              {!loading && loadError && (
                <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)', display: 'flex', flexDirection: 'column', gap: 10, alignItems: 'flex-start' }}>
                  <span>{t('meetings.loadFailed', { err: loadError })}</span>
                  <Btn size="sm" variant="ghost" onClick={() => void refresh()}>{t('common.retry')}</Btn>
                </div>
              )}
              {!loading && !loadError && filteredMeetings.length === 0 && (
                <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)', lineHeight: 1.5 }}>
                  {query.trim()
                    ? t('meetings.searchNoMatch', { query: query.trim() })
                    : t('meetings.empty')}
                </div>
              )}
              {!loadError && filteredMeetings.map(record => (
                <MeetingListItem
                  key={record.id}
                  record={record}
                  selected={selectedMeeting?.id === record.id}
                  active={activeSnapshot?.meeting.id === record.id}
                  onSelect={() => void selectMeeting(record.id)}
                />
              ))}
            </div>
          </Card>
        )}

        {(!mobile || mobileDetailOpen) && (
          <Card padding={20} style={{ display: 'flex', flexDirection: 'column', minHeight: 0, overflow: 'hidden' }}>
            {detailMeeting ? (
              <>
                {mobile && (
                  <div style={{ marginBottom: 12, flexShrink: 0 }}>
                    <Btn icon="chevLeft" variant="ghost" size="sm" onClick={() => setMobileDetailOpen(false)}>
                      {t('meetings.backToList')}
                    </Btn>
                  </div>
                )}
                <MeetingDetailHeader
                  record={detailMeeting}
                  snapshot={selectedActiveSnapshot}
                  draft={editDraft?.id === detailMeeting.id ? editDraft : null}
                  onDraftChange={setEditDraft}
                  controlMode={selectedControlMode}
                  actionLoading={actionLoading}
                  editing={editDraft?.id === detailMeeting.id}
                  deleteConfirming={deleteConfirmId === detailMeeting.id}
                  canEdit={canEditMeeting(detailMeeting, selectedActiveSnapshot)}
                  canDelete={canDeleteMeeting(detailMeeting, activeSnapshot)}
                  onEdit={() => setEditDraft(createEditDraft(detailMeeting))}
                  onCancelEdit={() => setEditDraft(null)}
                  onSaveEdit={() => void runSaveEdit(detailMeeting)}
                  onDelete={() => void runDelete(detailMeeting)}
                  onCancelDelete={() => setDeleteConfirmId(null)}
                  onExport={() => void runExportMarkdown(detailMeeting)}
                  onPause={() => void runPause(detailMeeting.id)}
                  onResume={() => void runResume(detailMeeting.id)}
                  onStop={() => void runStop(detailMeeting.id)}
                />
                <div className="ol-thinscroll" style={{ flex: 1, minHeight: 0, overflow: 'auto', paddingRight: 2 }}>
                  {actionError && (
                    <ErrorBanner tone="error">{actionError}</ErrorBanner>
                  )}
                  {eventError && (!eventError.meetingId || eventError.meetingId === detailMeeting.id) && (
                    <ErrorBanner tone="error">
                      {t('meetings.eventError', { message: eventError.message })}
                    </ErrorBanner>
                  )}
                  {deleteConfirmId === detailMeeting.id && (
                    <ErrorBanner tone="warning">
                      {t('meetings.deleteConfirm', { title: detailMeeting.title || t('meetings.untitled') })}
                    </ErrorBanner>
                  )}
                  {rewriteConfirmId === detailMeeting.id && (
                    <ErrorBanner tone="warning">
                      {t('meetings.rewriteConfirm')}
                    </ErrorBanner>
                  )}
                  {(detailMeeting.status === 'transcribing_interrupted' || selectedActiveSnapshot?.asrInterrupted) && (
                    <ErrorBanner tone="warning">
                      {selectedActiveSnapshot?.asrInterrupted
                        ? t('meetings.asrInterrupted')
                        : t('meetings.interruptedRecordingEnded')}
                    </ErrorBanner>
                  )}
                  {detailMeeting.audio.state === 'missing' && (
                    <ErrorBanner tone="warning">
                      {t('meetings.audioPlayback.missing')}
                    </ErrorBanner>
                  )}
                  {canPlayMeetingAudio(detailMeeting, activeSnapshot) && (
                    <MeetingAudioPlayer
                      meetingId={detailMeeting.id}
                      onMissing={() => markMeetingAudioMissing(detailMeeting.id)}
                    />
                  )}
                  <SummarySection
                    record={detailMeeting}
                    draft={editDraft?.id === detailMeeting.id ? editDraft : null}
                    onDraftChange={setEditDraft}
                    actionLoading={actionLoading}
                    onRetry={() => void runRetrySummary(detailMeeting.id)}
                    onRewrite={() => void runGenerateSummary(detailMeeting.id)}
                    rewriteConfirming={rewriteConfirmId === detailMeeting.id}
                    onCancelRewrite={() => setRewriteConfirmId(null)}
                  />
                  <TranscriptList
                    record={detailMeeting}
                    draft={selectedDraft}
                    scrollRef={transcriptScrollRef}
                    onScroll={() => {
                      const el = transcriptScrollRef.current;
                      if (!el) return;
                      transcriptStickToBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
                    }}
                    actionLoading={actionLoading}
                    canRetranscribe={editDraft?.id !== detailMeeting.id && canRetranscribeMeeting(detailMeeting, activeSnapshot)}
                    onRetranscribe={() => void runRetranscribe(detailMeeting)}
                  />
                </div>
              </>
            ) : (
              <div style={{ padding: 40, textAlign: 'center', fontSize: 13, color: 'var(--ol-ink-4)' }}>
                {mobile && selectedMeeting && (
                  <div style={{ marginBottom: 12 }}>
                    <Btn icon="chevLeft" variant="ghost" size="sm" onClick={() => setMobileDetailOpen(false)}>
                      {t('meetings.backToList')}
                    </Btn>
                  </div>
                )}
                {detailLoading || loading ? t('common.loading') : null}
                {!detailLoading && detailError && (
                  <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 10 }}>
                    <span>{detailError}</span>
                    <Btn size="sm" variant="ghost" onClick={() => setDetailRetryNonce(value => value + 1)}>
                      {t('common.retry')}
                    </Btn>
                  </div>
                )}
                {!detailLoading && !detailError && !loading && (
                  loadError ? t('meetings.loadFailed', { err: loadError }) : t('meetings.selectHint')
                )}
              </div>
            )}
          </Card>
        )}
      </div>
    </div>
  );
}

function MeetingListHeader({
  query,
  total,
  shown,
  onQueryChange,
}: {
  query: string;
  total: number;
  shown: number;
  onQueryChange: (value: string) => void;
}) {
  const { t } = useTranslation();
  return (
    <div style={{ padding: '12px 14px', borderBottom: '0.5px solid var(--ol-line)' }}>
      <div style={{
        display: 'flex',
        alignItems: 'center',
        gap: 6,
        padding: '6px 10px',
        fontSize: 12,
        border: '0.5px solid var(--ol-line-strong)',
        borderRadius: 8,
        background: 'var(--ol-surface-2)',
        color: 'var(--ol-ink-3)',
      }}>
        <Icon name="search" size={12} />
        <input
          type="search"
          value={query}
          onChange={event => onQueryChange(event.target.value)}
          placeholder={t('meetings.searchPlaceholder')}
          aria-label={t('meetings.searchPlaceholder')}
          style={{
            flex: 1,
            minWidth: 0,
            outline: 'none',
            border: 0,
            background: 'transparent',
            fontSize: 12,
            color: 'var(--ol-ink-1)',
            fontFamily: 'inherit',
          }}
        />
      </div>
      <div style={{ marginTop: 8, fontSize: 11, color: 'var(--ol-ink-4)' }}>
        {t('meetings.summary', { total, shown })}
      </div>
    </div>
  );
}

function MeetingListItem({
  record,
  selected,
  active,
  onSelect,
}: {
  record: MeetingListItem;
  selected: boolean;
  active: boolean;
  onSelect: () => void;
}) {
  const { t } = useTranslation();
  const preview = record.transcriptPreview;
  return (
    <button
      type="button"
      onClick={onSelect}
      style={{
        width: '100%',
        padding: '10px 12px',
        textAlign: 'left',
        display: 'flex',
        flexDirection: 'column',
        gap: 6,
        border: 0,
        borderRadius: 8,
        background: selected ? 'rgba(37,99,235,0.06)' : 'transparent',
        boxShadow: selected ? 'inset 2px 0 0 var(--ol-blue)' : 'none',
        cursor: 'default',
        fontFamily: 'inherit',
        marginBottom: 1,
        transition: 'background 0.16s var(--ol-motion-quick), box-shadow 0.18s var(--ol-motion-soft)',
      }}
    >
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8 }}>
        <span style={{ fontSize: 11, fontFamily: 'var(--ol-font-mono)', color: 'var(--ol-ink-3)' }}>
          {formatDateTime(record.startedAt)}
        </span>
        <span style={{ fontSize: 10, color: 'var(--ol-ink-4)', fontFamily: 'var(--ol-font-mono)' }}>
          {formatDuration(record.durationMs, t)}
        </span>
      </div>
      <div style={{ fontSize: 12.5, fontWeight: 600, color: 'var(--ol-ink)', lineHeight: 1.35, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
        {record.title || t('meetings.untitled')}
      </div>
      <div style={{ fontSize: 12, color: preview ? 'var(--ol-ink-3)' : 'var(--ol-ink-4)', lineHeight: 1.45, display: '-webkit-box', WebkitLineClamp: 2, WebkitBoxOrient: 'vertical', overflow: 'hidden' }}>
        {preview || t('meetings.noTranscriptPreview')}
      </div>
      <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
        <Pill size="sm" tone={statusTone(record.status)}>{statusLabel(record.status, t)}</Pill>
        <Pill size="sm" tone="outline">{audioLabel(record.audio.state, t)}</Pill>
        {active && <Pill size="sm" tone="blue">{t('meetings.activeBadge')}</Pill>}
      </div>
    </button>
  );
}

function MeetingDetailHeader({
  record,
  snapshot,
  draft,
  onDraftChange,
  controlMode,
  actionLoading,
  editing,
  deleteConfirming,
  canEdit,
  canDelete,
  onEdit,
  onCancelEdit,
  onSaveEdit,
  onDelete,
  onCancelDelete,
  onExport,
  onPause,
  onResume,
  onStop,
}: {
  record: MeetingRecord;
  snapshot: MeetingRecordingSnapshot | null;
  draft: MeetingEditDraft | null;
  onDraftChange: (draft: MeetingEditDraft) => void;
  controlMode: ActiveControlMode | null;
  actionLoading: ActionLoading;
  editing: boolean;
  deleteConfirming: boolean;
  canEdit: boolean;
  canDelete: boolean;
  onEdit: () => void;
  onCancelEdit: () => void;
  onSaveEdit: () => void;
  onDelete: () => void;
  onCancelDelete: () => void;
  onExport: () => void;
  onPause: () => void;
  onResume: () => void;
  onStop: () => void;
}) {
  const { t } = useTranslation();
  const active = snapshot != null;
  const phase = snapshot?.phase ?? null;
  const showPause = active && (phase === 'recording' || (phase === 'transcribing_interrupted' && controlMode === 'recording'));
  const showResume = active && (phase === 'paused' || (phase === 'transcribing_interrupted' && controlMode === 'paused'));
  const showStop = active;
  return (
    <div style={{ flexShrink: 0, borderBottom: '0.5px solid var(--ol-line-soft)', paddingBottom: 14, marginBottom: 14 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', gap: 12, flexWrap: 'wrap' }}>
        <div style={{ minWidth: 0, flex: 1 }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap', marginBottom: 8 }}>
            {draft ? (
              <input
                value={draft.title}
                onChange={event => onDraftChange({ ...draft, title: event.target.value })}
                aria-label={t('meetings.edit.titleLabel')}
                placeholder={t('meetings.untitled')}
                style={{ ...editorInputStyle, maxWidth: 420, fontSize: 16, fontWeight: 600 }}
              />
            ) : (
              <h2 style={{ margin: 0, fontSize: 18, fontWeight: 600, color: 'var(--ol-ink)', lineHeight: 1.25 }}>
                {record.title || t('meetings.untitled')}
              </h2>
            )}
            <Pill size="sm" tone={statusTone(record.status)}>{statusLabel(record.status, t)}</Pill>
            <Pill size="sm" tone="outline">{audioLabel(record.audio.state, t)}</Pill>
          </div>
          <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', fontSize: 11, color: 'var(--ol-ink-4)' }}>
            <span>{t('meetings.startedAt')}: {formatDateTime(record.startedAt)}</span>
            <span>{active ? t('meetings.elapsed') : t('meetings.duration')}: {formatDuration(active ? snapshot.elapsedMs : record.durationMs, t)}</span>
          </div>
        </div>
        {(showPause || showResume || showStop || canEdit || canDelete || editing) && (
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', justifyContent: 'flex-end' }}>
            {editing ? (
              <>
                <Btn icon="check" variant="blue" size="sm" disabled={actionLoading !== null} onClick={onSaveEdit}>
                  {actionLoading === 'save' ? t('meetings.edit.saving') : t('meetings.edit.save')}
                </Btn>
                <Btn icon="x" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onCancelEdit}>
                  {t('common.cancel')}
                </Btn>
              </>
            ) : canEdit && (
              <Btn icon="doc" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onEdit}>
                {t('meetings.edit.edit')}
              </Btn>
            )}
            {!editing && (
              <Btn icon="download" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onExport}>
                {actionLoading === 'export' ? t('meetings.actions.exporting') : t('meetings.actions.exportMarkdown')}
              </Btn>
            )}
            {showPause && (
              <Btn icon="mic" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onPause}>
                {actionLoading === 'pause' ? t('meetings.actions.pausing') : t('meetings.actions.pause')}
              </Btn>
            )}
            {showResume && (
              <Btn icon="play" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onResume}>
                {actionLoading === 'resume' ? t('meetings.actions.resuming') : t('meetings.actions.resume')}
              </Btn>
            )}
            {showStop && (
              <Btn icon="check" variant="blue" size="sm" disabled={actionLoading !== null} onClick={onStop}>
                {actionLoading === 'stop' ? t('meetings.actions.stopping') : t('meetings.actions.stop')}
              </Btn>
            )}
            {!editing && canDelete && deleteConfirming && (
              <>
                <Btn icon="trash" variant="blue" size="sm" disabled={actionLoading !== null} onClick={onDelete}>
                  {actionLoading === 'delete' ? t('meetings.actions.deleting') : t('meetings.actions.deleteConfirm')}
                </Btn>
                <Btn icon="x" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onCancelDelete}>
                  {t('common.cancel')}
                </Btn>
              </>
            )}
            {!editing && canDelete && !deleteConfirming && (
              <Btn icon="trash" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onDelete}>
                {t('common.delete')}
              </Btn>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function TranscriptList({
  record,
  draft,
  scrollRef,
  onScroll,
  actionLoading,
  canRetranscribe,
  onRetranscribe,
}: {
  record: MeetingRecord;
  draft: MeetingTranscriptDraftEvent | null;
  scrollRef: MutableRefObject<HTMLDivElement | null>;
  onScroll: () => void;
  actionLoading: ActionLoading;
  canRetranscribe: boolean;
  onRetranscribe: () => void;
}) {
  const { t } = useTranslation();
  const hasTranscriptRows = record.transcriptSegments.length > 0;
  const rowCount = record.transcriptSegments.length + (draft ? 1 : 0);
  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 92,
    getItemKey: index => record.transcriptSegments[index]?.id ?? 'draft',
    overscan: 6,
  });
  const virtualRows = virtualizer.getVirtualItems();
  return (
    <div style={{ display: 'flex', flexDirection: 'column', minHeight: 260 }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10, marginBottom: 10, flexShrink: 0 }}>
        <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--ol-ink-2)' }}>
          {t('meetings.transcriptTitle')}
        </span>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap', justifyContent: 'flex-end' }}>
          <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>
            {t('meetings.segmentCount', { count: record.transcriptSegments.length })}
          </span>
          {canRetranscribe && (
            <Btn icon="refresh" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onRetranscribe}>
              {actionLoading === 'retranscribe' ? t('meetings.actions.retranscribing') : t('meetings.actions.retranscribe')}
            </Btn>
          )}
        </div>
      </div>
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="ol-thinscroll"
        style={{ maxHeight: 'min(46vh, 520px)', overflow: 'auto', paddingRight: 2 }}
      >
        {!hasTranscriptRows && !draft ? (
          <div style={{ padding: 18, border: '0.5px solid var(--ol-line)', borderRadius: 10, background: 'var(--ol-surface-2)', color: 'var(--ol-ink-4)', fontSize: 12.5, lineHeight: 1.55 }}>
            {t('meetings.noTranscript')}
          </div>
        ) : (
          <div style={{ height: virtualizer.getTotalSize(), position: 'relative', width: '100%' }}>
            {virtualRows.map(virtualRow => {
              const segment = record.transcriptSegments[virtualRow.index];
              return (
                <div
                  key={virtualRow.key}
                  data-index={virtualRow.index}
                  ref={virtualizer.measureElement}
                  style={{
                    position: 'absolute',
                    top: 0,
                    left: 0,
                    width: '100%',
                    transform: `translateY(${virtualRow.start}px)`,
                    paddingBottom: 8,
                  }}
                >
                  {segment ? <TranscriptRow segment={segment} /> : draft ? <TranscriptDraftRow draft={draft} /> : null}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function SummarySection({
  record,
  draft,
  onDraftChange,
  actionLoading,
  onRetry,
  onRewrite,
  rewriteConfirming,
  onCancelRewrite,
}: {
  record: MeetingRecord;
  draft: MeetingEditDraft | null;
  onDraftChange: (draft: MeetingEditDraft) => void;
  actionLoading: ActionLoading;
  onRetry: () => void;
  onRewrite: () => void;
  rewriteConfirming: boolean;
  onCancelRewrite: () => void;
}) {
  const { t } = useTranslation();
  const summary = record.summary;
  const hasSummary = Boolean(
    summary.overview.trim()
    || summary.keyDecisions.length
    || summary.todos.length
    || summary.risksAndOpenQuestions.length,
  );
  const canRewrite = !draft && record.status === 'completed' && hasSummary;
  return (
    <div style={{
      marginBottom: 14,
      padding: 12,
      border: '0.5px solid var(--ol-line)',
      borderRadius: 8,
      background: 'var(--ol-surface-2)',
    }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10, marginBottom: 10 }}>
        <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--ol-ink-2)' }}>
          {t('meetings.summaryTitle')}
        </span>
        {!draft && record.status === 'summary_failed' && (
          <Btn icon="refresh" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onRetry}>
            {actionLoading === 'summary' ? t('meetings.actions.summaryRetrying') : t('meetings.actions.summaryRetry')}
          </Btn>
        )}
        {canRewrite && (
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', justifyContent: 'flex-end' }}>
            <Btn icon="refresh" variant={rewriteConfirming ? 'blue' : 'ghost'} size="sm" disabled={actionLoading !== null} onClick={onRewrite}>
              {actionLoading === 'summary'
                ? t('meetings.actions.summaryRewriting')
                : rewriteConfirming
                  ? t('meetings.actions.summaryRewriteConfirm')
                  : t('meetings.actions.summaryRewrite')}
            </Btn>
            {rewriteConfirming && (
              <Btn icon="x" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onCancelRewrite}>
                {t('common.cancel')}
              </Btn>
            )}
          </div>
        )}
      </div>
      {draft ? (
        <SummaryEditor draft={draft} onDraftChange={onDraftChange} />
      ) : record.status === 'summarizing' ? (
        <div style={{ fontSize: 12.5, color: 'var(--ol-ink-4)', lineHeight: 1.55 }}>
          {t('meetings.summaryLoading')}
        </div>
      ) : record.status === 'summary_failed' ? (
        <div style={{ fontSize: 12.5, color: 'var(--ol-red, #ef4444)', lineHeight: 1.55 }}>
          {t('meetings.summaryFailed')}
        </div>
      ) : hasSummary ? (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
          {summary.overview.trim() && (
            <div style={{ fontSize: 13, color: 'var(--ol-ink)', lineHeight: 1.65, whiteSpace: 'pre-wrap' }}>
              {summary.overview}
            </div>
          )}
          <SummaryList title={t('meetings.keyDecisions')} items={summary.keyDecisions} />
          <TodoList todos={summary.todos} />
          <SummaryList title={t('meetings.risksAndOpenQuestions')} items={summary.risksAndOpenQuestions} />
        </div>
      ) : (
        <div style={{ fontSize: 12.5, color: 'var(--ol-ink-4)', lineHeight: 1.55 }}>
          {t('meetings.summaryEmpty')}
        </div>
      )}
    </div>
  );
}

function SummaryEditor({
  draft,
  onDraftChange,
}: {
  draft: MeetingEditDraft;
  onDraftChange: (draft: MeetingEditDraft) => void;
}) {
  const { t } = useTranslation();
  const update = (patch: Partial<MeetingEditDraft>) => onDraftChange({ ...draft, ...patch });
  const updateTodo = (index: number, patch: Partial<MeetingTodoDraft>) => {
    update({
      todos: draft.todos.map((todo, todoIndex) => (
        todoIndex === index ? { ...todo, ...patch } : todo
      )),
    });
  };
  const addTodo = () => {
    update({
      todos: [
        ...draft.todos,
        {
          id: `manual-${Date.now()}`,
          content: '',
          owner: '',
          dueDate: '',
          sourceQuote: '',
          sourceSegmentIds: [],
        },
      ],
    });
  };
  const removeTodo = (index: number) => {
    update({ todos: draft.todos.filter((_, todoIndex) => todoIndex !== index) });
  };
  return (
    <div
      className="ol-thinscroll"
      style={{
        display: 'flex',
        flexDirection: 'column',
        gap: 10,
        maxHeight: 'min(58vh, 620px)',
        overflow: 'auto',
        paddingRight: 2,
      }}
    >
      <EditorField label={t('meetings.edit.overviewLabel')}>
        <textarea
          value={draft.overview}
          onChange={event => update({ overview: event.target.value })}
          rows={4}
          style={editorTextareaStyle}
        />
      </EditorField>
      <EditorField label={t('meetings.keyDecisions')} hint={t('meetings.edit.lineHint')}>
        <textarea
          value={draft.keyDecisions}
          onChange={event => update({ keyDecisions: event.target.value })}
          rows={4}
          style={editorTextareaStyle}
        />
      </EditorField>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 11, fontWeight: 600, color: 'var(--ol-ink-3)' }}>
            {t('meetings.todos')}
          </span>
          <Btn icon="plus" variant="ghost" size="sm" onClick={addTodo}>
            {t('meetings.edit.addTodo')}
          </Btn>
        </div>
        {draft.todos.length === 0 ? (
          <div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('meetings.edit.noTodos')}</div>
        ) : draft.todos.map((todo, index) => (
          <div key={todo.id} style={{ border: '0.5px solid var(--ol-line-soft)', borderRadius: 8, padding: 10, background: 'var(--ol-surface)' }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', gap: 8, marginBottom: 8 }}>
              <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>{t('meetings.edit.todoNumber', { index: index + 1 })}</span>
              <Btn icon="trash" variant="ghost" size="sm" onClick={() => removeTodo(index)}>
                {t('common.delete')}
              </Btn>
            </div>
            <div style={{ display: 'grid', gridTemplateColumns: 'minmax(0, 1fr)', gap: 8 }}>
              <EditorField label={t('meetings.edit.todoContentLabel')}>
                <textarea
                  value={todo.content}
                  onChange={event => updateTodo(index, { content: event.target.value })}
                  rows={2}
                  style={editorTextareaStyle}
                />
              </EditorField>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(150px, 1fr))', gap: 8 }}>
                <EditorField label={t('meetings.todoOwner')}>
                  <input
                    value={todo.owner}
                    onChange={event => updateTodo(index, { owner: event.target.value })}
                    style={editorInputStyle}
                  />
                </EditorField>
                <EditorField label={t('meetings.todoDueDate')}>
                  <input
                    value={todo.dueDate}
                    onChange={event => updateTodo(index, { dueDate: event.target.value })}
                    style={editorInputStyle}
                  />
                </EditorField>
              </div>
              <EditorField label={t('meetings.todoSource')}>
                <textarea
                  value={todo.sourceQuote}
                  onChange={event => updateTodo(index, { sourceQuote: event.target.value })}
                  rows={2}
                  style={editorTextareaStyle}
                />
              </EditorField>
              {todo.sourceSegmentIds.length > 0 && (
                <div style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>
                  {t('meetings.todoSourceSegments')}: {todo.sourceSegmentIds.join(', ')}
                </div>
              )}
            </div>
          </div>
        ))}
        <span style={{ fontSize: 11, color: 'var(--ol-ink-4)', lineHeight: 1.45 }}>
          {t('meetings.edit.todoHint')}
        </span>
      </div>
      <EditorField label={t('meetings.risksAndOpenQuestions')} hint={t('meetings.edit.lineHint')}>
        <textarea
          value={draft.risksAndOpenQuestions}
          onChange={event => update({ risksAndOpenQuestions: event.target.value })}
          rows={4}
          style={editorTextareaStyle}
        />
      </EditorField>
    </div>
  );
}

function EditorField({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <label style={{ display: 'flex', flexDirection: 'column', gap: 5 }}>
      <span style={{ fontSize: 11, fontWeight: 600, color: 'var(--ol-ink-3)' }}>
        {label}
      </span>
      {children}
      {hint && (
        <span style={{ fontSize: 11, color: 'var(--ol-ink-4)', lineHeight: 1.45 }}>
          {hint}
        </span>
      )}
    </label>
  );
}

function SummaryList({ title, items }: { title: string; items: string[] }) {
  if (items.length === 0) return null;
  return (
    <div>
      <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--ol-ink-3)', marginBottom: 5 }}>
        {title}
      </div>
      <ul style={{ margin: 0, paddingLeft: 18, color: 'var(--ol-ink)', fontSize: 12.5, lineHeight: 1.6 }}>
        {items.map((item, index) => (
          <li key={`${item}-${index}`}>{item}</li>
        ))}
      </ul>
    </div>
  );
}

function TodoList({ todos }: { todos: MeetingRecord['summary']['todos'] }) {
  const { t } = useTranslation();
  if (todos.length === 0) return null;
  return (
    <div>
      <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--ol-ink-3)', marginBottom: 5 }}>
        {t('meetings.todos')}
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
        {todos.map(todo => (
          <div key={todo.id} style={{ border: '0.5px solid var(--ol-line-soft)', borderRadius: 8, padding: '8px 9px', background: 'var(--ol-surface)' }}>
            <div style={{ fontSize: 12.5, color: 'var(--ol-ink)', lineHeight: 1.55 }}>{todo.content}</div>
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginTop: 5, fontSize: 11, color: 'var(--ol-ink-4)' }}>
              {todo.owner && <span>{t('meetings.todoOwner')}: {todo.owner}</span>}
              {todo.dueDate && <span>{t('meetings.todoDueDate')}: {todo.dueDate}</span>}
              {todo.sourceSegmentIds.length > 0 && (
                <span>{t('meetings.todoSourceSegments')}: {todo.sourceSegmentIds.join(', ')}</span>
              )}
              {todo.sourceQuote && <span>{t('meetings.todoSource')}: {todo.sourceQuote}</span>}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function TranscriptRow({ segment }: { segment: TranscriptSegment }) {
  const { t } = useTranslation();
  return (
    <div style={{
      padding: '11px 12px',
      border: '0.5px solid var(--ol-line)',
      borderRadius: 10,
      background: 'var(--ol-surface-2)',
    }}>
      <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', alignItems: 'center', marginBottom: 7 }}>
        <Pill size="sm" tone="outline">{segment.speakerLabel || t('meetings.unknownSpeaker')}</Pill>
        <Pill size="sm" tone="default">{formatTimestamp(segment.startMs)}</Pill>
        <Pill size="sm" tone="outline">{sourceLabel(segment.source, t)}</Pill>
      </div>
      <div style={{ fontSize: 13, lineHeight: 1.7, color: 'var(--ol-ink)', whiteSpace: 'pre-wrap' }}>
        {segment.text}
      </div>
    </div>
  );
}

function TranscriptDraftRow({ draft }: { draft: MeetingTranscriptDraftEvent }) {
  const { t } = useTranslation();
  return (
    <div style={{
      padding: '11px 12px',
      border: '0.5px dashed var(--ol-line-strong)',
      borderRadius: 10,
      background: 'var(--ol-surface)',
      opacity: 0.86,
    }}>
      <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', alignItems: 'center', marginBottom: 7 }}>
        <Pill size="sm" tone="blue">{t('meetings.draftRecognizing')}</Pill>
        {draft.startMs != null && (
          <Pill size="sm" tone="default">{formatTimestamp(draft.startMs)}</Pill>
        )}
      </div>
      <div style={{ fontSize: 13, lineHeight: 1.7, color: 'var(--ol-ink-2)', whiteSpace: 'pre-wrap' }}>
        {draft.text}
      </div>
    </div>
  );
}

function ErrorBanner({ children, tone }: { children: ReactNode; tone: 'error' | 'warning' }) {
  const warning = tone === 'warning';
  return (
    <div style={{
      marginBottom: 12,
      padding: '9px 10px',
      borderRadius: 8,
      background: warning ? 'rgba(245,158,11,0.10)' : 'rgba(239,68,68,0.08)',
      color: warning ? 'var(--ol-warn, #b45309)' : 'var(--ol-red, #ef4444)',
      fontSize: 12,
      lineHeight: 1.45,
      flexShrink: 0,
    }}>
      {children}
    </div>
  );
}

function MeetingAudioPlayer({
  meetingId,
  onMissing,
}: {
  meetingId: string;
  onMissing: () => void;
}) {
  const { t } = useTranslation();
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const objectUrlRef = useRef<string | null>(null);
  const audioContextRef = useRef<AudioContext | null>(null);
  const audioSourceRef = useRef<MediaElementAudioSourceNode | null>(null);
  const gainNodeRef = useRef<GainNode | null>(null);
  const limiterNodeRef = useRef<DynamicsCompressorNode | null>(null);
  const loadRequestRef = useRef(0);
  const [status, setStatus] = useState<'idle' | 'loading' | 'ready' | 'missing' | 'error'>('idle');
  const [errorText, setErrorText] = useState<string | null>(null);
  const [isPlaying, setIsPlaying] = useState(false);
  const [duration, setDuration] = useState(0);
  const [currentTime, setCurrentTime] = useState(0);
  const [speed, setSpeed] = useState<PlaybackSpeed>(1);
  const [volume, setVolume] = useState(1);

  useEffect(() => {
    return () => {
      loadRequestRef.current += 1;
      if (objectUrlRef.current) URL.revokeObjectURL(objectUrlRef.current);
      void audioContextRef.current?.close();
    };
  }, []);

  useEffect(() => {
    loadRequestRef.current += 1;
    const audio = audioRef.current;
    if (audio) {
      audio.pause();
      audio.removeAttribute('src');
      audio.load();
    }
    if (objectUrlRef.current) {
      URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = null;
    }
    setStatus('idle');
    setErrorText(null);
    setIsPlaying(false);
    setDuration(0);
    setCurrentTime(0);
  }, [meetingId]);

  useEffect(() => {
    if (audioRef.current) audioRef.current.playbackRate = speed;
  }, [speed]);

  useEffect(() => {
    const gainNode = gainNodeRef.current;
    if (gainNode) {
      gainNode.gain.value = volume;
    } else if (audioRef.current) {
      audioRef.current.volume = Math.min(volume, 1);
    }
  }, [volume]);

  const syncTiming = () => {
    const audio = audioRef.current;
    if (!audio) return;
    setCurrentTime(Number.isFinite(audio.currentTime) ? audio.currentTime : 0);
    setDuration(Number.isFinite(audio.duration) ? audio.duration : 0);
  };

  const playCurrentAudio = async (requestId = loadRequestRef.current) => {
    const audio = audioRef.current;
    if (!audio) return;
    try {
      let context = audioContextRef.current;
      if (!context) {
        context = new AudioContext();
        audioContextRef.current = context;
        const source = context.createMediaElementSource(audio);
        const gainNode = context.createGain();
        const limiterNode = context.createDynamicsCompressor();
        limiterNode.threshold.value = -3;
        limiterNode.knee.value = 0;
        limiterNode.ratio.value = 20;
        limiterNode.attack.value = 0.003;
        limiterNode.release.value = 0.25;
        source.connect(gainNode).connect(limiterNode).connect(context.destination);
        audioSourceRef.current = source;
        gainNodeRef.current = gainNode;
        limiterNodeRef.current = limiterNode;
        audio.volume = 1;
        gainNode.gain.value = volume;
      }
      if (context.state === 'suspended') await context.resume();
      if (loadRequestRef.current !== requestId) return;
      await audio.play();
    } catch (error) {
      if (loadRequestRef.current !== requestId) return;
      console.error('[meetings] play audio failed', error);
      setStatus('error');
      setErrorText(errorMessage(error));
    }
  };

  const loadAudio = async (playAfterLoad: boolean) => {
    const requestId = ++loadRequestRef.current;
    setStatus('loading');
    setErrorText(null);
    let candidateObjectUrl: string | null = null;
    try {
      const source = await prepareMeetingAudioPlayback(meetingId);
      let audioUrl: string;
      if (typeof source === 'string') {
        audioUrl = convertFileSrc(source);
      } else {
        if (binaryPayloadLength(source) === 0) throw new Error('empty meeting recording');
        const buffer = binaryPayloadToArrayBuffer(source);
        const blob = new Blob([buffer], { type: 'audio/wav' });
        audioUrl = URL.createObjectURL(blob);
        candidateObjectUrl = audioUrl;
      }

      if (loadRequestRef.current !== requestId) {
        if (candidateObjectUrl) URL.revokeObjectURL(candidateObjectUrl);
        return;
      }
      const audio = audioRef.current;
      if (!audio) {
        if (candidateObjectUrl) URL.revokeObjectURL(candidateObjectUrl);
        return;
      }
      if (objectUrlRef.current) URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = candidateObjectUrl;
      audio.src = audioUrl;
      audio.playbackRate = speed;
      audio.currentTime = 0;
      setCurrentTime(0);
      setDuration(0);
      setStatus('ready');
      if (playAfterLoad) await playCurrentAudio(requestId);
    } catch (error) {
      if (candidateObjectUrl) URL.revokeObjectURL(candidateObjectUrl);
      if (loadRequestRef.current !== requestId) return;
      console.error('[meetings] load audio failed', error);
      const msg = errorMessage(error);
      if (msg.includes('meeting recording not found') || msg.includes('not found')) {
        setStatus('missing');
        onMissing();
        return;
      }
      setStatus('error');
      setErrorText(msg);
    }
  };

  const togglePlayback = () => {
    if (status !== 'ready') {
      void loadAudio(true);
      return;
    }
    const audio = audioRef.current;
    if (!audio) return;
    if (audio.paused) {
      void playCurrentAudio();
    } else {
      audio.pause();
    }
  };

  const seekTo = (value: string) => {
    const next = Number(value);
    if (!Number.isFinite(next)) return;
    const audio = audioRef.current;
    if (audio) audio.currentTime = next;
    setCurrentTime(next);
  };

  const changeVolume = (value: string) => {
    const next = Number(value);
    if (!Number.isFinite(next)) return;
    setVolume(Math.min(2, Math.max(0, next)));
  };

  return (
    <div style={{
      marginBottom: 14,
      padding: 12,
      border: '0.5px solid var(--ol-line)',
      borderRadius: 8,
      background: 'var(--ol-surface-2)',
    }}>
      <audio
        ref={audioRef}
        crossOrigin="anonymous"
        preload="metadata"
        onLoadedMetadata={syncTiming}
        onDurationChange={syncTiming}
        onTimeUpdate={syncTiming}
        onPlay={() => setIsPlaying(true)}
        onPause={() => setIsPlaying(false)}
        onEnded={() => setIsPlaying(false)}
      />
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10, marginBottom: 10, flexWrap: 'wrap' }}>
        <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--ol-ink-2)' }}>
          {t('meetings.audioPlayback.title')}
        </span>
        <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
          <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>{t('meetings.audioPlayback.speed')}</span>
          <div style={{ display: 'flex', border: '0.5px solid var(--ol-line)', borderRadius: 8, overflow: 'hidden', background: 'var(--ol-surface)' }}>
            {PLAYBACK_SPEEDS.map(value => (
              <button
                key={value}
                type="button"
                aria-pressed={speed === value}
                onClick={() => setSpeed(value)}
                style={{
                  minWidth: 44,
                  height: 28,
                  padding: '0 8px',
                  border: 0,
                  borderLeft: value === PLAYBACK_SPEEDS[0] ? 0 : '0.5px solid var(--ol-line-soft)',
                  background: speed === value ? 'rgba(37,99,235,0.10)' : 'transparent',
                  color: speed === value ? 'var(--ol-blue)' : 'var(--ol-ink-3)',
                  fontSize: 11,
                  fontFamily: 'var(--ol-font-mono)',
                  cursor: 'default',
                }}
              >
                {formatPlaybackSpeed(value)}
              </button>
            ))}
          </div>
        </div>
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: 'auto minmax(120px, 1fr) auto', alignItems: 'center', gap: 10 }}>
        <Btn
          icon={isPlaying ? 'pause' : 'play'}
          variant={isPlaying ? 'blue' : 'ghost'}
          size="sm"
          onClick={togglePlayback}
          disabled={status === 'loading'}
        >
          {status === 'loading'
            ? t('meetings.audioPlayback.loading')
            : isPlaying
              ? t('meetings.audioPlayback.pause')
              : t('meetings.audioPlayback.play')}
        </Btn>
        <input
          type="range"
          min={0}
          max={duration > 0 ? duration : 0}
          step={0.1}
          value={duration > 0 ? Math.min(currentTime, duration) : 0}
          onChange={event => seekTo(event.target.value)}
          disabled={status !== 'ready' || duration <= 0}
          aria-label={t('meetings.audioPlayback.progress')}
          style={{ width: '100%', accentColor: 'var(--ol-blue)' }}
        />
        <span style={{ fontSize: 11, fontFamily: 'var(--ol-font-mono)', color: 'var(--ol-ink-4)', whiteSpace: 'nowrap' }}>
          {formatPlaybackTime(currentTime)} / {formatPlaybackTime(duration)}
        </span>
      </div>
      <div style={{ display: 'flex', justifyContent: 'flex-end', alignItems: 'center', gap: 8, marginTop: 8 }}>
        <Volume2 size={14} aria-hidden="true" style={{ color: 'var(--ol-ink-4)', flexShrink: 0 }} />
        <input
          type="range"
          min={0}
          max={2}
          step={0.1}
          value={volume}
          onChange={event => changeVolume(event.target.value)}
          aria-label={t('meetings.audioPlayback.volume')}
          title={t('meetings.audioPlayback.volume')}
          style={{ width: 112, accentColor: 'var(--ol-blue)' }}
        />
        <span style={{ width: 36, textAlign: 'right', fontSize: 11, fontFamily: 'var(--ol-font-mono)', color: 'var(--ol-ink-4)' }}>
          {Math.round(volume * 100)}%
        </span>
      </div>
      {status === 'missing' && (
        <div style={{ marginTop: 8, fontSize: 11, color: 'var(--ol-ink-4)' }}>
          {t('meetings.audioPlayback.missing')}
        </div>
      )}
      {status === 'error' && (
        <div style={{ marginTop: 8, fontSize: 11, color: 'var(--ol-red, #ef4444)' }}>
          {t('meetings.audioPlayback.loadFailed', { err: errorText ?? '-' })}
        </div>
      )}
    </div>
  );
}

function meetingListItemFromRecord(record: MeetingRecord): MeetingListItem {
  return {
    id: record.id,
    title: record.title,
    status: record.status,
    startedAt: record.startedAt,
    endedAt: record.endedAt,
    durationMs: record.durationMs,
    summaryOverview: record.summary.overview,
    transcriptPreview: record.transcriptSegments.find(segment => segment.text.trim())?.text.trim().slice(0, 180) ?? '',
    transcriptSegmentCount: record.transcriptSegments.length,
    audio: record.audio,
    createdAt: record.createdAt,
    updatedAt: record.updatedAt,
  };
}

function upsertMeetingListItem(records: MeetingListItem[], record: MeetingListItem): MeetingListItem[] {
  const exists = records.some(item => item.id === record.id);
  const next = exists
    ? records.map(item => (item.id === record.id ? record : item))
    : [record, ...records];
  return [...next].sort((a, b) => dateMs(b.startedAt) - dateMs(a.startedAt));
}

function appendSegmentToMeetingList(
  records: MeetingListItem[],
  meetingId: string,
  segment: TranscriptSegment,
): MeetingListItem[] {
  return records.map(record => {
    if (record.id !== meetingId) return record;
    const text = segment.text.trim();
    return {
      ...record,
      transcriptPreview: record.transcriptPreview || text.slice(0, 180),
      transcriptSegmentCount: record.transcriptSegmentCount + 1,
    };
  });
}

function missingMeetingAudio(): MeetingRecord['audio'] {
  return {
    state: 'missing',
    retained: false,
    path: null,
  };
}

function withMissingAudio(record: MeetingRecord): MeetingRecord {
  return {
    ...record,
    audio: missingMeetingAudio(),
  };
}

function removeDraft(
  drafts: Record<string, MeetingTranscriptDraftEvent>,
  meetingId: string,
): Record<string, MeetingTranscriptDraftEvent> {
  if (!(meetingId in drafts)) return drafts;
  const next = { ...drafts };
  delete next[meetingId];
  return next;
}

function hasSegment(record: MeetingRecord, segmentId: string): boolean {
  return record.transcriptSegments.some(segment => segment.id === segmentId);
}

function canEditMeeting(record: MeetingRecord, snapshot: MeetingRecordingSnapshot | null): boolean {
  if (snapshot?.meeting.id === record.id) return false;
  return record.status !== 'recording'
    && record.status !== 'paused'
    && record.status !== 'summarizing';
}

function canDeleteMeeting(record: MeetingRecord, snapshot: MeetingRecordingSnapshot | null): boolean {
  if (snapshot?.meeting.id === record.id) return false;
  return record.status !== 'recording'
    && record.status !== 'paused'
    && record.status !== 'summarizing';
}

function canRetranscribeMeeting(record: MeetingRecord, snapshot: MeetingRecordingSnapshot | null): boolean {
  if (snapshot) return false;
  return record.audio.state === 'retained'
    && record.status !== 'recording'
    && record.status !== 'paused'
    && record.status !== 'summarizing';
}

function canPlayMeetingAudio(record: MeetingRecord, snapshot: MeetingRecordingSnapshot | null): boolean {
  if (snapshot) return false;
  return record.audio.state === 'retained'
    && record.status !== 'recording'
    && record.status !== 'paused';
}

function createEditDraft(record: MeetingRecord): MeetingEditDraft {
  return {
    id: record.id,
    title: record.title,
    overview: record.summary.overview,
    keyDecisions: record.summary.keyDecisions.join('\n'),
    todos: record.summary.todos.map(todo => ({
      id: todo.id,
      content: todo.content,
      owner: todo.owner ?? '',
      dueDate: todo.dueDate ?? '',
      sourceQuote: todo.sourceQuote ?? '',
      sourceSegmentIds: todo.sourceSegmentIds,
    })),
    risksAndOpenQuestions: record.summary.risksAndOpenQuestions.join('\n'),
  };
}

function linesFromDraft(value: string): string[] {
  return value
    .split(/\r?\n/)
    .map(line => line.trim())
    .filter(Boolean);
}

function todosFromDraft(value: MeetingTodoDraft[], record: MeetingRecord): MeetingRecord['summary']['todos'] {
  return value
    .map((todo, index) => ({
      id: todo.id || `manual-${record.id}-${index + 1}`,
      content: todo.content.trim(),
      owner: emptyToNull(todo.owner),
      dueDate: emptyToNull(todo.dueDate),
      sourceSegmentIds: todo.sourceSegmentIds,
      sourceQuote: emptyToNull(todo.sourceQuote),
    }))
    .filter(todo => todo.content.length > 0);
}

function meetingSearchText(record: MeetingListItem): string {
  return `${record.title}\n${record.summaryOverview}`.toLowerCase();
}

function statusLabel(status: MeetingStatus, t: ReturnType<typeof useTranslation>['t']): string {
  const keyByStatus: Record<MeetingStatus, string> = {
    draft: 'draft',
    recording: 'recording',
    paused: 'paused',
    transcribing_interrupted: 'transcribingInterrupted',
    summarizing: 'summarizing',
    summary_failed: 'summaryFailed',
    completed: 'completed',
  };
  return t(`meetings.status.${keyByStatus[status]}`);
}

function phaseLabel(phase: MeetingRecordingPhase, t: ReturnType<typeof useTranslation>['t']): string {
  const keyByPhase: Record<MeetingRecordingPhase, string> = {
    starting: 'starting',
    recording: 'recording',
    paused: 'paused',
    stopping: 'stopping',
    transcribing_interrupted: 'transcribingInterrupted',
  };
  return t(`meetings.phase.${keyByPhase[phase]}`);
}

function controlModeForSnapshot(snapshot: MeetingRecordingSnapshot): { meetingId: string; mode: ActiveControlMode } | null {
  const mode = controlModeForPhase(snapshot.phase);
  return mode ? { meetingId: snapshot.meeting.id, mode } : null;
}

function controlModeForPhase(phase: MeetingRecordingPhase): ActiveControlMode | null {
  if (phase === 'recording') return 'recording';
  if (phase === 'paused') return 'paused';
  return null;
}

function statusTone(status: MeetingStatus): PillTone {
  if (status === 'recording' || status === 'paused') return 'blue';
  if (status === 'completed') return 'ok';
  if (status === 'transcribing_interrupted' || status === 'summary_failed') return 'outline';
  return 'default';
}

function audioLabel(state: MeetingAudioState, t: ReturnType<typeof useTranslation>['t']): string {
  const keyByState: Record<MeetingAudioState, string> = {
    temporary: 'temporary',
    retained: 'retained',
    pruned: 'pruned',
    missing: 'missing',
    unavailable: 'unavailable',
  };
  return t(`meetings.audio.${keyByState[state]}`);
}

function sourceLabel(source: TranscriptSegmentSource, t: ReturnType<typeof useTranslation>['t']): string {
  const keyBySource: Record<TranscriptSegmentSource, string> = {
    realtime_asr: 'realtimeAsr',
    retranscribed_asr: 'retranscribedAsr',
  };
  return t(`meetings.source.${keyBySource[source]}`);
}

function formatTimestamp(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
}

function formatPlaybackTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '00:00';
  const totalSeconds = Math.floor(seconds);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const sec = totalSeconds % 60;
  if (hours > 0) {
    return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(sec).padStart(2, '0')}`;
  }
  return `${String(minutes).padStart(2, '0')}:${String(sec).padStart(2, '0')}`;
}

function formatPlaybackSpeed(value: PlaybackSpeed): string {
  return `${value}x`;
}

function binaryPayloadLength(payload: BinaryPayload): number {
  if (payload instanceof ArrayBuffer) return payload.byteLength;
  if (ArrayBuffer.isView(payload)) return payload.byteLength;
  return payload.length;
}

function binaryPayloadToArrayBuffer(payload: BinaryPayload): ArrayBuffer {
  if (payload instanceof ArrayBuffer) return payload;
  if (ArrayBuffer.isView(payload)
    && payload.byteOffset === 0
    && payload.byteLength === payload.buffer.byteLength
    && payload.buffer instanceof ArrayBuffer) {
    return payload.buffer;
  }
  const buffer = new ArrayBuffer(binaryPayloadLength(payload));
  const target = new Uint8Array(buffer);
  if (ArrayBuffer.isView(payload)) {
    target.set(new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength));
  } else {
    target.set(payload);
  }
  return buffer;
}

function formatDateTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const now = new Date();
  const sameDay = date.toDateString() === now.toDateString();
  const pad = (value: number) => String(value).padStart(2, '0');
  if (sameDay) return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  return `${date.getMonth() + 1}/${date.getDate()} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function formatDuration(ms: number | null, t: ReturnType<typeof useTranslation>['t']): string {
  if (ms == null || ms <= 0) return '-';
  const sec = ms / 1000;
  if (sec < 60) return t('common.durationSeconds', { value: sec.toFixed(1) });
  return t('common.durationMinutes', { value: (sec / 60).toFixed(1) });
}

function dateMs(iso: string): number {
  const value = new Date(iso).getTime();
  return Number.isNaN(value) ? 0 : value;
}

function errorMessage(error: unknown): string {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

function emptyToNull(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

async function chooseMarkdownExportPath(record: MeetingRecord): Promise<string | null> {
  const suggestedFileName = `${safeFileName(record.title || 'meeting')}.md`;
  if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) {
    return `~/Downloads/${suggestedFileName}`;
  }
  const { save } = await import('@tauri-apps/plugin-dialog');
  return save({
    defaultPath: suggestedFileName,
    filters: [{ name: 'Markdown', extensions: ['md', 'markdown'] }],
  });
}

function safeFileName(value: string): string {
  const normalized = value
    .trim()
    .replace(/[<>:"/\\|?*\x00-\x1F]+/g, ' ')
    .replace(/\s+/g, ' ')
    .slice(0, 80)
    .trim();
  return normalized || 'meeting';
}

const editorInputStyle: CSSProperties = {
  width: '100%',
  boxSizing: 'border-box',
  minHeight: 36,
  padding: '8px 10px',
  borderRadius: 8,
  border: '0.5px solid var(--ol-line-strong)',
  background: 'var(--ol-surface)',
  color: 'var(--ol-ink)',
  fontFamily: 'inherit',
  outline: 'none',
};

const editorTextareaStyle: CSSProperties = {
  width: '100%',
  boxSizing: 'border-box',
  padding: '9px 10px',
  borderRadius: 8,
  border: '0.5px solid var(--ol-line-strong)',
  background: 'var(--ol-surface)',
  color: 'var(--ol-ink)',
  fontFamily: 'inherit',
  fontSize: 12.5,
  lineHeight: 1.6,
  maxWidth: '100%',
  minHeight: 76,
  resize: 'vertical',
  outline: 'none',
};
