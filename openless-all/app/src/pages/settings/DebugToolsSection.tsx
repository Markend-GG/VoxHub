// 高级 → 调试工具：保留原始录音、导出错误日志等排障入口。
// recordAudioForDebug 行自 Settings.tsx 的 RecordingSection 拆出；
// 导出错误日志自 SettingsModal 的 AboutMini 迁入 —— 调试相关集中到此处。

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { exportErrorLog, listProviderModels, readCredential, setCredential } from '../../lib/ipc';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { Btn, Card } from '../_atoms';
import { SettingRow, Toggle, SectionTitle, inputStyle } from './shared';

const clamp = (n: number, min: number, max: number) => Math.max(min, Math.min(max, n));

export function DebugToolsSection() {
  const { t } = useTranslation();
  const { prefs, updatePrefs: savePrefs } = useHotkeySettings();
  const [exportStatus, setExportStatus] = useState<'idle' | 'busy' | 'ok' | 'err'>('idle');
  const [exportMessage, setExportMessage] = useState<string>('');
  const [visionModel, setVisionModel] = useState('');
  const [visionModels, setVisionModels] = useState<string[]>([]);
  const [visionModelStatus, setVisionModelStatus] = useState<'idle' | 'loading' | 'saving' | 'error'>('idle');
  const [visionModelMessage, setVisionModelMessage] = useState('');
  const exportTimerRef = useRef<number | null>(null);

  useEffect(() => () => {
    if (exportTimerRef.current) clearTimeout(exportTimerRef.current);
  }, []);

  useEffect(() => {
    if (!prefs?.contextVisionAnalysisEnabled) return;
    let cancelled = false;
    readCredential('ark.context_vision_model_id')
      .then(value => {
        if (!cancelled) setVisionModel(value ?? '');
      })
      .catch(error => {
        if (!cancelled) console.warn('[settings] failed to read context vision model', error);
      });
    return () => {
      cancelled = true;
    };
  }, [prefs?.contextVisionAnalysisEnabled]);

  const onExportLog = async () => {
    setExportStatus('busy');
    setExportMessage('');
    try {
      const ts = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
      const target = await exportErrorLog(`openless-${ts}.log`);
      if (target == null) {
        setExportStatus('idle');
        return;
      }
      setExportStatus('ok');
      setExportMessage(target);
      if (exportTimerRef.current) clearTimeout(exportTimerRef.current);
      exportTimerRef.current = window.setTimeout(() => setExportStatus('idle'), 4000);
    } catch (err) {
      setExportStatus('err');
      setExportMessage(err instanceof Error ? err.message : String(err));
    }
  };

  if (!prefs) {
    return (
      <Card>
        <div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div>
      </Card>
    );
  }

  const onRecordAudioForDebugChange = (recordAudioForDebug: boolean) =>
    savePrefs({ ...prefs, recordAudioForDebug });
  const onContextCaptureEnabledChange = (contextCaptureEnabled: boolean) =>
    savePrefs({ ...prefs, contextCaptureEnabled });
  const onContextVisionAnalysisEnabledChange = async (contextVisionAnalysisEnabled: boolean) => {
    if (!contextVisionAnalysisEnabled) {
      await savePrefs({ ...prefs, contextVisionAnalysisEnabled: false });
      return;
    }
    if (!prefs.contextVisionAnalysisConsentAccepted) {
      const accepted = window.confirm(
        t(
          'settings.debug.contextVisionAnalysisConsent',
          '开启后，VoxHub 会将本次截图和本次输入文本发送给当前配置的 LLM 服务，用于生成上下文摘要和对话名称。截图可能包含聊天、网页或文档内容。请确认你同意发送这些内容。',
        ),
      );
      if (!accepted) return;
    }
    await savePrefs({
      ...prefs,
      contextVisionAnalysisEnabled: true,
      contextVisionAnalysisConsentAccepted: true,
    });
  };

  const fetchVisionModels = async () => {
    setVisionModelStatus('loading');
    setVisionModelMessage('');
    try {
      const result = await listProviderModels('llm');
      setVisionModels(result.models);
      setVisionModelMessage(result.models.length > 0
        ? t('settings.providers.modelsLoaded', { count: result.models.length })
        : t('settings.providers.modelsEmpty'));
      setVisionModelStatus('idle');
    } catch (error) {
      setVisionModelStatus('error');
      setVisionModelMessage(error instanceof Error ? error.message : String(error));
    }
  };

  const saveVisionModel = async (model: string) => {
    setVisionModel(model);
    setVisionModelStatus('saving');
    setVisionModelMessage('');
    try {
      await setCredential('ark.context_vision_model_id', model);
      setVisionModelStatus('idle');
      setVisionModelMessage(model.trim()
        ? t('common.saved')
        : t('settings.debug.contextVisionNoModel', '未选择模型时不会进行截图分析'));
    } catch (error) {
      setVisionModelStatus('error');
      setVisionModelMessage(error instanceof Error ? error.message : String(error));
    }
  };

  // 留空视为不限制，落回 null → 后端走 200 默认。
  const onAudioRecordingMaxEntriesChange = (raw: string) => {
    const trimmed = raw.trim();
    if (trimmed === '') {
      void savePrefs({ ...prefs, audioRecordingMaxEntries: null });
      return;
    }
    const parsed = Number.parseInt(trimmed, 10);
    if (Number.isNaN(parsed)) return;
    void savePrefs({ ...prefs, audioRecordingMaxEntries: clamp(parsed, 1, 200) });
  };

  return (
    <Card>
      <SectionTitle>{t('settings.debug.title')}</SectionTitle>
      <SettingRow label={t('settings.recording.recordAudioForDebugLabel')}>
        <Toggle on={prefs.recordAudioForDebug} onToggle={onRecordAudioForDebugChange} />
      </SettingRow>
      <SettingRow label={t('settings.debug.contextCaptureLabel', '上下文采集')}>
        <Toggle on={prefs.contextCaptureEnabled} onToggle={onContextCaptureEnabledChange} />
      </SettingRow>
      <SettingRow label={t('settings.debug.contextVisionAnalysisLabel', '截图 AI 分析')}>
        <Toggle on={prefs.contextVisionAnalysisEnabled} onToggle={next => void onContextVisionAnalysisEnabledChange(next)} />
      </SettingRow>
      {prefs.contextVisionAnalysisEnabled && (
        <SettingRow label={t('settings.debug.contextVisionModelLabel', '截图分析模型')}>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 6, width: '100%', maxWidth: 420 }}>
            <div style={{ display: 'flex', gap: 6, alignItems: 'center', flexWrap: 'wrap' }}>
              <input
                value={visionModel}
                placeholder={t('settings.debug.contextVisionModelPlaceholder', '选择或输入多模态模型')}
                onChange={event => setVisionModel(event.target.value)}
                onBlur={() => void saveVisionModel(visionModel)}
                style={{ ...inputStyle, flex: '1 1 200px', minWidth: 0 }}
                disabled={visionModelStatus === 'loading' || visionModelStatus === 'saving'}
              />
              <Btn
                variant="ghost"
                size="sm"
                disabled={visionModelStatus === 'loading' || visionModelStatus === 'saving'}
                onClick={() => void fetchVisionModels()}
              >
                {visionModelStatus === 'loading' ? t('common.loading') : t('settings.providers.fetchModels')}
              </Btn>
              {visionModels.length > 0 && (
                <select
                  value=""
                  onChange={event => {
                    const model = event.target.value;
                    if (model) void saveVisionModel(model);
                  }}
                  style={{ ...inputStyle, flex: '1 1 160px', minWidth: 0 }}
                  disabled={visionModelStatus === 'loading' || visionModelStatus === 'saving'}
                >
                  <option value="">{t('settings.providers.selectModel')}</option>
                  {visionModels.map(model => (
                    <option key={model} value={model}>{model}</option>
                  ))}
                </select>
              )}
            </div>
            <div style={{ fontSize: 11, color: 'var(--ol-ink-4)', lineHeight: 1.45 }}>
              {t('settings.debug.contextVisionModelHint', '请确认该模型支持图片输入')}
              {!visionModel.trim() && (
                <span> · {t('settings.debug.contextVisionNoModel', '未选择模型时不会进行截图分析')}</span>
              )}
            </div>
            {visionModelMessage && (
              <div style={{ fontSize: 11, color: visionModelStatus === 'error' ? 'var(--ol-err)' : 'var(--ol-ink-4)', lineHeight: 1.45 }}>
                {visionModelMessage}
              </div>
            )}
          </div>
        </SettingRow>
      )}
      <SettingRow
        label={t('settings.debug.dailyReportScheduleLabel', '定时日报')}
        desc={t('settings.debug.dailyReportScheduleDesc', '开启后每天按设定时间生成日报，并只保存到历史报告。')}
      >
        <Toggle
          on={prefs.dailyReportScheduleEnabled}
          onToggle={dailyReportScheduleEnabled => void savePrefs({ ...prefs, dailyReportScheduleEnabled })}
        />
      </SettingRow>
      <SettingRow label={t('settings.debug.dailyReportScheduleTime', '日报时间')}>
        <input
          type="time"
          value={prefs.dailyReportScheduleTime || '18:00'}
          onChange={event => void savePrefs({ ...prefs, dailyReportScheduleTime: event.target.value || '18:00' })}
          style={{ ...inputStyle, width: 112 }}
          disabled={!prefs.dailyReportScheduleEnabled}
        />
      </SettingRow>
      <SettingRow label={t('settings.recording.audioRecordingMaxEntriesLabel')}>
        <input
          type="number"
          min={1}
          max={200}
          placeholder="200"
          value={prefs.audioRecordingMaxEntries ?? ''}
          onChange={e => onAudioRecordingMaxEntriesChange(e.target.value)}
          style={{ ...inputStyle, width: 80, textAlign: 'right' }}
          disabled={!prefs.recordAudioForDebug}
        />
      </SettingRow>
      <SettingRow label={t('modal.about.exportErrorLog')}>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
          <Btn variant="ghost" size="sm" disabled={exportStatus === 'busy'} onClick={onExportLog}>
            {exportStatus === 'busy' ? t('modal.about.exporting') : t('modal.about.exportErrorLogBtn')}
          </Btn>
          {exportStatus === 'ok' && (
            <span
              style={{ fontSize: 11, color: 'var(--ol-ok)', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis', maxWidth: 220 }}
              title={exportMessage}
            >
              {t('modal.about.exportSuccess')}
            </span>
          )}
          {exportStatus === 'err' && (
            <span
              style={{ fontSize: 11, color: 'var(--ol-err)', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis', maxWidth: 220 }}
              title={exportMessage}
            >
              {t('modal.about.exportFailed')}
            </span>
          )}
        </div>
      </SettingRow>
    </Card>
  );
}
