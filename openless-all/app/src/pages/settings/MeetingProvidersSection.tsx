import { useCallback, useEffect, useMemo, useState, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { Icon } from '../../components/Icon';
import { detectOS } from '../../components/WindowChrome';
import {
  cancelSpeakerDiarizationModelDownload,
  deleteSpeakerDiarizationModel,
  downloadSpeakerDiarizationModel,
  isTauri,
  listAsrProviderModels,
  listPostMeetingAsrModels,
  listSpeakerDiarizationModels,
  readAsrProviderCredential,
  setAsrProviderCredential,
  type SpeakerDiarizationDownloadProgress,
  type SpeakerDiarizationModelDescriptor,
} from '../../lib/ipc';
import { listAsrProviderCapabilities } from '../../lib/ipc/settings';
import type {
  AsrProviderCapabilities,
  MeetingAsrMode,
  MeetingDiarizationMode,
  MeetingVadSilencePreset,
  PostMeetingAsrModelDescriptor,
} from '../../lib/types';
import { emitSaved } from '../../lib/savedEvent';
import { useMobileLayout } from '../../lib/useMobileLayout';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { SelectLite } from '../../components/ui/SelectLite';
import { Card } from '../_atoms';
import { LOCAL_ASR_PROVIDER_IDS } from './ProvidersSection';
import { ASR_PRESETS, inputStyle, SectionTitle, SettingRow, type AsrPresetId } from './shared';

const ASR_DEFAULT_RESOURCE_ID = 'volc.seedasr.sauc.duration';

function providerPreset(id: string) {
  return ASR_PRESETS.find(preset => preset.id === id);
}

function supportsModelField(id: string): boolean {
  return id !== 'volcengine' && !LOCAL_ASR_PROVIDER_IDS.includes(id);
}

export function MeetingProvidersSection() {
  const { t } = useTranslation();
  const { prefs, updatePrefs } = useHotkeySettings();
  const mobile = useMobileLayout();
  const os = detectOS();
  const [capabilities, setCapabilities] = useState<AsrProviderCapabilities[]>([]);
  const [postModels, setPostModels] = useState<PostMeetingAsrModelDescriptor[]>([]);
  const [volcengineAuthMode, setVolcengineAuthMode] = useState<'app_id_token' | 'api_key'>('app_id_token');

  useEffect(() => {
    let cancelled = false;
    void listAsrProviderCapabilities()
      .then(value => { if (!cancelled) setCapabilities(value); })
      .catch(error => {
        console.warn('[settings] failed to load meeting ASR capabilities', error);
        if (!cancelled) setCapabilities([]);
      });
    void listPostMeetingAsrModels()
      .then(value => { if (!cancelled) setPostModels(value); })
      .catch(error => {
        console.warn('[settings] failed to load post-meeting ASR models', error);
        if (!cancelled) setPostModels([]);
      });
    return () => { cancelled = true; };
  }, []);

  const meetingAsr = prefs?.meetingAsr;
  const meetingProviderId = (meetingAsr?.providerId || prefs?.activeAsrProvider || 'volcengine') as AsrPresetId;
  const effectiveProviderId = (meetingAsr?.mode === 'provider_specific'
    ? meetingProviderId
    : prefs?.activeAsrProvider || 'volcengine') as AsrPresetId;
  const effectivePreset = providerPreset(effectiveProviderId);
  const modelOverride = meetingAsr?.modelProviderId === effectiveProviderId
    ? meetingAsr.modelOverride || ''
    : '';

  useEffect(() => {
    if (meetingAsr?.mode !== 'provider_specific' || meetingProviderId !== 'volcengine') return;
    let cancelled = false;
    void readAsrProviderCredential(meetingProviderId, 'volcengine.auth_mode')
      .then(value => {
        if (!cancelled) setVolcengineAuthMode(value === 'api_key' ? 'api_key' : 'app_id_token');
      })
      .catch(() => { if (!cancelled) setVolcengineAuthMode('app_id_token'); });
    return () => { cancelled = true; };
  }, [meetingAsr?.mode, meetingProviderId]);

  if (!prefs || !meetingAsr) return null;

  const updateMeeting = (patch: Partial<typeof meetingAsr>) => updatePrefs(current => ({
    ...current,
    meetingAsr: { ...current.meetingAsr, ...patch },
  }));

  const saveModelOverride = (providerId: string, value: string) => {
    const trimmed = value.trim();
    return updateMeeting({
      modelOverride: trimmed || null,
      modelProviderId: trimmed ? providerId : null,
    });
  };

  const postMeetingAsr = prefs.postMeetingAsr;
  const postModelValue = `${postMeetingAsr.providerId}/${postMeetingAsr.modelId}`;
  const supportsSilencePreset = capabilities.some(
    item => item.providerId === effectiveProviderId && item.supportsVadSilencePreset,
  );

  return (
    <>
      <Card>
        <div style={{ marginBottom: 10 }}>
          <SectionTitle>{t('settings.providers.meetingAsrTitle')}</SectionTitle>
        </div>
        <SettingRow label={t('settings.providers.meetingAsrModeLabel')}>
          <SelectLite
            value={meetingAsr.mode}
            onChange={value => void updateMeeting({
              mode: value as MeetingAsrMode,
              providerId: value === 'provider_specific'
                ? meetingAsr.providerId || prefs.activeAsrProvider || 'volcengine'
                : meetingAsr.providerId,
            }).catch(error => reportSaveError(error, t))}
            options={[
              { value: 'inherit_global', label: t('settings.providers.meetingAsrInheritGlobal') },
              { value: 'provider_specific', label: t('settings.providers.meetingAsrProviderSpecific') },
            ]}
            ariaLabel={t('settings.providers.meetingAsrModeLabel')}
            style={{ ...inputStyle, width: '100%', maxWidth: mobile ? '100%' : 260 }}
          />
        </SettingRow>

        {meetingAsr.mode === 'inherit_global' ? (
          <>
            <SettingRow label={t('settings.providers.meetingAsrProviderLabel')}>
              <span style={valueTextStyle}>
                {effectivePreset
                  ? t(`settings.providers.presets.${effectivePreset.nameKey}`)
                  : effectiveProviderId}
              </span>
            </SettingRow>
            {supportsModelField(effectiveProviderId) && (
              <MeetingModelField
                providerId={effectiveProviderId}
                value={modelOverride}
                placeholder={effectivePreset?.model || 'whisper-1'}
                onSave={value => saveModelOverride(effectiveProviderId, value)}
              />
            )}
          </>
        ) : (
          <>
            <SettingRow label={t('settings.providers.meetingAsrProviderLabel')}>
              <SelectLite
                value={meetingProviderId}
                onChange={value => void updateMeeting({ mode: 'provider_specific', providerId: value })
                  .catch(error => reportSaveError(error, t))}
                options={ASR_PRESETS.map(preset => ({
                  value: preset.id,
                  label: t(`settings.providers.presets.${preset.nameKey}`),
                }))}
                ariaLabel={t('settings.providers.meetingAsrProviderLabel')}
                style={{ ...inputStyle, width: '100%', maxWidth: mobile ? '100%' : 260 }}
              />
            </SettingRow>
            <MeetingProviderCredentials
              providerId={meetingProviderId}
              authMode={volcengineAuthMode}
              onAuthModeChange={setVolcengineAuthMode}
            />
            {supportsModelField(meetingProviderId) && (
              <MeetingModelField
                providerId={meetingProviderId}
                value={modelOverride}
                placeholder={providerPreset(meetingProviderId)?.model || 'whisper-1'}
                onSave={value => saveModelOverride(meetingProviderId, value)}
              />
            )}
            {supportsSilencePreset && (
              <SettingRow label={t('settings.providers.meetingAsrSilenceLabel')}>
                <SelectLite
                  value={meetingAsr.silencePreset}
                  onChange={value => void updateMeeting({ silencePreset: value as MeetingVadSilencePreset })
                    .catch(error => reportSaveError(error, t))}
                  options={[
                    { value: 'short', label: t('settings.providers.meetingAsrSilenceShort') },
                    { value: 'standard', label: t('settings.providers.meetingAsrSilenceStandard') },
                    { value: 'long', label: t('settings.providers.meetingAsrSilenceLong') },
                  ]}
                  ariaLabel={t('settings.providers.meetingAsrSilenceLabel')}
                  style={{ ...inputStyle, width: '100%', maxWidth: mobile ? '100%' : 260 }}
                />
              </SettingRow>
            )}
          </>
        )}
      </Card>

      <Card>
        <div style={{ marginBottom: 10 }}>
          <SectionTitle>{t('settings.providers.postMeetingAsrTitle')}</SectionTitle>
        </div>
        <SettingRow label={t('settings.providers.postMeetingAsrModelLabel')}>
          <SelectLite
            value={postModelValue}
            onChange={value => {
              const selected = postModels.find(model => `${model.providerId}/${model.modelId}` === value);
              if (!selected) return;
              void updatePrefs(current => ({
                ...current,
                postMeetingAsr: {
                  ...current.postMeetingAsr,
                  providerId: selected.providerId,
                  modelId: selected.modelId,
                },
              })).catch(error => reportSaveError(error, t));
            }}
            options={postModels.map(model => ({
              value: `${model.providerId}/${model.modelId}`,
              label: model.isDefault
                ? t('settings.providers.postMeetingAsrDefaultOption', { model: model.displayName })
                : model.displayName,
            }))}
            disabled={postModels.length === 0}
            placeholder={t('common.loading')}
            ariaLabel={t('settings.providers.postMeetingAsrModelLabel')}
            style={{ ...inputStyle, width: '100%', maxWidth: mobile ? '100%' : 280 }}
          />
        </SettingRow>
        <SettingRow label={t('settings.providers.meetingDiarizationLabel')}>
          <SelectLite
            value={postMeetingAsr.diarization.mode}
            onChange={value => void updatePrefs(current => ({
              ...current,
              postMeetingAsr: {
                ...current.postMeetingAsr,
                diarization: {
                  ...current.postMeetingAsr.diarization,
                  mode: value as MeetingDiarizationMode,
                },
              },
            })).catch(error => reportSaveError(error, t))}
            options={[
              { value: 'off', label: t('settings.providers.meetingDiarizationOff') },
              { value: 'cloud', label: t('settings.providers.meetingDiarizationCloud') },
              { value: 'local', label: t('settings.providers.meetingDiarizationLocal') },
            ]}
            ariaLabel={t('settings.providers.meetingDiarizationLabel')}
            style={{ ...inputStyle, width: '100%', maxWidth: mobile ? '100%' : 260 }}
          />
        </SettingRow>
        {postMeetingAsr.diarization.mode === 'cloud' && (
          <div role="note" style={noteStyle}>{t('settings.providers.meetingDiarizationCloudHint')}</div>
        )}
        {postMeetingAsr.diarization.mode === 'local' && (
          <>
            <SpeakerDiarizationModelControl
              supported={os === 'win'}
              selectedModelId={postMeetingAsr.diarization.localModelId}
              onSelect={modelId => updatePrefs(current => ({
                ...current,
                postMeetingAsr: {
                  ...current.postMeetingAsr,
                  diarization: { ...current.postMeetingAsr.diarization, localModelId: modelId },
                },
              }))}
            />
            <div role="note" style={noteStyle}>
              {postMeetingAsr.diarization.localModelId
                ? t('settings.providers.meetingDiarizationLocalHint')
                : t('settings.providers.meetingDiarizationLocalModelMissing')}
            </div>
          </>
        )}
      </Card>
    </>
  );
}

function MeetingProviderCredentials({
  providerId,
  authMode,
  onAuthModeChange,
}: {
  providerId: string;
  authMode: 'app_id_token' | 'api_key';
  onAuthModeChange: (mode: 'app_id_token' | 'api_key') => void;
}) {
  const { t } = useTranslation();
  if (LOCAL_ASR_PROVIDER_IDS.includes(providerId)) {
    return <div style={noteStyle}>{t('settings.providers.localEngineNoCredentials')}</div>;
  }
  if (providerId === 'volcengine') {
    return (
      <>
        <SettingRow label={t('settings.providers.volcengineAuthModeLabel')}>
          <SelectLite
            value={authMode}
            onChange={value => {
              const next = value as 'app_id_token' | 'api_key';
              const previous = authMode;
              onAuthModeChange(next);
              void setAsrProviderCredential(providerId, 'volcengine.auth_mode', next)
                .catch(error => {
                  onAuthModeChange(previous);
                  reportSaveError(error, t);
                });
            }}
            options={[
              { value: 'app_id_token', label: t('settings.providers.volcengineAuthModeAppIdToken') },
              { value: 'api_key', label: t('settings.providers.volcengineAuthModeApiKey') },
            ]}
            ariaLabel={t('settings.providers.volcengineAuthModeLabel')}
            style={{ ...inputStyle, width: '100%', maxWidth: 280 }}
          />
        </SettingRow>
        {authMode === 'app_id_token' ? (
          <>
            <ProviderCredentialField providerId={providerId} account="volcengine.app_key" label={t('settings.providers.volcengineAppKeyLabel')} secret />
            <ProviderCredentialField providerId={providerId} account="volcengine.access_key" label={t('settings.providers.volcengineAccessKeyLabel')} secret />
          </>
        ) : (
          <ProviderCredentialField providerId={providerId} account="volcengine.api_key" label={t('settings.providers.volcengineApiKeyLabel')} secret />
        )}
        <ProviderCredentialField providerId={providerId} account="volcengine.resource_id" label={t('settings.providers.volcengineResourceIdLabel')} placeholder={ASR_DEFAULT_RESOURCE_ID} />
      </>
    );
  }
  if (providerId === 'iflytek') {
    return (
      <>
        <ProviderCredentialField providerId={providerId} account="xfyun.app_id" label={t('settings.providers.xfyunAppIdLabel')} />
        <ProviderCredentialField providerId={providerId} account="xfyun.api_key" label={t('settings.providers.xfyunApiKeyLabel')} secret />
      </>
    );
  }
  const preset = providerPreset(providerId);
  return (
    <>
      <ProviderCredentialField providerId={providerId} account="asr.api_key" label={t('settings.providers.apiKeyLabel')} secret />
      <ProviderCredentialField providerId={providerId} account="asr.endpoint" label={t('settings.providers.baseUrlLabel')} placeholder={preset?.baseUrl || 'https://api.openai.com/v1'} />
      {providerId === 'bailian' && (
        <ProviderCredentialField providerId={providerId} account="asr.vocabulary_id" label={t('settings.providers.bailianVocabularyIdLabel')} placeholder="vocab-..." />
      )}
    </>
  );
}

function ProviderCredentialField({
  providerId,
  account,
  label,
  placeholder,
  secret,
}: {
  providerId: string;
  account: string;
  label: string;
  placeholder?: string;
  secret?: boolean;
}) {
  const { t } = useTranslation();
  const [value, setValue] = useState('');
  const [loadedValue, setLoadedValue] = useState('');
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void readAsrProviderCredential(providerId, account)
      .then(current => {
        if (cancelled) return;
        setValue(current || '');
        setLoadedValue(current || '');
      })
      .catch(error => console.warn('[settings] failed to read meeting ASR credential', error));
    return () => { cancelled = true; };
  }, [providerId, account]);

  const save = async () => {
    if (value === loadedValue) return;
    setSaving(true);
    try {
      await setAsrProviderCredential(providerId, account, value.trim());
      setLoadedValue(value.trim());
      setValue(value.trim());
      emitSaved('saved', t('common.saved'));
    } catch (error) {
      reportSaveError(error, t);
    } finally {
      setSaving(false);
    }
  };

  return (
    <SettingRow label={label}>
      <input
        type={secret ? 'password' : 'text'}
        value={value}
        onChange={event => setValue(event.target.value)}
        onBlur={() => void save()}
        placeholder={placeholder}
        disabled={saving}
        style={{ ...inputStyle, width: '100%', maxWidth: 360, fontFamily: 'var(--ol-font-mono)' }}
      />
    </SettingRow>
  );
}

