import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type MutableRefObject,
  type ReactNode,
} from 'react';
import { useTranslation } from 'react-i18next';
import { Icon } from '../components/Icon';
import {
  getActiveMeetingRecording,
  getMeeting,
  retryMeetingSummary,
  listMeetings,
  pauseMeetingRecording,
  resumeMeetingRecording,
  startMeetingRecording,
  stopMeetingRecording,
} from '../lib/ipc';
import type {
  MeetingAudioState,
  MeetingErrorEvent,
  MeetingRecord,
  MeetingRecordingPhase,
  MeetingRecordingSnapshot,
  MeetingStatus,
  MeetingSummaryEvent,
  MeetingTranscriptSegmentEvent,
  TranscriptSegment,
  TranscriptSegmentSource,
} from '../lib/types';
import { useMobileLayout } from '../lib/useMobileLayout';
import { Btn, Card, PageHeader, Pill, type PillTone } from './_atoms';

type ActionLoading = 'start' | 'pause' | 'resume' | 'stop' | 'summary' | null;
type ActiveControlMode = 'recording' | 'paused';

export function Meetings() {
  const { t } = useTranslation();
  const mobile = useMobileLayout();
  const [meetings, setMeetings] = useState<MeetingRecord[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [activeSnapshot, setActiveSnapshot] = useState<MeetingRecordingSnapshot | null>(null);
  const [query, setQuery] = useState('');
  const [loading, setLoading] = useState(true);
  const [actionLoading, setActionLoading] = useState<ActionLoading>(null);
  const [activeControlMode, setActiveControlMode] = useState<{ meetingId: string; mode: ActiveControlMode } | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [eventError, setEventError] = useState<MeetingErrorEvent | null>(null);
  const [mobileDetailOpen, setMobileDetailOpen] = useState(false);
  const transcriptScrollRef = useRef<HTMLDivElement | null>(null);
  const transcriptStickToBottomRef = useRef(true);
  const meetingsRef = useRef<MeetingRecord[]>([]);

  useEffect(() => {
    meetingsRef.current = meetings;
  }, [meetings]);

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
      setMeetings(prev => upsertMeeting(prev, snapshot.meeting));
      setSelectedId(prev => prev ?? snapshot.meeting.id);
      setActiveControlMode(prev => {
        const nextMode = controlModeForPhase(snapshot.phase);
        if (nextMode) return { meetingId: snapshot.meeting.id, mode: nextMode };
        return prev?.meetingId === snapshot.meeting.id ? prev : null;
      });
    } catch (error) {
      console.warn('[meetings] active snapshot refresh failed', error);
    }
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const [active, records] = await Promise.all([
        getActiveMeetingRecording(),
        listMeetings(),
      ]);
      const nextRecords = active ? upsertMeeting(records, active.meeting) : records;
      setActiveSnapshot(active);
      setActiveControlMode(active ? controlModeForSnapshot(active) : null);
      setMeetings(nextRecords);
      setSelectedId(prev => {
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
    let unlistenSegment: (() => void) | undefined;
    let unlistenError: (() => void) | undefined;
    let unlistenSummary: (() => void) | undefined;

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
          setMeetings(prev => upsertMeeting(prev, snapshot.meeting));
          setSelectedId(prev => prev ?? snapshot.meeting.id);
        });
        const segmentHandle = await listen<MeetingTranscriptSegmentEvent>('meeting:transcript-segment', event => {
          if (cancelled) return;
          const payload = event.payload;
          const knownMeeting = meetingsRef.current.some(record => record.id === payload.meetingId);
          setMeetings(prev => appendSegment(prev, payload.meetingId, payload.segment));
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
            setMeetings(prev => upsertMeeting(prev, payload.meeting!));
            setSelectedId(prev => prev ?? payload.meeting!.id);
          }
          if (payload.error) setEventError(payload.error);
        });

        if (cancelled) {
          stateHandle();
          segmentHandle();
          errorHandle();
          summaryHandle();
        } else {
          unlistenState = stateHandle;
          unlistenSegment = segmentHandle;
          unlistenError = errorHandle;
          unlistenSummary = summaryHandle;
        }
      } catch (error) {
        console.warn('[meetings] event listener setup failed', error);
      }
    })();

    return () => {
      cancelled = true;
      unlistenState?.();
      unlistenSegment?.();
      unlistenError?.();
      unlistenSummary?.();
    };
  }, [syncActiveSnapshot]);

  const filteredMeetings = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return meetings;
    return meetings.filter(record => {
      if (record.title.toLowerCase().includes(q)) return true;
      return record.transcriptSegments.some(segment => segment.text.toLowerCase().includes(q));
    });
  }, [meetings, query]);

  const selectedMeeting = useMemo(() => {
    const visibleSelected = filteredMeetings.find(record => record.id === selectedId);
    return visibleSelected ?? filteredMeetings[0] ?? null;
  }, [filteredMeetings, selectedId]);

  const detailMeeting = activeSnapshot && selectedMeeting?.id === activeSnapshot.meeting.id
    ? activeSnapshot.meeting
    : selectedMeeting;
  const selectedActiveSnapshot = activeSnapshot && detailMeeting?.id === activeSnapshot.meeting.id
    ? activeSnapshot
    : null;
  const selectedControlMode = activeControlMode && detailMeeting?.id === activeControlMode.meetingId
    ? activeControlMode.mode
    : null;
  const transcriptCount = detailMeeting?.transcriptSegments.length ?? 0;

  useEffect(() => {
    if (!transcriptStickToBottomRef.current) return;
    const el = transcriptScrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [detailMeeting?.id, transcriptCount]);

  const selectMeeting = async (id: string) => {
    setSelectedId(id);
    setActionError(null);
    if (mobile) setMobileDetailOpen(true);
    try {
      const record = await getMeeting(id);
      setMeetings(prev => upsertMeeting(prev, record));
    } catch (error) {
      console.error('[meetings] failed to load meeting detail', error);
      setActionError(t('meetings.detailLoadFailed', { err: errorMessage(error) }));
    }
  };

  const runStart = async () => {
    setActionLoading('start');
    setActionError(null);
    setEventError(null);
    try {
      const snapshot = await startMeetingRecording();
      setActiveSnapshot(snapshot);
      setActiveControlMode({ meetingId: snapshot.meeting.id, mode: 'recording' });
      setMeetings(prev => upsertMeeting(prev, snapshot.meeting));
      setSelectedId(snapshot.meeting.id);
      if (mobile) setMobileDetailOpen(true);
    } catch (error) {
      console.error('[meetings] start failed', error);
      setActionError(t('meetings.actionFailed', { err: errorMessage(error) }));
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
      setMeetings(prev => upsertMeeting(prev, snapshot.meeting));
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
      setMeetings(prev => upsertMeeting(prev, snapshot.meeting));
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
      setMeetings(prev => upsertMeeting(prev, record));
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
      setMeetings(prev => upsertMeeting(prev, record));
      setSelectedId(record.id);
    } catch (error) {
      console.error('[meetings] retry summary failed', error);
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
                  controlMode={selectedControlMode}
                  actionLoading={actionLoading}
                  onPause={() => void runPause(detailMeeting.id)}
                  onResume={() => void runResume(detailMeeting.id)}
                  onStop={() => void runStop(detailMeeting.id)}
                />
                {actionError && (
                  <ErrorBanner tone="error">{actionError}</ErrorBanner>
                )}
                {eventError && (!eventError.meetingId || eventError.meetingId === detailMeeting.id) && (
                  <ErrorBanner tone="error">
                    {t('meetings.eventError', { message: eventError.message })}
                  </ErrorBanner>
                )}
                {(detailMeeting.status === 'transcribing_interrupted' || selectedActiveSnapshot?.asrInterrupted) && (
                  <ErrorBanner tone="warning">
                    {t('meetings.asrInterrupted')}
                  </ErrorBanner>
                )}
                <SummarySection
                  record={detailMeeting}
                  actionLoading={actionLoading}
                  onRetry={() => void runRetrySummary(detailMeeting.id)}
                />
                <TranscriptList
                  record={detailMeeting}
                  scrollRef={transcriptScrollRef}
                  onScroll={() => {
                    const el = transcriptScrollRef.current;
                    if (!el) return;
                    transcriptStickToBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
                  }}
                />
              </>
            ) : (
              <div style={{ padding: 40, textAlign: 'center', fontSize: 13, color: 'var(--ol-ink-4)' }}>
                {loading ? t('common.loading') : loadError ? t('meetings.loadFailed', { err: loadError }) : t('meetings.selectHint')}
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
  record: MeetingRecord;
  selected: boolean;
  active: boolean;
  onSelect: () => void;
}) {
  const { t } = useTranslation();
  const preview = meetingPreview(record);
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
  controlMode,
  actionLoading,
  onPause,
  onResume,
  onStop,
}: {
  record: MeetingRecord;
  snapshot: MeetingRecordingSnapshot | null;
  controlMode: ActiveControlMode | null;
  actionLoading: ActionLoading;
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
            <h2 style={{ margin: 0, fontSize: 18, fontWeight: 600, color: 'var(--ol-ink)', lineHeight: 1.25 }}>
              {record.title || t('meetings.untitled')}
            </h2>
            <Pill size="sm" tone={statusTone(record.status)}>{statusLabel(record.status, t)}</Pill>
            <Pill size="sm" tone="outline">{audioLabel(record.audio.state, t)}</Pill>
          </div>
          <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', fontSize: 11, color: 'var(--ol-ink-4)' }}>
            <span>{t('meetings.startedAt')}: {formatDateTime(record.startedAt)}</span>
            <span>{active ? t('meetings.elapsed') : t('meetings.duration')}: {formatDuration(active ? snapshot.elapsedMs : record.durationMs, t)}</span>
            {snapshot?.activeAsrProvider && (
              <span>{t('meetings.provider')}: <span style={{ fontFamily: 'var(--ol-font-mono)' }}>{snapshot.activeAsrProvider}</span></span>
            )}
          </div>
        </div>
        {(showPause || showResume || showStop) && (
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', justifyContent: 'flex-end' }}>
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
          </div>
        )}
      </div>
    </div>
  );
}

