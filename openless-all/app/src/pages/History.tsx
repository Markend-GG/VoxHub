// History.tsx — 接 Tauri 后端 list_history / delete_history_entry / clear_history。
// 真实数据来自 ~/Library/Application Support/OpenLess/history.json。

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { Icon } from '../components/Icon';
import { Tooltip } from '../components/Tooltip';
import { detectOS } from '../components/WindowChrome';
import { formatComboLabel } from '../lib/hotkey';
import { clearHistory, clearRewriteHistory, clearScreenshotRecords, deleteHistoryEntry, deleteRewriteHistoryEntry, deleteScreenshotRecord, getScreenshotAggregationStatus, isTauri, listHistory, listRewriteHistory, listScreenshotRecords, readAudioRecording, readContextScreenshot, reanalyzeContextHistory, reanalyzeScreenshotRecord, retranscribeRecording } from '../lib/ipc';
import { setSettings } from '../lib/ipc/settings';
import { useMobileLayout } from '../lib/useMobileLayout';
import type { ContextCaptureEntry, ContextAnalysisResult, DictationSession, PolishMode, ScreenshotRecord } from '../lib/types';
import type { RewriteHistoryEntry } from '../lib/ipc/rewrite';
import { useHotkeySettings } from '../state/HotkeySettingsContext';
import { Btn, Card, PageHeader, Pill } from './_atoms';
import { chipSelectedStyle } from './settings/shared';

const CONTEXT_SCREENSHOT_CACHE_LIMIT = 24;
const REANALYSIS_STATUS_CLEAR_MS = 4_000;
const DEFAULT_CONTEXT_ANALYSIS_FULL_SUMMARY_PROMPT = `fullSummary 是日报、周报、月报和历史复盘的上游材料，不是截图说明。请输出 100 到 300 个中文字符，让报告生成模型即使不看截图也能理解这条历史的工作含义：
- 必须交代当前工作背景，例如项目、页面、对话、文档、任务或正在处理的问题。
- 必须说明本条历史的来源语义：语音历史是用户通过语音表达、记录、询问或确认的内容；重写历史是用户对选中文本做表达调整或准备发送；截图记录只是屏幕中正在查看、讨论、处理或记录的上下文线索。
- 必须体现 workStatus 和 evidenceLevel 对应的事实强度。没有明确完成证据时，不要写成“已完成工作”。
- 对语音历史和重写历史，结合用户原文、处理后文本和截图上下文解释业务含义，不要只描述界面。
- 对截图记录，除非截图中有明确完成、提交、发布、上线、修复完成、测试通过或确认完成证据，否则应写成“正在查看/正在讨论/正在处理/待确认”。
- 提取可被报告复用的信息：进展、结论、待办、风险、协作对象、项目、交付物或后续价值；没有明确依据时写“未形成明确完成事项”或“待确认”。
- 保留必要的不确定性，不要把猜测包装成事实，不要补充截图、原文和窗口元数据之外的事实。
- 不输出大段 OCR、聊天逐字稿或与任务无关的 UI 描述。
- 不泄露 API Key、token、验证码、手机号、邮箱、地址、订单号、完整链接等敏感信息原文；如可见敏感信息，只做泛化说明。`;

type ReanalysisUiStatus = 'busy' | 'queued' | 'success' | 'failed' | 'skipped' | 'disabled';

interface ReanalysisUiState {
  status: ReanalysisUiStatus;
  updatedAt: number;
  errorCode?: string | null;
  previousAnalysisId?: string | null;
}

type ReanalysisStateMap = Record<string, ReanalysisUiState>;
type HistoryRefreshOptions = { silent?: boolean; force?: boolean };

type ContextScreenshotCacheEntry =
  | { state: 'loading'; promise: Promise<string>; lastUsed: number }
  | { state: 'ready'; url: string; lastUsed: number; refCount: number };

const contextScreenshotCache = new Map<string, ContextScreenshotCacheEntry>();

function retainCachedContextScreenshotUrl(contextId: string): string | null {
  const cached = contextScreenshotCache.get(contextId);
  if (cached?.state !== 'ready') return null;
  cached.lastUsed = Date.now();
  cached.refCount += 1;
  return cached.url;
}

function retainContextScreenshotUrl(contextId: string): Promise<string> {
  const cached = contextScreenshotCache.get(contextId);
  if (cached?.state === 'ready') {
    cached.lastUsed = Date.now();
    cached.refCount += 1;
    return Promise.resolve(cached.url);
  }
  if (cached?.state === 'loading') {
    cached.lastUsed = Date.now();
    return cached.promise.then(url => retainReadyContextScreenshotUrl(contextId, url));
  }

  const promise = readContextScreenshot(contextId).then(bytes => {
    if (bytes.byteLength === 0) {
      throw new Error('empty screenshot');
    }
    const buffer = new ArrayBuffer(bytes.byteLength);
    new Uint8Array(buffer).set(bytes);
    return URL.createObjectURL(new Blob([buffer], { type: 'image/bmp' }));
  });

  contextScreenshotCache.set(contextId, { state: 'loading', promise, lastUsed: Date.now() });
  void promise
    .then(url => {
      const current = contextScreenshotCache.get(contextId);
      if (current?.state === 'loading' && current.promise === promise) {
        contextScreenshotCache.set(contextId, { state: 'ready', url, lastUsed: Date.now(), refCount: 0 });
      } else {
        URL.revokeObjectURL(url);
      }
    })
    .catch(() => {
      const current = contextScreenshotCache.get(contextId);
      if (current?.state === 'loading' && current.promise === promise) {
        contextScreenshotCache.delete(contextId);
      }
    });

  return promise.then(url => retainReadyContextScreenshotUrl(contextId, url));
}

function retainReadyContextScreenshotUrl(contextId: string, url: string): string {
  const cached = contextScreenshotCache.get(contextId);
  if (cached?.state !== 'ready' || cached.url !== url) {
    throw new Error('context screenshot cache entry unavailable');
  }
  cached.lastUsed = Date.now();
  cached.refCount += 1;
  pruneContextScreenshotCache();
  return url;
}

function releaseContextScreenshotUrl(contextId: string) {
  const cached = contextScreenshotCache.get(contextId);
  if (cached?.state !== 'ready') return;
  cached.refCount = Math.max(0, cached.refCount - 1);
  cached.lastUsed = Date.now();
  pruneContextScreenshotCache();
}

function pruneContextScreenshotCache() {
  const readyEntries = [...contextScreenshotCache.entries()]
    .filter((entry): entry is [string, Extract<ContextScreenshotCacheEntry, { state: 'ready' }>] => entry[1].state === 'ready' && entry[1].refCount === 0)
    .sort((a, b) => a[1].lastUsed - b[1].lastUsed);

  while (contextScreenshotCache.size > CONTEXT_SCREENSHOT_CACHE_LIMIT && readyEntries.length > 0) {
    const [id, entry] = readyEntries.shift()!;
    URL.revokeObjectURL(entry.url);
    contextScreenshotCache.delete(id);
  }
}

function reanalysisKey(
  historyType: ContextCaptureEntry['linkedHistoryType'],
  historyId: string,
): string {
  return `${historyType}:${historyId}`;
}

function reanalysisKeyForContext(context: ContextCaptureEntry): string {
  return reanalysisKey(context.linkedHistoryType, context.linkedHistoryId);
}

function isReanalysisActive(state?: ReanalysisUiState): boolean {
  return state?.status === 'busy' || state?.status === 'queued';
}

function contextAnalysisNeedsRefresh(
  context: ContextCaptureEntry | null | undefined,
  state?: ReanalysisUiState,
): boolean {
  return context?.analysis?.status === 'pending' || isReanalysisActive(state);
}

function isFreshReanalysisResult(
  analysis: NonNullable<ContextCaptureEntry['analysis']>,
  state: ReanalysisUiState,
): boolean {
  if (state.previousAnalysisId !== undefined) {
    return state.previousAnalysisId == null || analysis.id !== state.previousAnalysisId;
  }
  const timestamp = analysis.analyzedAt ?? analysis.createdAt;
  const time = Date.parse(timestamp);
  if (!Number.isFinite(time)) return true;
  return time + 1_000 >= state.updatedAt;
}

function reanalysisUiStatusFromAnalysis(
  status: NonNullable<ContextCaptureEntry['analysis']>['status'],
): Extract<ReanalysisUiStatus, 'success' | 'failed' | 'skipped'> {
  if (status === 'success') return 'success';
  if (status === 'skipped') return 'skipped';
  return 'failed';
}

function useFilters(): Array<{ id: 'all' | PolishMode; label: string }> {
  const { t } = useTranslation();
  return [
    { id: 'all', label: t('history.filterAll') },
    { id: 'raw', label: t('style.modes.raw.name') },
    { id: 'light', label: t('style.modes.light.name') },
    { id: 'structured', label: t('style.modes.structured.name') },
    { id: 'formal', label: t('style.modes.formal.name') },
  ];
}

function useModeLabel(): Record<PolishMode, string> {
  const { t } = useTranslation();
  return {
    raw: t('style.modes.raw.name'),
    light: t('style.modes.light.name'),
    structured: t('style.modes.structured.name'),
    formal: t('style.modes.formal.name'),
  };
}