function MeetingModelField({
  providerId,
  value,
  placeholder,
  onSave,
}: {
  providerId: string;
  value: string;
  placeholder: string;
  onSave: (value: string) => Promise<void>;
}) {
  const { t } = useTranslation();
  const mobile = useMobileLayout();
  const [draft, setDraft] = useState(value);
  const [models, setModels] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => setDraft(value), [value, providerId]);

  const save = async (next: string) => {
    setDraft(next);
    try {
      await onSave(next);
      emitSaved('saved', t('common.saved'));
    } catch (error) {
      reportSaveError(error, t);
    }
  };

  return (
    <SettingRow label={t('settings.providers.modelLabel')}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 7, width: '100%', maxWidth: 430, flexWrap: mobile ? 'wrap' : 'nowrap' }}>
        {models.length > 0 ? (
          <SelectLite
            value={draft}
            onChange={value => void save(value)}
            options={models.map(model => ({ value: model, label: model }))}
            ariaLabel={t('settings.providers.modelLabel')}
            style={{ ...inputStyle, flex: 1, minWidth: 200 }}
          />
        ) : (
          <input
            value={draft}
            onChange={event => setDraft(event.target.value)}
            onBlur={() => void save(draft)}
            placeholder={placeholder}
            style={{ ...inputStyle, flex: 1, minWidth: 200, fontFamily: 'var(--ol-font-mono)' }}
          />
        )}
        <button
          type="button"
          onClick={() => {
            setLoading(true);
            void listAsrProviderModels(providerId)
              .then(result => setModels(result.models))
              .catch(error => reportSaveError(error, t))
              .finally(() => setLoading(false));
          }}
          disabled={loading}
          style={miniButtonStyle}
        >
          {loading ? t('settings.providers.loadingModels') : t('settings.providers.fetchModels')}
        </button>
      </div>
    </SettingRow>
  );
}