function TranscriptList({
  record,
  scrollRef,
  onScroll,
}: {
  record: MeetingRecord;
  scrollRef: MutableRefObject<HTMLDivElement | null>;
  onScroll: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10, marginBottom: 10, flexShrink: 0 }}>
        <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--ol-ink-2)' }}>
          {t('meetings.transcriptTitle')}
        </span>
        <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>
          {t('meetings.segmentCount', { count: record.transcriptSegments.length })}
        </span>
      </div>
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="ol-thinscroll"
        style={{ flex: 1, minHeight: 0, overflow: 'auto', paddingRight: 2 }}
      >
        {record.transcriptSegments.length === 0 ? (
          <div style={{ padding: 18, border: '0.5px solid var(--ol-line)', borderRadius: 10, background: 'var(--ol-surface-2)', color: 'var(--ol-ink-4)', fontSize: 12.5, lineHeight: 1.55 }}>
            {t('meetings.noTranscript')}
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {record.transcriptSegments.map(segment => (
              <TranscriptRow key={segment.id} segment={segment} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function SummarySection({
  record,
  actionLoading,
  onRetry,
}: {
  record: MeetingRecord;
  actionLoading: ActionLoading;
  onRetry: () => void;
}) {
  const { t } = useTranslation();
  const summary = record.summary;
  const hasSummary = Boolean(
    summary.overview.trim()
    || summary.keyDecisions.length
    || summary.todos.length
    || summary.risksAndOpenQuestions.length,
  );
  return (
    <div style={{
      flexShrink: 0,
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
        {record.status === 'summary_failed' && (
          <Btn icon="refresh" variant="ghost" size="sm" disabled={actionLoading !== null} onClick={onRetry}>
            {actionLoading === 'summary' ? t('meetings.actions.summaryRetrying') : t('meetings.actions.summaryRetry')}
          </Btn>
        )}
      </div>
      {record.status === 'summarizing' ? (
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

function upsertMeeting(records: MeetingRecord[], record: MeetingRecord): MeetingRecord[] {
  const exists = records.some(item => item.id === record.id);
  const next = exists
    ? records.map(item => (item.id === record.id ? record : item))
    : [record, ...records];
  return [...next].sort((a, b) => dateMs(b.startedAt) - dateMs(a.startedAt));
}

function appendSegment(records: MeetingRecord[], meetingId: string, segment: TranscriptSegment): MeetingRecord[] {
  return records.map(record => {
    if (record.id !== meetingId || hasSegment(record, segment.id)) return record;
    return {
      ...record,
      transcriptSegments: [...record.transcriptSegments, segment],
    };
  });
}

function hasSegment(record: MeetingRecord, segmentId: string): boolean {
  return record.transcriptSegments.some(segment => segment.id === segmentId);
}

function meetingPreview(record: MeetingRecord): string {
  return record.transcriptSegments.find(segment => segment.text.trim().length > 0)?.text.trim() ?? '';
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
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
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