export function History() {
  const { t } = useTranslation();
  const os = detectOS();
  const FILTERS = useFilters();
  const MODE_LABEL = useModeLabel();
  const [historyKind, setHistoryKind] = useState<'voice' | 'rewrite' | 'screenshotRecord'>('voice');
  const [filter, setFilter] = useState<'all' | PolishMode>('all');
  const [query, setQuery] = useState('');
  const [debouncedQuery, setDebouncedQuery] = useState('');
  const [items, setItems] = useState<DictationSession[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [justCopied, setJustCopied] = useState(false);
  const [reanalysisStates, setReanalysisStates] = useState<ReanalysisStateMap>({});
  const refreshSeqRef = useRef(0);
  const loadingSeqRef = useRef(0);
  const refreshInFlightRef = useRef<Promise<DictationSession[]> | null>(null);
  const reanalysisClearTimersRef = useRef<Record<string, number>>({});
  const activeReanalysisKeysRef = useRef<Set<string>>(new Set());
  const [justCopiedRaw, setJustCopiedRaw] = useState(false);
  // 「重新转录」进行中：禁用按钮 + 显示「转录中…」，避免重复点击发起多次 ASR。
  const [retranscribing, setRetranscribing] = useState(false);
  // 录音文件 lazily-detected missing 状态：retention / 条数 cap 清理后磁盘上 wav
  // 可能已被删，但 history 条目 hasAudioRecording 仍写 true。任一组件
  // （播放 / 导出）首次 IPC 拿到 'recording not found' 时把 id 加进来，
  // 之后渲染按钮的条件就转 false，避免反复点击得到同样的 error。
  // 修 pr_agent "Missing file check" 反馈。
  const [audioMissingIds, setAudioMissingIds] = useState<Set<string>>(() => new Set());
  const markAudioMissing = useCallback((id: string) => {
    setAudioMissingIds(prev => {
      if (prev.has(id)) return prev;
      const next = new Set(prev);
      next.add(id);
      return next;
    });
  }, []);
  const { prefs } = useHotkeySettings();
  const mobile = useMobileLayout();
  const [mobileDetailOpen, setMobileDetailOpen] = useState(false);
  const [promptOpen, setPromptOpen] = useState(false);
  const [promptDraft, setPromptDraft] = useState('');

  useEffect(() => {
    if (!promptOpen) return;
    setPromptDraft(prefs?.contextAnalysisFullSummaryPrompt ?? DEFAULT_CONTEXT_ANALYSIS_FULL_SUMMARY_PROMPT);
  }, [promptOpen, prefs?.contextAnalysisFullSummaryPrompt]);

  const savePromptDraft = async () => {
    if (!prefs) return;
    const trimmed = promptDraft.trim();
    if (!trimmed) {
      setActionError(t('history.contextAnalysis.promptEmpty', '提示词不能为空'));
      return;
    }
    try {
      await setSettings({ ...prefs, contextAnalysisFullSummaryPrompt: trimmed });
      setPromptOpen(false);
      setActionError(null);
    } catch (error) {
      setActionError(errorMessage(error));
    }
  };

  const clearReanalysisStateLater = useCallback((key: string) => {
    const existing = reanalysisClearTimersRef.current[key];
    if (existing != null) window.clearTimeout(existing);
    reanalysisClearTimersRef.current[key] = window.setTimeout(() => {
      delete reanalysisClearTimersRef.current[key];
      setReanalysisStates(prev => {
        if (!prev[key] || isReanalysisActive(prev[key])) return prev;
        const next = { ...prev };
        delete next[key];
        return next;
      });
    }, REANALYSIS_STATUS_CLEAR_MS);
  }, []);

  const reconcileReanalysisStates = useCallback((data: DictationSession[]) => {
    setReanalysisStates(prev => {
      let changed = false;
      const next = { ...prev };
      const existingKeys = new Set(data.map(entry => reanalysisKey('voice', entry.id)));

      for (const key of Object.keys(next)) {
        if (!existingKeys.has(key)) {
          delete next[key];
          changed = true;
        }
      }

      for (const entry of data) {
        const key = reanalysisKey('voice', entry.id);
        const state = next[key];
        if (!isReanalysisActive(state)) continue;
        const analysis = entry.contextCapture?.analysis ?? null;
        if (!analysis || analysis.status === 'pending') continue;
        if (!isFreshReanalysisResult(analysis, state)) continue;
        next[key] = {
          status: reanalysisUiStatusFromAnalysis(analysis.status),
          updatedAt: Date.now(),
          errorCode: analysis.errorCode,
        };
        activeReanalysisKeysRef.current.delete(key);
        clearReanalysisStateLater(key);
        changed = true;
      }

      return changed ? next : prev;
    });
  }, [clearReanalysisStateLater]);

  const refresh = useCallback(async (options?: HistoryRefreshOptions) => {
    const silent = Boolean(options?.silent);
    const existingRequest = options?.force ? null : refreshInFlightRef.current;
    const request = existingRequest ?? listHistory();
    if (!existingRequest) {
      refreshInFlightRef.current = request;
      refreshSeqRef.current += 1;
    }
    const seq = refreshSeqRef.current;
    if (!silent) {
      loadingSeqRef.current = seq;
      setLoading(true);
      setLoadError(null);
    }
    try {
      const data = await request;
      if (seq !== refreshSeqRef.current) return;
      setItems(data);
      reconcileReanalysisStates(data);
      if (!silent) {
        setActionError(null);
      }
      setSelectedId(prev => (prev && data.some(s => s.id === prev) ? prev : data[0]?.id ?? null));
    } catch (error) {
      if (seq !== refreshSeqRef.current) return;
      console.error('[history] failed to load history', error);
      if (!silent) {
        setLoadError(errorMessage(error));
      }
    } finally {
      if (refreshInFlightRef.current === request) {
        refreshInFlightRef.current = null;
      }
      if (loadingSeqRef.current === seq && !silent) {
        loadingSeqRef.current = 0;
        setLoading(false);
      }
    }
  }, [reconcileReanalysisStates]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 监听后端 history:updated 事件，新语音记录产生时自动刷新列表
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<string>('history:updated', (event) => {
      if (event.payload === 'voice') {
        void refresh({ silent: true });
      }
    }).then(fn => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [refresh]);

  useEffect(() => {
    return () => {
      Object.values(reanalysisClearTimersRef.current).forEach(timer => window.clearTimeout(timer));
    };
  }, []);

  const searchInputRef = useRef<HTMLInputElement>(null);
  const searchShortcut = os === 'mac' ? '⌘K' : 'Ctrl+K';

  // 搜索词防抖：随输入实时更新 query，300ms 后落到 debouncedQuery 再过滤，
  // 避免每个按键都重算整张列表（与 Marketplace 同模式）。
  useEffect(() => {
    const id = window.setTimeout(() => setDebouncedQuery(query), 300);
    return () => window.clearTimeout(id);
  }, [query]);

  // ⌘K / Ctrl+K 聚焦搜索框（设计稿提示的快捷键）；⌘R / Ctrl+R 刷新历史列表
  // （与浏览器「重新加载」直觉一致）。preventDefault 拦掉 webview 默认的整页
  // reload，改为只重拉 listHistory，避免整个前端重挂载。
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) {
        e.preventDefault();
        searchInputRef.current?.focus();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && (e.key === 'r' || e.key === 'R')) {
        e.preventDefault();
        void refresh();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [refresh]);

  const filtered = useMemo(() => {
    const byMode = filter === 'all' ? items : items.filter(s => s.mode === filter);
    const q = debouncedQuery.trim().toLowerCase();
    if (!q) return byMode;
    // 按原始转写 + 润色后文本匹配关键词，覆盖用户能想起的两种内容。
    return byMode.filter(
      s =>
        s.rawTranscript.toLowerCase().includes(q) ||
        s.finalText.toLowerCase().includes(q),
    );
  }, [items, filter, debouncedQuery]);
  const item = useMemo(
    () => filtered.find(s => s.id === selectedId) || filtered[0],
    [filtered, selectedId],
  );

  const onClear = async () => {
    if (items.length === 0) return;
    if (!confirm(t('history.confirmClear', { count: items.length }))) return;
    setActionError(null);
    try {
      await clearHistory();
      setItems([]);
      setSelectedId(null);
      setReanalysisStates(prev => {
        const next = { ...prev };
        Object.keys(next)
          .filter(key => key.startsWith('voice:'))
          .forEach(key => {
            delete next[key];
            const timer = reanalysisClearTimersRef.current[key];
            if (timer != null) {
              window.clearTimeout(timer);
              delete reanalysisClearTimersRef.current[key];
            }
            activeReanalysisKeysRef.current.delete(key);
          });
        return next;
      });
    } catch (error) {
      console.error('[history] failed to clear history', error);
      setActionError(t('history.clearFailed', { err: errorMessage(error) }));
    }
  };

  const onDelete = async () => {
    if (!item) return;
    const deletedId = item.id;
    setActionError(null);
    try {
      await deleteHistoryEntry(deletedId);
      setItems(prev => prev.filter(s => s.id !== deletedId));
      setSelectedId(current => (current === deletedId ? null : current));
      const key = reanalysisKey('voice', deletedId);
      const timer = reanalysisClearTimersRef.current[key];
      if (timer != null) {
        window.clearTimeout(timer);
        delete reanalysisClearTimersRef.current[key];
      }
      activeReanalysisKeysRef.current.delete(key);
      setReanalysisStates(prev => {
        if (!prev[key]) return prev;
        const next = { ...prev };
        delete next[key];
        return next;
      });
    } catch (error) {
      console.error('[history] failed to delete history entry', error);
      setActionError(t('history.deleteFailed', { err: errorMessage(error) }));
    }
  };

  const onCopy = async () => {
    if (!item) return;
    try {
      if (!navigator.clipboard?.writeText) {
        throw new Error('clipboard unavailable');
      }
      // 润色失败/未产出时 finalText 为空，回退到原文，避免「复制」按钮复制空字符串
      // 导致原文无法从 UI 取回（polish 失败时仍能拿到识别原文）。
      await navigator.clipboard.writeText(item.finalText.trim() ? item.finalText : item.rawTranscript);
      setActionError(null);
      setJustCopied(true);
      window.setTimeout(() => setJustCopied(false), 1500);
    } catch (error) {
      console.error('[history] failed to copy entry', error);
      setActionError(t('history.copyFailed', { err: errorMessage(error) }));
    }
  };

  // 原文（识别结果）单独复制：润色失败或用户只想要未润色文本时使用。
  const onCopyRaw = async () => {
    if (!item) return;
    try {
      if (!navigator.clipboard?.writeText) {
        throw new Error('clipboard unavailable');
      }
      await navigator.clipboard.writeText(item.rawTranscript);
      setActionError(null);
      setJustCopiedRaw(true);
      window.setTimeout(() => setJustCopiedRaw(false), 1500);
    } catch (error) {
      console.error('[history] failed to copy raw transcript', error);
      setActionError(t('history.copyFailed', { err: errorMessage(error) }));
    }
  };

  const onExportAudio = async () => {
    if (!item || !item.hasAudioRecording) return;
    try {
      // Wry/WebKit 中 data URL 的 <a download> 可能不触发保存对话框，后端直接调系统对话框
      if (isTauri) {
        const { invoke } = await import('@tauri-apps/api/core');
        await invoke('export_audio_recording', { sessionId: item.id });
      } else {
        const dataUrl = await readAudioRecording(item.id);
        if (!dataUrl || dataUrl === 'data:audio/wav;base64,') throw new Error('empty recording');
        const a = document.createElement('a');
        a.href = dataUrl;
        a.download = `openless-recording-${item.id}.wav`;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
      }
      setActionError(null);
    } catch (error) {
      console.error('[history] failed to export recording', error);
      const msg = errorMessage(error);
      if (isUserCancelled(msg)) {
        setActionError(null);
        return;
      }
      if (msg === 'recording export failed') {
        setActionError(t('history.exportError'));
        return;
      }
      // wav 已被 retention / 条数 cap 清理：把按钮隐藏，不显示错误（用户没干错事）。
      if (msg.includes('recording not found') || msg.includes('not found')) {
        markAudioMissing(item.id);
        return;
      }
      setActionError(t('history.exportFailed', { err: msg }));
    }
  };

  // 对一条「转录失败 / 没识别到语音」的历史用当前 ASR provider 重新转录（issue #613）。
  // 后端读 recordings/<id>.wav → 重转 → 原地回写该条 rawTranscript/finalText、清 errorCode，
  // 返回整条记录；前端据此局部刷新。失败保留 + 自动重试已让这些条目的录音留得住，这里给
  // 持久失败（重试也没救回来）一个手动重转入口。
  const onRetranscribe = async () => {
    if (!item || !item.hasAudioRecording) return;
    setRetranscribing(true);
    setActionError(null);
    try {
      const updated = await retranscribeRecording(item.id);
      setItems(prev => prev.map(s => (s.id === updated.id ? updated : s)));
    } catch (error) {
      console.error('[history] retranscribe failed', error);
      const msg = errorMessage(error);
      // wav 已被 retention / 条数 cap 清理：隐藏入口，不报错（用户没干错事）。
      if (msg.includes('recording not found') || msg.includes('not found')) {
        markAudioMissing(item.id);
        return;
      }
      setActionError(t('history.retranscribeFailed', { err: msg }));
    } finally {
      setRetranscribing(false);
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}>
      <PageHeader
        kicker={t('history.kicker')}
        title={t('history.title')}
        desc={t('history.desc')}
        right={
          <div style={{ display: 'flex', gap: 8 }}>
            <Btn icon="sparkle" variant="ghost" size="sm" onClick={() => setPromptOpen(true)}>{t('history.contextAnalysis.promptSettings', '截图分析提示词')}</Btn>
            <Btn icon="refresh" variant="ghost" size="sm" onClick={() => void refresh()}>{t('common.refresh')}</Btn>
            <Btn icon="trash" variant="ghost" size="sm" onClick={onClear}>{t('common.clear')}</Btn>
          </div>
        }
      />
      {promptOpen && (
        <ContextAnalysisPromptModal
          value={promptDraft}
          onChange={setPromptDraft}
          onClose={() => setPromptOpen(false)}
          onSave={() => void savePromptDraft()}
          onRestoreDefault={() => setPromptDraft(DEFAULT_CONTEXT_ANALYSIS_FULL_SUMMARY_PROMPT)}
        />
      )}
      <div style={{ display: 'flex', gap: 8, marginBottom: 10 }}>
        {(['voice', 'rewrite', 'screenshotRecord'] as const).map(kind => (
          <button
            key={kind}
            onClick={() => setHistoryKind(kind)}
            style={{
              padding: '5px 14px', fontSize: 12, borderRadius: 6, border: 0,
              fontFamily: 'inherit', cursor: 'pointer', fontWeight: 500,
              background: historyKind === kind ? 'var(--ol-blue)' : 'var(--ol-surface-2)',
              color: historyKind === kind ? '#fff' : 'var(--ol-ink-3)',
            }}
          >
            {historyTabLabel(kind, t)}
          </button>
        ))}
      </div>
      {historyKind === 'screenshotRecord' ? (
        <ScreenshotRecordHistoryView />
      ) : historyKind === 'rewrite' ? (
        <RewriteHistoryView />
      ) : (
      <div style={{ display: 'grid', gridTemplateColumns: mobile ? '1fr' : '300px 1fr', gap: 14, flex: 1, minHeight: 0 }}>
        {( !mobile || !mobileDetailOpen) && (
        <Card padding={0} style={{ display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
          <div style={{ padding: '12px 14px', borderBottom: '0.5px solid var(--ol-line)' }}>
            <div style={{
              display: 'flex', alignItems: 'center', gap: 6,
              padding: '6px 10px', fontSize: 12,
              border: '0.5px solid var(--ol-line-strong)', borderRadius: 8,
              background: 'var(--ol-surface-2)', color: 'var(--ol-ink-3)',
            }}>
              <Icon name="search" size={12} />
              <input
                ref={searchInputRef}
                type="search"
                value={query}
                onChange={e => setQuery(e.target.value)}
                placeholder={t('history.searchPlaceholder', { shortcut: searchShortcut })}
                aria-label={t('history.searchPlaceholder', { shortcut: searchShortcut })}
                style={{
                  flex: 1, minWidth: 0,
                  outline: 'none', border: 0, background: 'transparent',
                  fontSize: 12, color: 'var(--ol-ink-1)', fontFamily: 'inherit',
                }}
              />
            </div>
            <div style={{ marginTop: 6, fontSize: 11, color: 'var(--ol-ink-4)' }}>
              {t('history.summary', { total: items.length, shown: filtered.length })}
            </div>
            <div style={{ display: 'flex', gap: 4, flexWrap: 'wrap', marginTop: 10 }}>
              {FILTERS.map(f => (
                <button
                  key={f.id}
                  onClick={() => setFilter(f.id)}
                  style={{
                    padding: '3px 9px', fontSize: 11, borderRadius: 999,
                    ...chipSelectedStyle(filter === f.id),
                    cursor: 'default', fontFamily: 'inherit', fontWeight: 500,
                    transition: 'background 0.16s var(--ol-motion-quick), color 0.16s var(--ol-motion-quick), border-color 0.16s var(--ol-motion-quick)',
                  }}
                >{f.label}</button>
              ))}
            </div>
          </div>
          <div className="ol-thinscroll" style={{ flex: 1, overflow: 'auto', padding: 6 }}>
            {actionError && (
              <div style={{ margin: 8, padding: '9px 10px', borderRadius: 8, background: 'rgba(239,68,68,0.08)', color: 'var(--ol-red, #ef4444)', fontSize: 12, lineHeight: 1.45 }}>
                {actionError}
              </div>
            )}
            {loading && <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div>}
            {!loading && loadError && (
              <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)', display: 'flex', flexDirection: 'column', alignItems: 'flex-start', gap: 10 }}>
                <span>{t('history.loadFailed', { err: loadError })}</span>
                <Btn size="sm" variant="ghost" onClick={() => void refresh()}>{t('history.retry')}</Btn>
              </div>
            )}
            {!loading && !loadError && filtered.length === 0 && (
              <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)' }}>
                {debouncedQuery.trim()
                  ? t('history.searchNoMatch', { query: debouncedQuery.trim() })
                  : t('history.empty', { trigger: prefs ? formatComboLabel(prefs.dictationHotkey) : '' })}
              </div>
            )}
            {!loadError && filtered.map(s => (
              <button
                key={s.id}
                onClick={() => {
                  setSelectedId(s.id);
                  if (mobile) setMobileDetailOpen(true);
                  if (contextAnalysisNeedsRefresh(
                    s.contextCapture,
                    s.contextCapture ? reanalysisStates[reanalysisKeyForContext(s.contextCapture)] : undefined,
                  )) {
                    void refresh({ silent: true });
                  }
                }}
                style={{
                  width: '100%', padding: '10px 12px', textAlign: 'left',
                  display: 'flex', flexDirection: 'column', gap: 4,
                  border: 0, borderRadius: 8,
                  background: selectedId === s.id ? 'rgba(37,99,235,0.06)' : 'transparent',
                  boxShadow: selectedId === s.id ? 'inset 2px 0 0 var(--ol-blue)' : 'none',
                  cursor: 'default', fontFamily: 'inherit', marginBottom: 1,
                  transition: 'background 0.16s var(--ol-motion-quick), box-shadow 0.18s var(--ol-motion-soft)',
                }}
              >
                <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8 }}>
                  <span style={{ fontSize: 11, fontFamily: 'var(--ol-font-mono)', color: 'var(--ol-ink-3)' }}>
                    {formatTime(s.createdAt)}
                  </span>
                  <span style={{ fontSize: 10, color: 'var(--ol-ink-4)', fontFamily: 'var(--ol-font-mono)' }}>
                    {formatDuration(s.durationMs, t)}
                  </span>
                </div>
                <div style={{ fontSize: 12, color: 'var(--ol-ink-2)', lineHeight: 1.45, display: '-webkit-box', WebkitLineClamp: 2, WebkitBoxOrient: 'vertical', overflow: 'hidden' }}>
                  {s.finalText.split('\n')[0]}
                </div>
                <div><Pill size="sm" tone={s.mode === 'raw' ? 'outline' : 'default'}>{MODE_LABEL[s.mode]}</Pill></div>
              </button>
            ))}
          </div>
        </Card>
        )}

        {(!mobile || mobileDetailOpen) && (
        <Card padding={20} className="ol-thinscroll" style={{ overflow: 'auto' }}>
          {item ? (
            <>
              {mobile && (
                <div style={{ marginBottom: 12 }}>
                  <Btn icon="chevLeft" variant="ghost" size="sm" onClick={() => { setMobileDetailOpen(false); void refresh({ silent: true }); }}>
                    {t('history.backToList')}
                  </Btn>
                </div>
              )}
              <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 14, flexWrap: 'wrap', gap: 8 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
                  <span style={{ fontSize: 13, fontFamily: 'var(--ol-font-mono)', color: 'var(--ol-ink-3)' }}>{formatTime(item.createdAt)}</span>
                  <Pill size="sm" tone="default">{MODE_LABEL[item.mode]}</Pill>
                  {/* 「录音」前缀：与下方识别/润色耗时区分——录音时长发生在松键前，
                      不该与流水线各步耗时加总（用户反馈"时间对不上"）。 */}
                  <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>{t('history.recorded', { duration: formatDuration(item.durationMs, t) })}</span>
                </div>
                <div style={{ display: 'flex', gap: 6 }}>
                  {item.hasAudioRecording && !audioMissingIds.has(item.id) && (
                    <Btn icon="download" variant="ghost" size="sm" onClick={() => void onExportAudio()}>{t('history.exportRecording')}</Btn>
                  )}
                  {item.hasAudioRecording
                    && !audioMissingIds.has(item.id)
                    && (item.errorCode === 'transcribeFailed' || item.errorCode === 'emptyTranscript') && (
                    <Btn icon="refresh" variant="ghost" size="sm" disabled={retranscribing} onClick={() => void onRetranscribe()}>
                      {retranscribing ? t('history.retranscribing') : t('history.retranscribe')}
                    </Btn>
                  )}
                  <Btn icon="trash" variant="ghost" size="sm" onClick={onDelete}>{t('common.delete')}</Btn>
                </div>
              </div>
              {item.hasAudioRecording && !audioMissingIds.has(item.id) && (
                <AudioRecordingPlayer
                  sessionId={item.id}
                  onMissing={() => markAudioMissing(item.id)}
                  key={item.id}
                />
              )}
              <div style={{ display: 'grid', gridTemplateColumns: mobile ? '1fr' : '1fr 1fr', gap: 12 }}>
                <div style={{ padding: 14, border: '0.5px solid var(--ol-line)', borderRadius: 10, background: 'var(--ol-surface-2)' }}>
                  <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8, marginBottom: 10 }}>
                    <Pill size="sm" tone="outline">{t('history.rawLabel')}</Pill>
                    {item.rawTranscript && (
                      <Btn icon={justCopiedRaw ? 'check' : 'copy'} variant="ghost" size="sm" onClick={() => void onCopyRaw()}>
                        {justCopiedRaw ? t('common.copied') : t('common.copy')}
                      </Btn>
                    )}
                  </div>
                  <p style={{ margin: 0, fontSize: 13, lineHeight: 1.7, color: 'var(--ol-ink-2)', whiteSpace: 'pre-wrap' }}>
                    {item.rawTranscript || t('history.rawEmpty')}
                  </p>
                </div>
                <div style={{ padding: 14, border: '0.5px solid var(--ol-blue)', borderRadius: 10, background: 'var(--ol-blue-soft)' }}>
                  <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8, marginBottom: 10 }}>
                    <Pill size="sm" tone="blue">{MODE_LABEL[item.mode]}</Pill>
                    <Btn icon={justCopied ? 'check' : 'copy'} variant="ghost" size="sm" onClick={() => void onCopy()}>
                      {justCopied ? t('common.copied') : t('common.copy')}
                    </Btn>
                  </div>
                  <p style={{ margin: 0, fontSize: 13, lineHeight: 1.7, color: 'var(--ol-ink)', whiteSpace: 'pre-line' }}>
                    {item.finalText}
                  </p>
                </div>
              </div>
              <ContextCapturePanel
                context={item.contextCapture}
                reanalysisState={item.contextCapture ? reanalysisStates[reanalysisKeyForContext(item.contextCapture)] : undefined}
                onReanalyze={context => {
                  const key = reanalysisKeyForContext(context);
                  if (activeReanalysisKeysRef.current.has(key) || isReanalysisActive(reanalysisStates[key])) return;
                  activeReanalysisKeysRef.current.add(key);
                  const previousAnalysisId = context.analysis?.id ?? null;
                  setReanalysisStates(prev => ({
                    ...prev,
                    [key]: { status: 'busy', updatedAt: Date.now(), previousAnalysisId },
                  }));
                  void reanalyzeContextHistory(context.linkedHistoryType, context.linkedHistoryId)
                    .then(() => {
                      setReanalysisStates(prev => ({
                        ...prev,
                        [key]: { status: 'queued', updatedAt: Date.now(), previousAnalysisId },
                      }));
                      void refresh({ silent: true, force: true });
                    })
                    .catch(error => {
                      console.error('[history] context reanalysis failed', error);
                      activeReanalysisKeysRef.current.delete(key);
                      setReanalysisStates(prev => ({
                        ...prev,
                        [key]: {
                          status: 'failed',
                          updatedAt: Date.now(),
                          errorCode: errorMessage(error),
                          previousAnalysisId,
                        },
                      }));
                      clearReanalysisStateLater(key);
                  });
                }}
              />
              {/* 流水线明细：识别 / 润色 / 插入 三步各占一行 —— 左列步骤名、中列
                  provider·model（或插入目标），右列该步耗时/状态。旧历史没有模型与
                  耗时字段时对应行自动隐藏，只剩插入行 = 改版前的信息量。 */}
              <div style={{ marginTop: 18, paddingTop: 14, borderTop: '0.5px solid var(--ol-line-soft)', display: 'grid', gridTemplateColumns: 'auto 1fr auto', columnGap: 14, rowGap: 7, fontSize: 11, color: 'var(--ol-ink-4)', alignItems: 'baseline' }}>
                {(item.asrProvider || item.asrMs != null || item.asrDurationMs != null) && (
                  <>
                    <span style={{ display: 'flex' }}>
                      <Tooltip content={t('history.stepAsrHint')} wrap placement="bottom" focusable>
                        <span style={{ cursor: 'help', textDecoration: 'underline dotted', textDecorationColor: 'var(--ol-ink-4)', textUnderlineOffset: 3 }}>
                          {t('history.stepAsr')}
                        </span>
                      </Tooltip>
                    </span>
                    <span style={{ color: 'var(--ol-ink-2)', fontFamily: 'var(--ol-font-mono)', overflowWrap: 'anywhere' }}>
                      {[item.asrProvider, item.asrModel].filter(Boolean).join(' · ')}
                    </span>
                    <span style={{ fontFamily: 'var(--ol-font-mono)', textAlign: 'right' }}>
                      {(item.asrMs ?? item.asrDurationMs) != null
                        ? formatStepDuration((item.asrMs ?? item.asrDurationMs) as number, t)
                        : ''}
                    </span>
                  </>
                )}
                {(item.llmProvider || item.llmModel || item.polishMs != null || item.polishDurationMs != null) && (
                  <>
                    <span>{t('history.stepPolish')}</span>
                    <span style={{ color: 'var(--ol-ink-2)', fontFamily: 'var(--ol-font-mono)', overflowWrap: 'anywhere' }}>
                      {[item.llmProvider, item.llmModel].filter(Boolean).join(' · ')}
                    </span>
                    <span style={{ fontFamily: 'var(--ol-font-mono)', textAlign: 'right' }}>
                      {(item.polishMs ?? item.polishDurationMs) != null
                        ? formatStepDuration((item.polishMs ?? item.polishDurationMs) as number, t)
                        : ''}
                    </span>
                  </>
                )}
                <span>{t('history.stepInsert')}</span>
                <span style={{ color: 'var(--ol-ink-2)' }}>
                  {item.appName && <><b>{item.appName}</b>{' · '}</>}
                  {t('history.chars', { count: item.finalText.length })}
                  {item.dictionaryEntryCount != null && item.dictionaryEntryCount > 0 && (
                    <>{' · '}{t('history.vocabHits', { count: item.dictionaryEntryCount })}</>
                  )}
                </span>
                <span style={{ textAlign: 'right' }}>{
                  item.insertStatus === 'inserted'
                    ? t('history.inserted')
                    : item.insertStatus === 'pasteSent'
                      ? t('history.pasteSent')
                    : item.insertStatus === 'copiedFallback'
                      ? t('history.copiedFallback', { shortcut: os === 'mac' ? '⌘V' : 'Ctrl+V' })
                      : t('history.insertFailed')
                }</span>
              </div>
            </>
          ) : (
            <div style={{ padding: 40, textAlign: 'center', fontSize: 13, color: 'var(--ol-ink-4)' }}>
              {loading ? t('common.loading') : loadError ? t('history.loadFailed', { err: loadError }) : t('history.selectHint')}
            </div>
          )}
        </Card>
        )}
      </div>
      )}
    </div>
  );
}