function SpeakerDiarizationModelControl({
  supported,
  selectedModelId,
  onSelect,
}: {
  supported: boolean;
  selectedModelId: string | null;
  onSelect: (modelId: string | null) => Promise<void>;
}) {
  const { t } = useTranslation();
  const mobile = useMobileLayout();
  const [models, setModels] = useState<SpeakerDiarizationModelDescriptor[]>([]);
  const [progress, setProgress] = useState<SpeakerDiarizationDownloadProgress | null>(null);
  const [busy, setBusy] = useState<'download' | 'delete' | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!supported) return;
    try {
      setModels(await listSpeakerDiarizationModels());
      setError(null);
    } catch (loadError) {
      setError(String(loadError));
    }
  }, [supported]);

  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => {
    if (!supported || !isTauri) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import('@tauri-apps/api/event')
      .then(({ listen }) => listen<SpeakerDiarizationDownloadProgress>(
        'speaker-diarization-model-download-progress',
        event => {
          if (disposed) return;
          setProgress(event.payload);
          if (event.payload.phase === 'finished' || event.payload.phase === 'cancelled' || event.payload.phase === 'failed') {
            setBusy(null);
            void refresh();
          }
        },
      ))
      .then(off => { if (disposed) off(); else unlisten = off; })
      .catch(subscribeError => console.warn('[settings] speaker model progress subscribe failed', subscribeError));
    return () => { disposed = true; unlisten?.(); };
  }, [refresh, supported]);

  if (!supported) {
    return (
      <SettingRow label={t('settings.providers.meetingDiarizationModelLabel')}>
        <span style={{ fontSize: 11.5, color: 'var(--ol-warn)' }}>
          {t('settings.providers.meetingDiarizationModelUnsupported')}
        </span>
      </SettingRow>
    );
  }

  const selected = models.find(model => model.id === selectedModelId) ?? models[0];
  const downloading = busy === 'download' || progress?.phase === 'started' || progress?.phase === 'progress';
  const downloadedBytes = progress?.bytesDownloaded ?? selected?.downloadedBytes ?? 0;
  const totalBytes = progress?.bytesTotal ?? selected?.totalBytes ?? 0;
  const percent = totalBytes > 0 ? Math.min(100, Math.round(downloadedBytes / totalBytes * 100)) : 0;

  return (
    <SettingRow label={t('settings.providers.meetingDiarizationModelLabel')}>
      <div style={{ display: 'grid', gap: 7, width: '100%', maxWidth: mobile ? '100%' : 430 }}>
        <div style={{ display: 'flex', gap: 6, alignItems: 'center', flexWrap: mobile ? 'wrap' : 'nowrap' }}>
          <SelectLite
            value={selectedModelId ?? ''}
            onChange={value => void onSelect(value || null)}
            options={models.map(model => ({ value: model.id, label: model.displayName }))}
            disabled={models.length === 0 || downloading || busy === 'delete'}
            placeholder={models.length === 0 ? t('common.loading') : t('settings.providers.meetingDiarizationModelChoose')}
            ariaLabel={t('settings.providers.meetingDiarizationModelLabel')}
            style={{ ...inputStyle, flex: 1, minWidth: mobile ? '100%' : 220 }}
          />
          {downloading ? (
            <button type="button" onClick={() => selected && void cancelSpeakerDiarizationModelDownload(selected.id)} title={t('common.cancel')} style={iconButtonStyle}>
              <Icon name="x" size={14} />
            </button>
          ) : selected?.readiness === 'ready' ? (
            <button type="button" onClick={() => {
              if (!window.confirm(t('settings.providers.meetingDiarizationModelDeleteConfirm'))) return;
              setBusy('delete');
              void deleteSpeakerDiarizationModel(selected.id)
                .then(async () => {
                  if (selectedModelId === selected.id) await onSelect(null);
                  await refresh();
                })
                .catch(deleteError => setError(String(deleteError)))
                .finally(() => setBusy(null));
            }} title={t('common.delete')} style={iconButtonStyle} disabled={busy === 'delete'}>
              <Icon name="trash" size={14} />
            </button>
          ) : (
            <button type="button" onClick={() => {
              if (!selected) return;
              setBusy('download');
              setProgress({ modelId: selected.id, file: '', fileIndex: 0, fileCount: 0, bytesDownloaded: 0, bytesTotal: selected.totalBytes, phase: 'started', error: null });
              void downloadSpeakerDiarizationModel(selected.id)
                .then(refresh)
                .catch(downloadError => setError(String(downloadError)))
                .finally(() => { if (!isTauri) setBusy(null); });
            }} title={t('settings.providers.meetingDiarizationModelDownload')} style={iconButtonStyle} disabled={!selected}>
              <Icon name={selected?.readiness === 'invalid' ? 'refresh' : 'download'} size={14} />
            </button>
          )}
        </div>
        {selected && (
          <>
            <div style={{ display: 'flex', justifyContent: 'space-between', gap: 8, fontSize: 11, color: 'var(--ol-ink-4)' }}>
              <span>{t(`settings.providers.meetingDiarizationModelStatus.${downloading ? 'downloading' : selected.readiness}`)}</span>
              <span>{formatBytes(downloadedBytes)} / {formatBytes(totalBytes)}</span>
            </div>
            {downloading && (
              <div role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent} style={{ height: 4, borderRadius: 2, overflow: 'hidden', background: 'var(--ol-line)' }}>
                <div style={{ width: `${percent}%`, height: '100%', background: 'var(--ol-blue)' }} />
              </div>
            )}
            <div style={{ display: 'grid', gap: 2, fontSize: 11, color: 'var(--ol-ink-4)', lineHeight: 1.45 }}>
              <span>{t('settings.providers.meetingDiarizationModelSource')}: {selected.source}</span>
              <span>{t('settings.providers.meetingDiarizationModelPlatform')}: {selected.supportedPlatforms.join(', ')}</span>
              <span>{t('settings.providers.meetingDiarizationModelDuration')}: {selected.maxRecommendedDurationMs == null ? t('settings.providers.meetingDiarizationModelPendingValidation') : `${Math.round(selected.maxRecommendedDurationMs / 60_000)} min`}</span>
              <span>{t('settings.providers.meetingDiarizationModelMemory')}: {selected.memoryTier ?? t('settings.providers.meetingDiarizationModelPendingValidation')}</span>
            </div>
          </>
        )}
        {error && <span role="alert" style={{ fontSize: 11, color: 'var(--ol-warn)' }}>{error}</span>}
      </div>
    </SettingRow>
  );
}

function reportSaveError(error: unknown, t: ReturnType<typeof useTranslation>['t']) {
  console.error('[settings] failed to save meeting provider setting', error);
  emitSaved('failed', t('common.operationFailed'));
}

function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 MB';
  return `${(bytes / 1024 / 1024).toFixed(bytes < 10 * 1024 * 1024 ? 1 : 0)} MB`;
}

const valueTextStyle: CSSProperties = { fontSize: 12.5, color: 'var(--ol-ink-2)' };
const noteStyle: CSSProperties = { marginTop: 8, fontSize: 11.5, color: 'var(--ol-ink-4)', lineHeight: 1.6 };
const miniButtonStyle: CSSProperties = { padding: '6px 10px', borderRadius: 7, border: '0.5px solid var(--ol-line)', background: 'var(--ol-surface-2)', color: 'var(--ol-ink-2)', fontFamily: 'inherit', fontSize: 11.5, cursor: 'pointer' };
const iconButtonStyle: CSSProperties = { width: 30, height: 30, display: 'grid', placeItems: 'center', borderRadius: 7, border: '0.5px solid var(--ol-line)', background: 'var(--ol-surface-2)', color: 'var(--ol-ink-3)', cursor: 'pointer' };