function RewriteHistoryView() {
  const { t } = useTranslation();
  const [items, setItems] = useState<RewriteHistoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [reanalysisStates, setReanalysisStates] = useState<ReanalysisStateMap>({});
  const [query, setQuery] = useState('');
  const [debouncedQuery, setDebouncedQuery] = useState('');
  const searchInputRef = useRef<HTMLInputElement>(null);
  const refreshSeqRef = useRef(0);
  const loadingSeqRef = useRef(0);
  const refreshInFlightRef = useRef<Promise<RewriteHistoryEntry[]> | null>(null);
  const reanalysisClearTimersRef = useRef<Record<string, number>>({});
  const activeReanalysisKeysRef = useRef<Set<string>>(new Set());
  const mobile = useMobileLayout();
  const [mobileDetailOpen, setMobileDetailOpen] = useState(false);

  const clearReanalysisStateLater = useCallback((key: string) => {
    const existing = reanalysisClearTimersRef.current[key];
    if (existing != null) window.clearTimeout(existing);
    reanalysisClearTimersRef.current[key] = window.setTimeout(() => {
      delete reanalysisClearTimersRef.current[key];
      setReanalysisStates(prev => {
        if (!prev[key] || isReanalysisActive(prev[key])) return prev;
        const next = { ...prev };
        delete next[key];
        return next;
      });
    }, REANALYSIS_STATUS_CLEAR_MS);
  }, []);

  const reconcileReanalysisStates = useCallback((data: RewriteHistoryEntry[]) => {
    setReanalysisStates(prev => {
      let changed = false;
      const next = { ...prev };
      const existingKeys = new Set(data.map(entry => reanalysisKey('rewrite', entry.id)));

      for (const key of Object.keys(next)) {
        if (!existingKeys.has(key)) {
          delete next[key];
          changed = true;
        }
      }

      for (const entry of data) {
        const key = reanalysisKey('rewrite', entry.id);
        const state = next[key];
        if (!isReanalysisActive(state)) continue;
        const analysis = entry.contextCapture?.analysis ?? null;
        if (!analysis || analysis.status === 'pending') continue;
        if (!isFreshReanalysisResult(analysis, state)) continue;
        next[key] = {
          status: reanalysisUiStatusFromAnalysis(analysis.status),
          updatedAt: Date.now(),
          errorCode: analysis.errorCode,
        };
        activeReanalysisKeysRef.current.delete(key);
        clearReanalysisStateLater(key);
        changed = true;
      }

      return changed ? next : prev;
    });
  }, [clearReanalysisStateLater]);

  const refresh = useCallback(async (options?: HistoryRefreshOptions) => {
    const silent = Boolean(options?.silent);
    const existingRequest = options?.force ? null : refreshInFlightRef.current;
    const request = existingRequest ?? listRewriteHistory();
    if (!existingRequest) {
      refreshInFlightRef.current = request;
      refreshSeqRef.current += 1;
    }
    const seq = refreshSeqRef.current;
    if (!silent) {
      loadingSeqRef.current = seq;
      setLoading(true);
      setError(null);
    }
    try {
      const data = await request;
      if (seq !== refreshSeqRef.current) return;
      setItems(data);
      reconcileReanalysisStates(data);
      setSelectedId(prev => (prev && data.some(e => e.id === prev) ? prev : data[0]?.id ?? null));
    } catch (err) {
      if (seq !== refreshSeqRef.current) return;
      if (!silent) {
        setError(errorMessage(err));
      }
    } finally {
      if (refreshInFlightRef.current === request) {
        refreshInFlightRef.current = null;
      }
      if (loadingSeqRef.current === seq && !silent) {
        loadingSeqRef.current = 0;
        setLoading(false);
      }
    }
  }, [reconcileReanalysisStates]);

  useEffect(() => { void refresh(); }, [refresh]);

  // 监听后端 history:updated 事件，新重写记录产生时自动刷新列表
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<string>('history:updated', (event) => {
      if (event.payload === 'rewrite') {
        void refresh({ silent: true });
      }
    }).then(fn => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [refresh]);

  useEffect(() => {
    return () => {
      Object.values(reanalysisClearTimersRef.current).forEach(timer => window.clearTimeout(timer));
    };
  }, []);

  // 搜索词防抖 300ms
  useEffect(() => {
    const id = window.setTimeout(() => setDebouncedQuery(query), 300);
    return () => window.clearTimeout(id);
  }, [query]);

  // ⌘K / Ctrl+K 聚焦搜索框
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) {
        e.preventDefault();
        searchInputRef.current?.focus();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);

  const filtered = useMemo(() => {
    const q = debouncedQuery.trim().toLowerCase();
    if (!q) return items;
    return items.filter(e =>
      e.sourceText.toLowerCase().includes(q) ||
      e.rewrittenText.toLowerCase().includes(q) ||
      (e.stylePackName ?? '').toLowerCase().includes(q),
    );
  }, [items, debouncedQuery]);

  const selected = filtered.find(e => e.id === selectedId) || filtered[0];

  const onDelete = async (id: string) => {
    try {
      await deleteRewriteHistoryEntry(id);
      setItems(prev => prev.filter(e => e.id !== id));
      setSelectedId(current => (current === id ? null : current));
      const key = reanalysisKey('rewrite', id);
      const timer = reanalysisClearTimersRef.current[key];
      if (timer != null) {
        window.clearTimeout(timer);
        delete reanalysisClearTimersRef.current[key];
      }
      activeReanalysisKeysRef.current.delete(key);
      setReanalysisStates(prev => {
        if (!prev[key]) return prev;
        const next = { ...prev };
        delete next[key];
        return next;
      });
    } catch (err) {
      console.error('[rewrite-history] delete failed', err);
    }
  };

  const onClear = async () => {
    if (items.length === 0) return;
    if (!confirm(t('history.confirmClear', { count: items.length }))) return;
    try {
      await clearRewriteHistory();
      setItems([]);
      setSelectedId(null);
      setReanalysisStates(prev => {
        const next = { ...prev };
        Object.keys(next)
          .filter(key => key.startsWith('rewrite:'))
          .forEach(key => {
            delete next[key];
            const timer = reanalysisClearTimersRef.current[key];
            if (timer != null) {
              window.clearTimeout(timer);
              delete reanalysisClearTimersRef.current[key];
            }
            activeReanalysisKeysRef.current.delete(key);
          });
        return next;
      });
    } catch (err) {
      console.error('[rewrite-history] clear failed', err);
    }
  };

  const onCopy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch (err) {
      console.error('[rewrite-history] copy failed', err);
    }
  };

  const insertStatusLabel = (status: string): string => {
    switch (status) {
      case 'inserted': return t('rewrite.inserted', '已替换');
      case 'pasteSent': return t('rewrite.pasteSent', '已粘贴');
      case 'copiedFallback': return t('rewrite.copiedFallback', '已复制');
      case 'failed': return t('rewrite.failed', '失败');
      default: return status;
    }
  };

  if (loading) {
    return <Card><div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div></Card>;
  }
  if (error) {
    return <Card><div style={{ fontSize: 12, color: 'var(--ol-err)' }}>{t('history.loadFailed', { err: error })}</div></Card>;
  }
  if (items.length === 0) {
    return <Card><div style={{ padding: 40, textAlign: 'center', fontSize: 13, color: 'var(--ol-ink-4)' }}>{t('rewrite.emptyHistory', '暂无重写历史')}</div></Card>;
  }

  return (
    <div style={{ display: 'grid', gridTemplateColumns: mobile ? '1fr' : '300px 1fr', gap: 14, flex: 1, minHeight: 0 }}>
      {(!mobile || !mobileDetailOpen) && (
      <Card padding={0} style={{ display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
        <div style={{ padding: '12px 14px', borderBottom: '0.5px solid var(--ol-line)' }}>
          <div style={{
            display: 'flex', alignItems: 'center', gap: 6,
            padding: '6px 10px', fontSize: 12,
            border: '0.5px solid var(--ol-line-strong)', borderRadius: 8,
            background: 'var(--ol-surface-2)', color: 'var(--ol-ink-3)',
          }}>
            <Icon name="search" size={12} />
            <input
              ref={searchInputRef}
              type="search"
              value={query}
              onChange={e => setQuery(e.target.value)}
              placeholder={t('history.searchPlaceholder', { shortcut: '⌘K' })}
              aria-label={t('history.searchPlaceholder', { shortcut: '⌘K' })}
              style={{
                flex: 1, minWidth: 0,
                outline: 'none', border: 0, background: 'transparent',
                fontSize: 12, color: 'var(--ol-ink-1)', fontFamily: 'inherit',
              }}
            />
          </div>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginTop: 6 }}>
            <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>
              {t('history.summary', { total: items.length, shown: filtered.length })}
            </span>
            <Btn icon="trash" variant="ghost" size="sm" onClick={onClear}>{t('common.clear')}</Btn>
          </div>
        </div>
        <div className="ol-thinscroll" style={{ overflowY: 'auto', flex: 1 }}>
          {filtered.length === 0 && (
            <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)', textAlign: 'center' }}>
              {debouncedQuery.trim()
                ? t('history.searchNoMatch', { query: debouncedQuery.trim() })
                : t('rewrite.emptyHistory', '暂无重写历史')}
            </div>
          )}
          {filtered.map(entry => (
            <button
              key={entry.id}
              onClick={() => {
                setSelectedId(entry.id);
                if (mobile) setMobileDetailOpen(true);
                if (contextAnalysisNeedsRefresh(
                  entry.contextCapture,
                  entry.contextCapture ? reanalysisStates[reanalysisKeyForContext(entry.contextCapture)] : undefined,
                )) {
                  void refresh({ silent: true });
                }
              }}
              style={{
                width: '100%', textAlign: 'left', padding: '10px 14px',
                border: 0, borderBottom: '0.5px solid var(--ol-line)',
                background: entry.id === selected?.id ? 'rgba(37,99,235,0.06)' : 'transparent',
                boxShadow: entry.id === selected?.id ? 'inset 2px 0 0 var(--ol-blue)' : 'none',
                cursor: 'default', fontFamily: 'inherit',
                transition: 'background 0.16s var(--ol-motion-quick), box-shadow 0.18s var(--ol-motion-soft)',
              }}
            >
              <div style={{ fontSize: 12, color: 'var(--ol-ink-2)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {entry.sourceText || '(空)'}
              </div>
              <div style={{ display: 'flex', gap: 6, marginTop: 4, alignItems: 'center', flexWrap: 'wrap' }}>
                <span style={{ fontSize: 10, color: 'var(--ol-ink-4)' }}>{formatTime(entry.createdAt)}</span>
                {entry.stylePackName && (
                  <Pill size="sm" tone="blue">{entry.stylePackName}</Pill>
                )}
                <Pill size="sm">{insertStatusLabel(entry.insertStatus)}</Pill>
                {entry.errorCode && (
                  <span style={{ fontSize: 10, color: 'var(--ol-err)' }}>{entry.errorCode}</span>
                )}
              </div>
            </button>
          ))}
        </div>
      </Card>
      )}
      {(!mobile || mobileDetailOpen) && selected && (
        <Card className="ol-thinscroll" style={{ overflowY: 'auto', padding: 20 }}>
          {mobile && (
            <div style={{ marginBottom: 12 }}>
              <Btn icon="chevLeft" variant="ghost" size="sm" onClick={() => { setMobileDetailOpen(false); void refresh({ silent: true }); }}>
                {t('history.backToList')}
              </Btn>
            </div>
          )}
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 14, flexWrap: 'wrap', gap: 8 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
              <span style={{ fontSize: 13, fontFamily: 'var(--ol-font-mono)', color: 'var(--ol-ink-3)' }}>{formatTime(selected.createdAt)}</span>
              {selected.stylePackName && (
                <Pill size="sm" tone="blue">{selected.stylePackName}</Pill>
              )}
              <Pill size="sm">{insertStatusLabel(selected.insertStatus)}</Pill>
            </div>
            <div style={{ display: 'flex', gap: 6 }}>
              <Btn size="sm" icon="copy" variant="ghost" onClick={() => void onCopy(selected.rewrittenText)}>{t('rewrite.copyResult', '复制结果')}</Btn>
              <Btn size="sm" icon="copy" variant="ghost" onClick={() => void onCopy(selected.sourceText)}>{t('rewrite.copySource', '复制原文')}</Btn>
              <Btn size="sm" icon="trash" variant="ghost" onClick={() => void onDelete(selected.id)}>{t('common.delete')}</Btn>
            </div>
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
            <div style={{ padding: 14, border: '0.5px solid var(--ol-line)', borderRadius: 10, background: 'var(--ol-surface-2)' }}>
              <Pill size="sm" tone="outline" style={{ marginBottom: 10 }}>{t('rewrite.sourceText', '原文')}</Pill>
              <p style={{ margin: 0, fontSize: 13, lineHeight: 1.7, color: 'var(--ol-ink-2)', whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>
                {selected.sourceText || '(空)'}
              </p>
            </div>
            <div style={{ padding: 14, border: '0.5px solid var(--ol-blue)', borderRadius: 10, background: 'var(--ol-blue-soft)' }}>
              <Pill size="sm" tone="blue" style={{ marginBottom: 10 }}>{t('rewrite.resultText', '重写结果')}</Pill>
              <p style={{ margin: 0, fontSize: 13, lineHeight: 1.7, color: 'var(--ol-ink)', whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>
                {selected.rewrittenText || '(空)'}
              </p>
            </div>
          </div>
          <ContextCapturePanel
            context={selected.contextCapture ?? null}
            reanalysisState={selected.contextCapture ? reanalysisStates[reanalysisKeyForContext(selected.contextCapture)] : undefined}
            onReanalyze={context => {
              const key = reanalysisKeyForContext(context);
              if (activeReanalysisKeysRef.current.has(key) || isReanalysisActive(reanalysisStates[key])) return;
              activeReanalysisKeysRef.current.add(key);
              const previousAnalysisId = context.analysis?.id ?? null;
              setReanalysisStates(prev => ({
                ...prev,
                [key]: { status: 'busy', updatedAt: Date.now(), previousAnalysisId },
              }));
              void reanalyzeContextHistory(context.linkedHistoryType, context.linkedHistoryId)
                .then(() => {
                  setReanalysisStates(prev => ({
                    ...prev,
                    [key]: { status: 'queued', updatedAt: Date.now(), previousAnalysisId },
                  }));
                  void refresh({ silent: true, force: true });
                })
                .catch(error => {
                  console.error('[history] context reanalysis failed', error);
                  activeReanalysisKeysRef.current.delete(key);
                  setReanalysisStates(prev => ({
                    ...prev,
                    [key]: {
                      status: 'failed',
                      updatedAt: Date.now(),
                      errorCode: errorMessage(error),
                      previousAnalysisId,
                    },
                  }));
                  clearReanalysisStateLater(key);
                });
            }}
          />
          <div style={{ marginTop: 18, paddingTop: 14, borderTop: '0.5px solid var(--ol-line-soft)', display: 'flex', gap: 18, fontSize: 11, color: 'var(--ol-ink-4)', flexWrap: 'wrap' }}>
            {selected.appName && <span>{t('rewrite.sourceApp', '来源应用')}: <b style={{ color: 'var(--ol-ink-2)' }}>{selected.appName}</b></span>}
            {selected.stylePackName && <span>{t('rewrite.stylePack', '风格包')}: <b style={{ color: 'var(--ol-ink-2)' }}>{selected.stylePackName}</b></span>}
            {selected.durationMs != null && selected.durationMs > 0 && (
              <span>{t('common.durationSeconds', { value: (selected.durationMs / 1000).toFixed(1) })}</span>
            )}
            {selected.errorCode && <span style={{ color: 'var(--ol-err)' }}>{selected.errorCode}</span>}
          </div>
        </Card>
      )}
      {(!mobile || !mobileDetailOpen) && !selected && (
        <Card style={{ display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
          <div style={{ padding: 40, textAlign: 'center', fontSize: 13, color: 'var(--ol-ink-4)' }}>
            {t('history.selectHint')}
          </div>
        </Card>
      )}
    </div>
  );
}

function ScreenshotRecordHistoryView() {
  const { t } = useTranslation();
  const mobile = useMobileLayout();
  const [items, setItems] = useState<ScreenshotRecord[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [mobileDetailOpen, setMobileDetailOpen] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [aggBuckets, setAggBuckets] = useState<{ processName: string; appDisplayName: string | null; screenshotCount: number }[]>([]);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await listScreenshotRecords();
      setItems(data);
      setSelectedId(prev => (prev && data.some(entry => entry.id === prev) ? prev : data[0]?.id ?? null));
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 监听后端 history:updated 事件，新截图记录产生时自动刷新列表
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<string>('history:updated', (event) => {
      if (event.payload === 'screenshot') {
        void refresh();
      }
    }).then(fn => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [refresh]);

  // 轮询待聚合状态（5 秒间隔） + 监听后端 aggregation:updated 事件即时刷新
  const pollAggregation = useCallback(async () => {
    try {
      const status = await getScreenshotAggregationStatus();
      setAggBuckets(status.buckets.map(b => ({
        processName: b.processName,
        appDisplayName: b.appDisplayName,
        screenshotCount: b.screenshotCount,
      })));
    } catch {
      // ignore
    }
  }, []);

  useEffect(() => {
    void pollAggregation();
    const id = window.setInterval(pollAggregation, 5000);
    return () => window.clearInterval(id);
  }, [pollAggregation]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<unknown>('aggregation:updated', () => {
      void pollAggregation();
    }).then(fn => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [pollAggregation]);

  const selected = items.find(entry => entry.id === selectedId) ?? items[0] ?? null;

  const onClear = async () => {
    if (items.length === 0) return;
    if (!confirm(t('history.confirmClear', { count: items.length }))) return;
    try {
      await clearScreenshotRecords();
      setItems([]);
      setSelectedId(null);
      setActionError(null);
    } catch (err) {
      setActionError(errorMessage(err));
    }
  };

  const onDelete = async (id: string) => {
    try {
      await deleteScreenshotRecord(id);
      setItems(prev => prev.filter(entry => entry.id !== id));
      setSelectedId(prev => (prev === id ? null : prev));
      setActionError(null);
    } catch (err) {
      setActionError(errorMessage(err));
    }
  };

  const onReanalyze = async (id: string) => {
    setBusyId(id);
    try {
      await reanalyzeScreenshotRecord(id);
      await refresh();
      setActionError(null);
    } catch (err) {
      setActionError(errorMessage(err));
    } finally {
      setBusyId(null);
    }
  };

  if (loading) {
    return <Card><div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div></Card>;
  }
  if (error) {
    return <Card><div style={{ fontSize: 12, color: 'var(--ol-err)' }}>{t('history.loadFailed', { err: error })}</div></Card>;
  }
  // 待聚合状态条（只在有待聚合桶时展示）
  const aggStatusBar = aggBuckets.length > 0 ? (
    <div style={{ padding: '8px 12px', marginBottom: 10, borderRadius: 8, background: 'rgba(59,130,246,0.06)', fontSize: 12, color: 'var(--ol-ink-3)' }}>
      <span style={{ fontWeight: 500 }}>{t('history.screenshotAggregation.pending', '待聚合')}：</span>
      {aggBuckets.map((b, i) => (
        <span key={i}>
          {b.appDisplayName ?? b.processName} {b.screenshotCount} {t('history.screenshotAggregation.count', '张')}
          {i < aggBuckets.length - 1 ? '，' : ''}
        </span>
      ))}
    </div>
  ) : null;

  if (items.length === 0) {
    return (
      <>
        {aggStatusBar}
        <Card>
          <div style={{ padding: 40, textAlign: 'center', fontSize: 13, color: 'var(--ol-ink-4)' }}>
            {t('history.screenshotRecord.empty', '暂无截图记录')}
          </div>
        </Card>
      </>
    );
  }

  return (
    <>
      {aggStatusBar}
      <div style={{ display: 'grid', gridTemplateColumns: mobile ? '1fr' : '300px 1fr', gap: 14, flex: 1, minHeight: 0 }}>
      {(!mobile || !mobileDetailOpen) && (
        <Card padding={0} style={{ display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
          <div style={{ padding: '12px 14px', borderBottom: '0.5px solid var(--ol-line)', display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 10 }}>
            <div style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>
              {t('history.summary', { total: items.length, shown: items.length })}
            </div>
            <Btn icon="trash" variant="ghost" size="sm" onClick={onClear}>{t('common.clear')}</Btn>
          </div>
          {actionError && (
            <div style={{ margin: 8, padding: '9px 10px', borderRadius: 8, background: 'rgba(239,68,68,0.08)', color: 'var(--ol-red, #ef4444)', fontSize: 12, lineHeight: 1.45 }}>
              {actionError}
            </div>
          )}
          <div className="ol-thinscroll" style={{ overflowY: 'auto', flex: 1 }}>
            {items.map(entry => (
              <button
                key={entry.id}
                onClick={() => {
                  setSelectedId(entry.id);
                  if (mobile) setMobileDetailOpen(true);
                }}
                style={{
                  width: '100%',
                  textAlign: 'left',
                  padding: '10px 14px',
                  border: 0,
                  borderBottom: '0.5px solid var(--ol-line)',
                  background: entry.id === selected?.id ? 'rgba(37,99,235,0.06)' : 'transparent',
                  boxShadow: entry.id === selected?.id ? 'inset 2px 0 0 var(--ol-blue)' : 'none',
                  cursor: 'default',
                  fontFamily: 'inherit',
                }}
              >
                <div style={{ fontSize: 12, color: 'var(--ol-ink-2)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {entry.conversationWindow || entry.windowTitle || entry.contextApp || entry.processName || t('history.screenshotRecord.unknownWindow', '未知窗口')}
                </div>
                <div style={{ display: 'flex', gap: 6, marginTop: 4, alignItems: 'center', flexWrap: 'wrap' }}>
                  <span style={{ fontSize: 10, color: 'var(--ol-ink-4)' }}>{formatTime(entry.createdAt)}</span>
                  <Pill size="sm" tone={entry.status === 'success' ? 'blue' : 'outline'}>{screenshotRecordStatusLabel(entry.status, t)}</Pill>
                  <span style={{ fontSize: 10, color: 'var(--ol-ink-4)' }}>
                    {t('history.screenshotRecord.count', '{{count}} 张截图', { count: entry.screenshotIds.length })}
                  </span>
                </div>
              </button>
            ))}
          </div>
        </Card>
      )}

      {(!mobile || mobileDetailOpen) && selected && (
        <Card className="ol-thinscroll" style={{ overflowY: 'auto', padding: 20 }}>
          {mobile && (
            <div style={{ marginBottom: 12 }}>
              <Btn icon="chevLeft" variant="ghost" size="sm" onClick={() => { setMobileDetailOpen(false); void refresh(); }}>
                {t('history.backToList')}
              </Btn>
            </div>
          )}
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 14, flexWrap: 'wrap', gap: 8 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
              <span style={{ fontSize: 13, fontFamily: 'var(--ol-font-mono)', color: 'var(--ol-ink-3)' }}>{formatTime(selected.createdAt)}</span>
              <Pill size="sm" tone={selected.status === 'success' ? 'blue' : 'outline'}>{screenshotRecordStatusLabel(selected.status, t)}</Pill>
              <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>
                {t('history.screenshotRecord.triggerCount', '触发 {{count}} 次', { count: selected.triggerCount })}
              </span>
            </div>
            <div style={{ display: 'flex', gap: 6 }}>
              <Btn icon="refresh" variant="ghost" size="sm" disabled={busyId === selected.id || selected.status === 'analyzing' || selected.status === 'collecting' || selected.status === 'queued'} onClick={() => void onReanalyze(selected.id)}>
                {busyId === selected.id || selected.status === 'queued' || selected.status === 'analyzing' ? t('history.contextAnalysis.reanalyzing', '分析中') : t('history.contextAnalysis.reanalyze', '重新分析')}
              </Btn>
              <Btn icon="trash" variant="ghost" size="sm" onClick={() => void onDelete(selected.id)}>{t('common.delete')}</Btn>
            </div>
          </div>
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(140px, 1fr))', gap: 10, marginBottom: 12 }}>
            <ContextMeta label={t('history.contextCapture.app', '获取应用')} value={selected.contextApp || selected.processName || null} />
            <ContextMeta label={t('history.contextCapture.window', '对话窗口')} value={selected.conversationWindow || selected.windowTitle || selected.contextApp || selected.processName || null} />
            <ContextMeta label={t('history.contextAnalysis.status', '分析状态')} value={screenshotRecordStatusLabel(selected.status, t)} />
            <ContextMeta label={t('history.contextAnalysis.duration', '分析耗时')} value={selected.analysis ? formatContextAnalysisDuration(selected.analysis) : null} />
            <ContextMeta label={t('history.screenshotRecord.submitted', '提交截图')} value={`${selected.submittedScreenshotIds.length}/${selected.screenshotIds.length}`} />
          </div>
          {selected.windowTitle && (
            <div style={{ marginBottom: 12, fontSize: 11, color: 'var(--ol-ink-4)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={selected.windowTitle}>
              {selected.windowTitle}
            </div>
          )}
          {selected.errorCode && (
            <div style={{ marginBottom: 12, fontSize: 12, color: 'var(--ol-err)' }}>
              {contextAnalysisStatusLabel(selected.analysis?.status ?? 'failed', selected.errorCode, t)}
              {selected.errorMessage ? `：${selected.errorMessage}` : ''}
            </div>
          )}
          {selected.analysis && (
            <div style={{ marginBottom: 14, padding: 14, border: '0.5px solid var(--ol-line)', borderRadius: 10, background: 'var(--ol-surface-2)' }}>
              <Pill size="sm" tone="blue" style={{ marginBottom: 10 }}>{t('history.contextAnalysis.title', 'AI 上下文分析')}</Pill>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(140px, 1fr))', gap: 10, marginBottom: 10 }}>
                <ContextMeta label={t('history.contextAnalysis.conversationName', '对话名称')} value={selected.analysis.conversationName} />
                <ContextMeta label={t('history.contextAnalysis.topic', '主题')} value={selected.analysis.topic} />
                <ContextMeta label={t('history.contextAnalysis.workStatus', '工作状态')} value={contextAnalysisWorkStatusLabel(selected.analysis.workStatus, t)} />
                <ContextMeta label={t('history.contextAnalysis.evidenceLevel', '证据强度')} value={contextAnalysisEvidenceLevelLabel(selected.analysis.evidenceLevel, t)} />
                <ContextMeta label={t('history.contextAnalysis.confidence', '置信度')} value={formatConfidence(selected.analysis.confidence)} />
              </div>
              {selected.analysis.briefSummary && (
                <div style={{ marginBottom: 8 }}>
                  <div style={{ fontSize: 10, color: 'var(--ol-ink-4)', marginBottom: 4 }}>{t('history.contextAnalysis.briefSummary', '简要摘要')}</div>
                  <div style={{ fontSize: 12, color: 'var(--ol-ink-2)', lineHeight: 1.6 }}>{selected.analysis.briefSummary}</div>
                </div>
              )}
              <details style={{ fontSize: 12, color: 'var(--ol-ink-3)' }}>
                <summary style={{ cursor: 'default', color: 'var(--ol-ink-3)', marginBottom: 8 }}>
                  {t('history.contextAnalysis.details', '完整分析')}
                </summary>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 8, lineHeight: 1.6 }}>
                  {selected.analysis.fullSummary && <AnalysisBlock label={t('history.contextAnalysis.fullSummary', '完整摘要')} value={selected.analysis.fullSummary} />}
                  {selected.analysis.userIntent && <AnalysisBlock label={t('history.contextAnalysis.userIntent', '用户意图')} value={selected.analysis.userIntent} />}
                  {selected.analysis.decision && <AnalysisBlock label={t('history.contextAnalysis.decision', '决策/结论')} value={selected.analysis.decision} />}
                  {selected.analysis.actionItems.length > 0 && (
                    <AnalysisBlock
                      label={t('history.contextAnalysis.actionItems', '待办事项')}
                      value={selected.analysis.actionItems.map(item => `${item.text}${item.owner ? `（${item.owner}）` : ''}`).join('\n')}
                    />
                  )}
                  {selected.analysis.relatedPeople.length > 0 && <AnalysisBlock label={t('history.contextAnalysis.relatedPeople', '相关人员')} value={selected.analysis.relatedPeople.join('、')} />}
                  {selected.analysis.projectOrDomain && <AnalysisBlock label={t('history.contextAnalysis.projectOrDomain', '项目/领域')} value={selected.analysis.projectOrDomain} />}
                  {selected.analysis.visualEvidence.length > 0 && <AnalysisBlock label={t('history.contextAnalysis.visualEvidence', '视觉依据')} value={selected.analysis.visualEvidence.join('\n')} />}
                  <AnalysisBlock label={t('history.contextAnalysis.contextType', '上下文类型')} value={selected.analysis.detectedContextType} />
                  <AnalysisBlock label={t('history.contextAnalysis.activityType', '活动类型')} value={selected.analysis.activityType} />
                  {contextAnalysisWorkStatusLabel(selected.analysis.workStatus, t) && (
                    <AnalysisBlock label={t('history.contextAnalysis.workStatus', '工作状态')} value={contextAnalysisWorkStatusLabel(selected.analysis.workStatus, t)!} />
                  )}
                  {contextAnalysisEvidenceLevelLabel(selected.analysis.evidenceLevel, t) && (
                    <AnalysisBlock label={t('history.contextAnalysis.evidenceLevel', '证据强度')} value={contextAnalysisEvidenceLevelLabel(selected.analysis.evidenceLevel, t)!} />
                  )}
                  <AnalysisBlock label={t('history.contextAnalysis.sensitive', '敏感信息')} value={selected.analysis.sensitiveContentVisible ? t('common.yes', '是') : t('common.no', '否')} />
                  {selected.analysis.uncertaintyReason && <AnalysisBlock label={t('history.contextAnalysis.uncertaintyReason', '不确定原因')} value={selected.analysis.uncertaintyReason} />}
                  {selected.analysis.model && <AnalysisBlock label={t('history.contextAnalysis.model', '模型')} value={selected.analysis.model} />}
                  <AnalysisBlock label={t('history.contextAnalysis.promptVersion', '提示词版本')} value={selected.analysis.promptVersion} />
                </div>
              </details>
            </div>
          )}
          <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
            {selected.screenshotIds.map(contextId => (
              <ScreenshotImagePanel key={contextId} contextId={contextId} windowTitle={selected.windowTitle} />
            ))}
          </div>
        </Card>
      )}
      </div>
    </>
  );
}

function errorMessage(error: unknown): string {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

function historyTabLabel(
  kind: 'voice' | 'rewrite' | 'screenshotRecord',
  t: ReturnType<typeof useTranslation>['t'],
): string {
  if (kind === 'voice') return t('history.tabs.voice', '语音历史');
  if (kind === 'rewrite') return t('history.tabs.rewrite', '重写历史');
  return t('history.tabs.screenshotRecord', '截图记录');
}

function ContextAnalysisPromptModal({
  value,
  onChange,
  onClose,
  onSave,
  onRestoreDefault,
}: {
  value: string;
  onChange: (value: string) => void;
  onClose: () => void;
  onSave: () => void;
  onRestoreDefault: () => void;
}) {
  const { t } = useTranslation();

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={t('history.contextAnalysis.promptSettings', '截图分析提示词')}
      style={{
        position: 'fixed',
        inset: 0,
        zIndex: 1900,
        display: 'grid',
        placeItems: 'center',
        padding: 24,
        background: 'rgba(9,12,18,0.34)',
        backdropFilter: 'blur(4px)',
      }}
      onClick={onClose}
    >
      <div
        onClick={event => event.stopPropagation()}
        style={{
          width: 'min(720px, 100%)',
          maxHeight: 'min(720px, 100%)',
          display: 'flex',
          flexDirection: 'column',
          borderRadius: 10,
          background: 'var(--ol-surface)',
          border: '0.5px solid var(--ol-line-strong)',
          boxShadow: '0 18px 64px rgba(0,0,0,0.22)',
          overflow: 'hidden',
        }}
      >
        <div style={{ padding: '16px 18px', borderBottom: '0.5px solid var(--ol-line)' }}>
          <div style={{ fontSize: 15, fontWeight: 600, color: 'var(--ol-ink)' }}>
            {t('history.contextAnalysis.promptSettings', '截图分析提示词')}
          </div>
          <div style={{ marginTop: 6, fontSize: 12, lineHeight: 1.55, color: 'var(--ol-ink-4)' }}>
            {t(
              'history.contextAnalysis.promptSettingsDesc',
              '用于控制语音历史、重写历史、截图记录的完整摘要生成方式。完整摘要会作为后续日报、周报、月报的主要输入，请保持可复盘、可汇总，并避免把截图记录误写成已完成工作。',
            )}
          </div>
        </div>
        <div style={{ padding: 18, minHeight: 0, display: 'flex', flexDirection: 'column', gap: 10 }}>
          <textarea
            value={value}
            onChange={event => onChange(event.target.value)}
            style={{
              width: '100%',
              minHeight: 320,
              resize: 'vertical',
              borderRadius: 8,
              border: '0.5px solid var(--ol-line-strong)',
              background: 'var(--ol-surface-2)',
              color: 'var(--ol-ink-1)',
              fontSize: 12,
              lineHeight: 1.6,
              fontFamily: 'var(--ol-font-mono)',
              padding: 12,
              outline: 'none',
            }}
          />
          <div style={{ fontSize: 11, color: 'var(--ol-ink-4)', lineHeight: 1.5 }}>
            {t('history.contextAnalysis.promptSettingsHint', '保存后只影响新分析和手动重新分析，不会自动重跑旧历史。')}
          </div>
        </div>
        <div style={{ padding: '12px 18px', borderTop: '0.5px solid var(--ol-line)', display: 'flex', justifyContent: 'space-between', gap: 10 }}>
          <Btn variant="ghost" size="sm" onClick={onRestoreDefault}>{t('common.restoreDefault', '恢复默认')}</Btn>
          <div style={{ display: 'flex', gap: 8 }}>
            <Btn variant="ghost" size="sm" onClick={onClose}>{t('common.cancel')}</Btn>
            <Btn variant="primary" size="sm" onClick={onSave}>{t('common.save')}</Btn>
          </div>
        </div>
      </div>
    </div>
  );
}

function isUserCancelled(message: string): boolean {
  const normalized = message.trim().toLowerCase();
  return normalized === 'cancelled'
    || normalized === 'canceled'
    || normalized === 'user cancelled'
    || normalized === 'user canceled';
}

/** 当 session.hasAudioRecording 为 true 时渲染：一个加载按钮 + 拿到字节后切换为
 *  原生 audio controls。Blob URL 在组件 unmount 时 revoke，避免泄漏。
 *  `onMissing` 在后端返回 'recording not found'（wav 已被 prune）时触发，让父组件
 *  把按钮永久隐藏，避免用户继续点击得到同样错误。 */
function ContextCapturePanel({
  context,
  reanalysisState,
  onReanalyze,
}: {
  context?: ContextCaptureEntry | null;
  reanalysisState?: ReanalysisUiState;
  onReanalyze: (context: ContextCaptureEntry) => void;
}) {
  const { t } = useTranslation();
  const { prefs } = useHotkeySettings();
  const [url, setUrl] = useState<string | null>(null);
  const [status, setStatus] = useState<'idle' | 'loading' | 'ready' | 'missing'>('idle');
  const [previewOpen, setPreviewOpen] = useState(false);

  useEffect(() => {
    setPreviewOpen(false);
    if (!context?.screenshotRef) {
      setUrl(null);
      setStatus('missing');
      return;
    }

    const contextId = context.id;
    const cachedUrl = retainCachedContextScreenshotUrl(contextId);
    if (cachedUrl) {
      setUrl(cachedUrl);
      setStatus('ready');
      return () => releaseContextScreenshotUrl(contextId);
    }

    setUrl(null);
    setStatus('loading');
    let cancelled = false;
    let retained = false;
    void retainContextScreenshotUrl(contextId)
      .then(objectUrl => {
        retained = true;
        if (cancelled) {
          releaseContextScreenshotUrl(contextId);
          return;
        }
        setUrl(objectUrl);
        setStatus('ready');
      })
      .catch(error => {
        console.warn('[history] context screenshot unavailable', error);
        if (!cancelled) setStatus('missing');
      });
    return () => {
      cancelled = true;
      if (retained) releaseContextScreenshotUrl(contextId);
    };
  }, [context?.id, context?.screenshotRef]);

  useEffect(() => {
    if (!previewOpen) return undefined;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        setPreviewOpen(false);
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [previewOpen]);

  if (!context) {
    return null;
  }

  const reanalysisEnabled = Boolean(prefs?.contextVisionAnalysisEnabled && prefs.contextVisionAnalysisConsentAccepted);

  return (
    <div style={{ marginTop: 12, padding: 14, border: '0.5px solid var(--ol-line)', borderRadius: 10, background: 'var(--ol-surface-2)' }}>
      <Pill size="sm" tone="outline" style={{ marginBottom: 10 }}>{t('history.contextCapture.title', '上下文采集')}</Pill>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(140px, 1fr))', gap: 10, marginBottom: 12 }}>
        <ContextMeta label={t('history.contextCapture.app', '获取应用')} value={context.contextApp} />
        <ContextMeta label={t('history.contextCapture.window', '对话窗口')} value={context.conversationWindow} />
        <ContextMeta label={t('history.contextCapture.status', '采集状态')} value={contextCaptureStatusLabel(context.captureStatus, t)} />
        <ContextMeta label={t('history.contextCapture.source', '截图来源')} value={contextCaptureSourceLabel(context.captureSource, t)} />
      </div>
      {context.windowTitle && (
        <div style={{ marginBottom: 12, fontSize: 11, color: 'var(--ol-ink-4)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={context.windowTitle}>
          {context.windowTitle}
        </div>
      )}
      {status === 'ready' && url ? (
        <>
          <button
            type="button"
            onClick={() => setPreviewOpen(true)}
            style={{
              display: 'block',
              width: '100%',
              padding: 0,
              border: 0,
              background: 'transparent',
              cursor: 'zoom-in',
              fontFamily: 'inherit',
            }}
            aria-label={t('history.contextCapture.preview', '预览截图')}
          >
            <img
              src={url}
              alt={t('history.contextCapture.screenshotAlt', '上下文截图')}
              style={{ display: 'block', width: '100%', maxHeight: 360, objectFit: 'contain', borderRadius: 8, border: '0.5px solid var(--ol-line)' }}
            />
          </button>
          {previewOpen && (
            <ContextScreenshotPreview
              url={url}
              title={context.windowTitle || t('history.contextCapture.screenshotAlt', '上下文截图')}
              onClose={() => setPreviewOpen(false)}
            />
          )}
        </>
      ) : (
        <div style={{ height: 92, borderRadius: 8, border: '0.5px dashed var(--ol-line-strong)', display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--ol-ink-4)', fontSize: 12 }}>
          {status === 'loading'
            ? t('common.loading')
            : t('history.contextCapture.screenshotUnavailable', '截图不可用')}
        </div>
      )}
      <ContextAnalysisPanel
        context={context}
        reanalysisState={reanalysisState}
        reanalysisEnabled={reanalysisEnabled}
        onReanalyze={() => {
          if (reanalysisEnabled) {
            onReanalyze(context);
          }
        }}
      />
    </div>
  );
}

function ContextAnalysisPanel({
  context,
  reanalysisState,
  reanalysisEnabled,
  onReanalyze,
}: {
  context: ContextCaptureEntry;
  reanalysisState?: ReanalysisUiState;
  reanalysisEnabled: boolean;
  onReanalyze: () => void;
}) {
  const { t } = useTranslation();
  const analysis = context.analysis ?? null;
  const backendAnalysisPending = analysis?.status === 'pending';
  const reanalysisStatus = reanalysisEnabled
    ? reanalysisState?.status ?? (backendAnalysisPending ? 'busy' : undefined)
    : 'disabled';
  const reanalysisBusy = reanalysisStatus === 'busy' || reanalysisStatus === 'queued' || backendAnalysisPending;
  const analysisDuration = analysis ? formatContextAnalysisDuration(analysis) : null;

  return (
    <div style={{ marginTop: 12, paddingTop: 12, borderTop: '0.5px solid var(--ol-line-soft)' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', gap: 10, alignItems: 'center', marginBottom: 10, flexWrap: 'wrap' }}>
        <Pill size="sm" tone="blue">{t('history.contextAnalysis.title', 'AI 上下文分析')}</Pill>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          {reanalysisStatus === 'busy' && (
            <span style={{ fontSize: 11, color: 'var(--ol-ok)' }}>{t('history.contextAnalysis.statusPending', '分析中')}</span>
          )}
          {reanalysisStatus === 'queued' && (
            <span style={{ fontSize: 11, color: 'var(--ol-ok)' }}>{t('history.contextAnalysis.started', '已加入分析队列')}</span>
          )}
          {reanalysisStatus === 'success' && (
            <span style={{ fontSize: 11, color: 'var(--ol-ok)' }}>{t('history.contextAnalysis.finished', '分析完成')}</span>
          )}
          {reanalysisStatus === 'failed' && (
            <span style={{ fontSize: 11, color: 'var(--ol-err)' }}>{t('common.operationFailed')}</span>
          )}
          {reanalysisStatus === 'skipped' && (
            <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>{t('history.contextAnalysis.statusSkipped', '已跳过')}</span>
          )}
          {reanalysisStatus === 'disabled' && (
            <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>{t('history.contextAnalysis.disabled', '请先在设置中开启并授权截图 AI 分析')}</span>
          )}
          <Btn
            size="sm"
            variant="ghost"
            icon="refresh"
            disabled={reanalysisBusy || !reanalysisEnabled}
            onClick={onReanalyze}
          >
            {reanalysisBusy
              ? t('history.contextAnalysis.reanalyzing', '分析中')
              : t('history.contextAnalysis.reanalyze', '重新分析')}
          </Btn>
        </div>
      </div>

      {!analysis ? (
        <div style={{ fontSize: 12, color: 'var(--ol-ink-4)', lineHeight: 1.55 }}>
          {t('history.contextAnalysis.none', '暂无分析结果。开启截图 AI 分析并配置支持图片输入的模型后，新历史会自动分析。')}
        </div>
      ) : (
        <>
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(140px, 1fr))', gap: 10, marginBottom: 10 }}>
            <ContextMeta label={t('history.contextAnalysis.status', '分析状态')} value={contextAnalysisStatusLabel(analysis.status, analysis.errorCode, t)} />
            <ContextMeta label={t('history.contextAnalysis.conversationName', '对话名称')} value={analysis.conversationName} />
            <ContextMeta label={t('history.contextAnalysis.topic', '主题')} value={analysis.topic} />
            <ContextMeta label={t('history.contextAnalysis.workStatus', '工作状态')} value={contextAnalysisWorkStatusLabel(analysis.workStatus, t)} />
            <ContextMeta label={t('history.contextAnalysis.evidenceLevel', '证据强度')} value={contextAnalysisEvidenceLevelLabel(analysis.evidenceLevel, t)} />
            <ContextMeta label={t('history.contextAnalysis.confidence', '置信度')} value={formatConfidence(analysis.confidence)} />
            <ContextMeta label={t('history.contextAnalysis.duration', '分析耗时')} value={analysisDuration} />
          </div>
          {analysis.briefSummary && (
            <div style={{ marginBottom: 8 }}>
              <div style={{ fontSize: 10, color: 'var(--ol-ink-4)', marginBottom: 4 }}>{t('history.contextAnalysis.briefSummary', '简要摘要')}</div>
              <div style={{ fontSize: 12, color: 'var(--ol-ink-2)', lineHeight: 1.6 }}>{analysis.briefSummary}</div>
            </div>
          )}
          <details style={{ fontSize: 12, color: 'var(--ol-ink-3)' }}>
            <summary style={{ cursor: 'default', color: 'var(--ol-ink-3)', marginBottom: 8 }}>
              {t('history.contextAnalysis.details', '完整分析')}
            </summary>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8, lineHeight: 1.6 }}>
              {analysis.fullSummary && <AnalysisBlock label={t('history.contextAnalysis.fullSummary', '完整摘要')} value={analysis.fullSummary} />}
              {analysis.userIntent && <AnalysisBlock label={t('history.contextAnalysis.userIntent', '用户意图')} value={analysis.userIntent} />}
              {analysis.decision && <AnalysisBlock label={t('history.contextAnalysis.decision', '决策/结论')} value={analysis.decision} />}
              {analysis.actionItems.length > 0 && (
                <AnalysisBlock
                  label={t('history.contextAnalysis.actionItems', '待办事项')}
                  value={analysis.actionItems.map(item => `${item.text}${item.owner ? `（${item.owner}）` : ''}`).join('\n')}
                />
              )}
              {analysis.relatedPeople.length > 0 && <AnalysisBlock label={t('history.contextAnalysis.relatedPeople', '相关人员')} value={analysis.relatedPeople.join('、')} />}
              {analysis.projectOrDomain && <AnalysisBlock label={t('history.contextAnalysis.projectOrDomain', '项目/领域')} value={analysis.projectOrDomain} />}
              {analysis.visualEvidence.length > 0 && <AnalysisBlock label={t('history.contextAnalysis.visualEvidence', '视觉依据')} value={analysis.visualEvidence.join('\n')} />}
              <AnalysisBlock label={t('history.contextAnalysis.contextType', '上下文类型')} value={analysis.detectedContextType} />
              <AnalysisBlock label={t('history.contextAnalysis.activityType', '活动类型')} value={analysis.activityType} />
              {contextAnalysisWorkStatusLabel(analysis.workStatus, t) && (
                <AnalysisBlock label={t('history.contextAnalysis.workStatus', '工作状态')} value={contextAnalysisWorkStatusLabel(analysis.workStatus, t)!} />
              )}
              {contextAnalysisEvidenceLevelLabel(analysis.evidenceLevel, t) && (
                <AnalysisBlock label={t('history.contextAnalysis.evidenceLevel', '证据强度')} value={contextAnalysisEvidenceLevelLabel(analysis.evidenceLevel, t)!} />
              )}
              <AnalysisBlock label={t('history.contextAnalysis.sensitive', '敏感信息')} value={analysis.sensitiveContentVisible ? t('common.yes', '是') : t('common.no', '否')} />
              {analysis.uncertaintyReason && <AnalysisBlock label={t('history.contextAnalysis.uncertaintyReason', '不确定原因')} value={analysis.uncertaintyReason} />}
              {analysis.errorCode && <AnalysisBlock label={t('history.contextAnalysis.errorCode', '错误原因')} value={analysis.errorCode} />}
              {analysis.model && <AnalysisBlock label={t('history.contextAnalysis.model', '模型')} value={analysis.model} />}
              {analysis.imageBytes != null && (
                <AnalysisBlock
                  label={t('history.contextAnalysis.imageInfo', '图片请求')}
                  value={`${analysis.imageMimeType ?? 'image/jpeg'} · ${analysis.imageWidth ?? '?'}x${analysis.imageHeight ?? '?'} · ${formatBytes(analysis.imageBytes)}`}
                />
              )}
              <AnalysisBlock label={t('history.contextAnalysis.promptVersion', '提示词版本')} value={analysis.promptVersion} />
            </div>
          </details>
        </>
      )}
    </div>
  );
}

// 截图记录详情页的简化截图展示组件：仅显示截图图片和预览，不含冗余元数据和 AI 分析。
function ScreenshotImagePanel({ contextId, windowTitle }: { contextId: string; windowTitle: string | null }) {
  const { t } = useTranslation();
  const [url, setUrl] = useState<string | null>(null);
  const [status, setStatus] = useState<'idle' | 'loading' | 'ready' | 'missing'>('idle');
  const [previewOpen, setPreviewOpen] = useState(false);

  useEffect(() => {
    setPreviewOpen(false);
    const cachedUrl = retainCachedContextScreenshotUrl(contextId);
    if (cachedUrl) {
      setUrl(cachedUrl);
      setStatus('ready');
      return () => releaseContextScreenshotUrl(contextId);
    }
    setUrl(null);
    setStatus('loading');
    let cancelled = false;
    let retained = false;
    void retainContextScreenshotUrl(contextId)
      .then(objectUrl => {
        retained = true;
        if (cancelled) { releaseContextScreenshotUrl(contextId); return; }
        setUrl(objectUrl);
        setStatus('ready');
      })
      .catch(error => {
        console.warn('[history] screenshot unavailable', error);
        if (!cancelled) setStatus('missing');
      });
    return () => {
      cancelled = true;
      if (retained) releaseContextScreenshotUrl(contextId);
    };
  }, [contextId]);

  useEffect(() => {
    if (!previewOpen) return undefined;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') { event.preventDefault(); setPreviewOpen(false); }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [previewOpen]);

  return (
    <div style={{ padding: 10, border: '0.5px solid var(--ol-line)', borderRadius: 10, background: 'var(--ol-surface-2)' }}>
      {status === 'ready' && url ? (
        <>
          <button
            type="button"
            onClick={() => setPreviewOpen(true)}
            style={{ display: 'block', width: '100%', padding: 0, border: 0, background: 'transparent', cursor: 'zoom-in', fontFamily: 'inherit' }}
            aria-label={t('history.contextCapture.preview', '预览截图')}
          >
            <img
              src={url}
              alt={t('history.contextCapture.screenshotAlt', '上下文截图')}
              style={{ display: 'block', width: '100%', maxHeight: 360, objectFit: 'contain', borderRadius: 8, border: '0.5px solid var(--ol-line)' }}
            />
          </button>
          {previewOpen && (
            <ContextScreenshotPreview
              url={url}
              title={windowTitle || t('history.contextCapture.screenshotAlt', '上下文截图')}
              onClose={() => setPreviewOpen(false)}
            />
          )}
        </>
      ) : (
        <div style={{ height: 92, borderRadius: 8, border: '0.5px dashed var(--ol-line-strong)', display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--ol-ink-4)', fontSize: 12 }}>
          {status === 'loading'
            ? t('common.loading')
            : t('history.contextCapture.screenshotUnavailable', '截图不可用')}
        </div>
      )}
    </div>
  );
}

function screenshotRecordStatusLabel(
  status: ScreenshotRecord['status'],
  t: ReturnType<typeof useTranslation>['t'],
): string {
  switch (status) {
    case 'collecting':
      return t('history.screenshotRecord.statusCollecting', '采集中');
    case 'queued':
      return t('history.screenshotRecord.statusQueued', '已入队');
    case 'analyzing':
      return t('history.screenshotRecord.statusAnalyzing', '分析中');
    case 'success':
      return t('history.screenshotRecord.statusSuccess', '成功');
    case 'failed':
      return t('history.screenshotRecord.statusFailed', '失败');
    case 'skipped':
      return t('history.screenshotRecord.statusSkipped', '已跳过');
    default:
      return status;
  }
}

function AnalysisBlock({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div style={{ fontSize: 10, color: 'var(--ol-ink-4)', marginBottom: 3 }}>{label}</div>
      <div style={{ whiteSpace: 'pre-wrap', color: 'var(--ol-ink-2)' }}>{value}</div>
    </div>
  );
}

function ContextMeta({ label, value }: { label: string; value?: string | null }) {
  return (
    <div style={{ minWidth: 0 }}>
      <div style={{ fontSize: 10, color: 'var(--ol-ink-4)', marginBottom: 3 }}>{label}</div>
      <div style={{ fontSize: 12, color: 'var(--ol-ink-2)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={value || undefined}>
        {value || '未识别'}
      </div>
    </div>
  );
}

function ContextScreenshotPreview({
  url,
  title,
  onClose,
}: {
  url: string;
  title: string;
  onClose: () => void;
}) {
  const { t } = useTranslation();

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={t('history.contextCapture.preview', '预览截图')}
      onClick={onClose}
      style={{
        position: 'fixed',
        inset: 0,
        zIndex: 2000,
        display: 'grid',
        gridTemplateRows: 'auto minmax(0, 1fr)',
        gap: 12,
        padding: 24,
        background: 'rgba(9, 12, 18, 0.72)',
        backdropFilter: 'blur(8px)',
      }}
    >
      <div
        onClick={event => event.stopPropagation()}
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
          gap: 12,
          color: '#fff',
          minWidth: 0,
        }}
      >
        <div style={{ fontSize: 13, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={title}>
          {title}
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label={t('common.close')}
          style={{
            width: 34,
            height: 34,
            borderRadius: 8,
            border: '0.5px solid rgba(255,255,255,0.28)',
            background: 'rgba(255,255,255,0.12)',
            color: '#fff',
            display: 'inline-grid',
            placeItems: 'center',
            cursor: 'default',
          }}
        >
          <Icon name="x" size={16} />
        </button>
      </div>
      <div
        onClick={event => event.stopPropagation()}
        style={{
          minHeight: 0,
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
        }}
      >
        <img
          src={url}
          alt={title}
          style={{
            maxWidth: '100%',
            maxHeight: '100%',
            objectFit: 'contain',
            borderRadius: 8,
            boxShadow: '0 24px 80px rgba(0,0,0,0.42)',
            background: '#111',
          }}
        />
      </div>
    </div>
  );
}

function contextCaptureStatusLabel(
  status: ContextCaptureEntry['captureStatus'],
  t: ReturnType<typeof useTranslation>['t'],
) {
  switch (status) {
    case 'success':
      return t('history.contextCapture.statusSuccess', '成功');
    case 'activeWindowFailedFullScreenSuccess':
      return t('history.contextCapture.statusFullScreenFallback', '活动窗口失败，已回退全屏');
    case 'failed':
      return t('history.contextCapture.statusFailed', '失败');
    case 'unsupported':
      return t('history.contextCapture.statusUnsupported', '当前平台不支持');
    default:
      return status;
  }
}

function contextCaptureSourceLabel(
  source: ContextCaptureEntry['captureSource'],
  t: ReturnType<typeof useTranslation>['t'],
) {
  switch (source) {
    case 'activeWindow':
      return t('history.contextCapture.sourceActiveWindow', '活动窗口');
    case 'fullScreen':
      return t('history.contextCapture.sourceFullScreen', '全屏');
    default:
      return null;
  }
}

function contextAnalysisStatusLabel(
  status: NonNullable<ContextCaptureEntry['analysis']>['status'],
  errorCode: string | null,
  t: ReturnType<typeof useTranslation>['t'],
) {
  if (status === 'pending') return t('history.contextAnalysis.statusPending', '分析中');
  if (status === 'success') return t('history.contextAnalysis.statusSuccess', '成功');
  if (status === 'skipped') {
    if (errorCode === 'skipped:modelNotConfigured') {
      return t('history.contextAnalysis.modelNotConfigured', '未配置截图分析模型');
    }
    if (errorCode === 'skipped:screenshotUnavailable') {
      return t('history.contextAnalysis.screenshotUnavailable', '截图不可用，未分析');
    }
    if (errorCode === 'skipped:unsupportedProvider') {
      return t('history.contextAnalysis.unsupportedProvider', '当前 LLM 服务暂不支持');
    }
    if (errorCode === 'skipped:providerNotConfigured') {
      return t('history.contextAnalysis.providerNotConfigured', '未配置 LLM 服务地址');
    }
    return t('history.contextAnalysis.statusSkipped', '已跳过');
  }
  if (status === 'failed') {
    if (errorCode === 'failed:modelNotVisionCapable') {
      return t('history.contextAnalysis.modelNotVisionCapable', '模型可能不支持图片输入');
    }
    if (errorCode === 'failed:imagePrepareFailed') {
      return t('history.contextAnalysis.imagePrepareFailed', '图片处理失败');
    }
    if (errorCode === 'failed:visionRequestTimeout') {
      return t('history.contextAnalysis.visionRequestTimeout', '截图分析请求超时，请稍后重试');
    }
    if (errorCode === 'failed:visionRequestFailed') {
      return t('history.contextAnalysis.visionRequestFailed', '截图分析请求失败，请稍后重试');
    }
    return t('history.contextAnalysis.statusFailed', '失败');
  }
  return status;
}

function contextAnalysisWorkStatusLabel(
  status: ContextAnalysisResult['workStatus'] | undefined,
  t: ReturnType<typeof useTranslation>['t'],
): string | null {
  if (status === 'completed') return t('history.contextAnalysis.workStatusCompleted', '已完成');
  if (status === 'inProgress') return t('history.contextAnalysis.workStatusInProgress', '进行中');
  if (status === 'planned') return t('history.contextAnalysis.workStatusPlanned', '计划中');
  if (status === 'discussed') return t('history.contextAnalysis.workStatusDiscussed', '讨论中');
  if (status === 'viewed') return t('history.contextAnalysis.workStatusViewed', '仅查看');
  if (status === 'unknown') return t('history.contextAnalysis.workStatusUnknown', '未知');
  return null;
}

function contextAnalysisEvidenceLevelLabel(
  level: ContextAnalysisResult['evidenceLevel'] | undefined,
  t: ReturnType<typeof useTranslation>['t'],
): string | null {
  if (level === 'explicit') return t('history.contextAnalysis.evidenceLevelExplicit', '明确证据');
  if (level === 'inferred') return t('history.contextAnalysis.evidenceLevelInferred', '合理推断');
  if (level === 'weak') return t('history.contextAnalysis.evidenceLevelWeak', '弱线索');
  if (level === 'unknown') return t('history.contextAnalysis.evidenceLevelUnknown', '未知');
  return null;
}

function formatConfidence(confidence: number | null | undefined): string | null {
  if (confidence == null || Number.isNaN(confidence)) return null;
  return `${Math.round(Math.max(0, Math.min(1, confidence)) * 100)}%`;
}

function formatContextAnalysisDuration(
  analysis: NonNullable<ContextCaptureEntry['analysis']>,
): string | null {
  const start = Date.parse(analysis.createdAt);
  if (!Number.isFinite(start)) return null;
  const end = analysis.analyzedAt ? Date.parse(analysis.analyzedAt) : Date.now();
  if (!Number.isFinite(end) || end < start) return null;
  return formatPlainDuration(end - start);
}

function formatPlainDuration(ms: number): string {
  const seconds = Math.max(0, ms / 1000);
  if (seconds < 60) return `${seconds.toFixed(1)} 秒`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = Math.round(seconds % 60);
  if (minutes < 60) return `${minutes} 分 ${remainingSeconds} 秒`;
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return `${hours} 小时 ${remainingMinutes} 分`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

function AudioRecordingPlayer({
  sessionId,
  onMissing,
}: {
  sessionId: string;
  onMissing?: () => void;
}) {
  const { t } = useTranslation();
  const [blobUrl, setBlobUrl] = useState<string | null>(null);
  const [status, setStatus] = useState<'idle' | 'loading' | 'ready' | 'error'>('idle');
  const [errorText, setErrorText] = useState<string | null>(null);
  const mountedRef = useRef(true);
  const blobUrlRef = useRef<string | null>(null);

  // 组件 unmount 时释放 Blob URL，避免内存泄漏。
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (blobUrlRef.current) {
        URL.revokeObjectURL(blobUrlRef.current);
        blobUrlRef.current = null;
      }
    };
  }, []);

  const clearBlobUrl = () => {
    if (blobUrlRef.current) {
      URL.revokeObjectURL(blobUrlRef.current);
      blobUrlRef.current = null;
    }
    setBlobUrl(null);
  };

  const load = async () => {
    setStatus('loading');
    setErrorText(null);
    try {
      const dataUrl = await readAudioRecording(sessionId);
      if (!mountedRef.current) return;
      if (!dataUrl || dataUrl === 'data:audio/wav;base64,') throw new Error('empty recording');
      // WebKitGTK <audio> 对 data: URL 解码不稳定（时长 0 / 播不动），
      // 把 base64 解码为二进制再封装成 Blob URL，在 WebKit 里远更可靠。
      const comma = dataUrl.indexOf(',');
      const b64 = comma >= 0 ? dataUrl.slice(comma + 1) : '';
      if (!b64) throw new Error('empty recording');
      const bin = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
      const blob = new Blob([bin], { type: 'audio/wav' });
      const url = URL.createObjectURL(blob);
      if (!mountedRef.current) {
        URL.revokeObjectURL(url);
        return;
      }
      if (blobUrlRef.current) URL.revokeObjectURL(blobUrlRef.current);
      blobUrlRef.current = url;
      setBlobUrl(url);
      setStatus('ready');
    } catch (error) {
      if (!mountedRef.current) return;
      console.error('[history] load recording failed', error);
      const msg = errorMessage(error);
      if (msg.includes('recording not found') || msg.includes('not found')) {
        onMissing?.();
        return;
      }
      setStatus('error');
      setErrorText(msg);
    }
  };

  if (status === 'ready' && blobUrl) {
    return (
      <div style={{ marginBottom: 14 }}>
        <audio
          src={blobUrl}
          controls
          preload="auto"
          autoPlay
          style={{ width: '100%' }}
          onError={(e) => {
            if (!mountedRef.current) return;
            const a = e.currentTarget;
            const code = a.error?.code ?? -1;
            const detail = a.error?.message ?? `${code}`;
            console.error('[history] <audio> decode/play failed', { code, detail });
            clearBlobUrl();
            setStatus('error');
            setErrorText(t('history.audioDecodeFailed', { err: detail }));
          }}
        />
      </div>
    );
  }
  return (
    <div style={{ marginBottom: 14, display: 'flex', alignItems: 'center', gap: 10 }}>
      <Btn
        icon="play"
        variant="ghost"
        size="sm"
        onClick={() => void load()}
        disabled={status === 'loading'}
      >
        {status === 'loading' ? t('history.audioLoading') : t('history.playRecording')}
      </Btn>
      {status === 'error' && (
        <span style={{ fontSize: 11, color: 'var(--ol-err)' }}>{errorText}</span>
      )}
    </div>
  );
}

function formatTime(iso: string): string {
  const d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  const pad = (n: number) => String(n).padStart(2, '0');
  if (sameDay) return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  return `${d.getMonth() + 1}/${d.getDate()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 流水线单步耗时：<1s 显示整数毫秒（流式收尾常在几十 ms，0.1s 精度会把不同结果
 *  拍成同一个值，模型对比就失真了——PR #826 review）；≥1s 沿用 0.1s 精度。 */
function formatStepDuration(ms: number, t: ReturnType<typeof useTranslation>['t']): string {
  if (ms < 1000) return t('common.durationMillis', { value: Math.round(ms) });
  return formatDuration(ms, t);
}

function formatDuration(ms: number | null, t: ReturnType<typeof useTranslation>['t']): string {
  if (ms == null || ms <= 0) return '—';
  const sec = ms / 1000;
  if (sec < 60) return t('common.durationSeconds', { value: sec.toFixed(1) });
  return t('common.durationMinutes', { value: (sec / 60).toFixed(1) });
}
