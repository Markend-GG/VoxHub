// 服务 → 截图分析：上下文采集、截图 AI 分析、截图分析模型。
// 从 DebugToolsSection 迁入，作为独立服务分类与 LLM 润色平级。

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { listProviderModels, readCredential, setCredential } from '../../lib/ipc';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { Btn, Card } from '../_atoms';
import { SettingRow, Toggle, SectionTitle, inputStyle } from './shared';

export function ScreenshotAnalysisSection() {
  const { t } = useTranslation();
  const { prefs, updatePrefs: savePrefs } = useHotkeySettings();
  const [visionModel, setVisionModel] = useState('');
  const [visionModels, setVisionModels] = useState<string[]>([]);
  const [visionModelStatus, setVisionModelStatus] = useState<'idle' | 'loading' | 'saving' | 'error'>('idle');
  const [visionModelMessage, setVisionModelMessage] = useState('');

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

  if (!prefs) {
    return (
      <Card>
        <div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div>
      </Card>
    );
  }

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

  return (
    <Card>
      <SectionTitle>{t('settings.screenshotAnalysis.title')}</SectionTitle>
      <SettingRow label={t('settings.debug.contextCaptureLabel', '上下文采集')}>
        <Toggle on={prefs.contextCaptureEnabled} onToggle={onContextCaptureEnabledChange} />
      </SettingRow>
      <div style={{ fontSize: 11, color: 'var(--ol-ink-4)', lineHeight: 1.55, margin: '-6px 0 6px', padding: '0 0 0 2px' }}>
        {t('settings.screenshotAnalysis.contextCaptureHint', '采集当前前台应用的窗口标题和屏幕截图，用于录音/重写时自动记录上下文信息。')}
      </div>
      <SettingRow label={t('settings.debug.contextVisionAnalysisLabel', '截图 AI 分析')}>
        <Toggle on={prefs.contextVisionAnalysisEnabled} onToggle={next => void onContextVisionAnalysisEnabledChange(next)} />
      </SettingRow>
      <div style={{ fontSize: 11, color: 'var(--ol-ink-4)', lineHeight: 1.55, margin: '-6px 0 6px', padding: '0 0 0 2px' }}>
        {t('settings.screenshotAnalysis.visionAnalysisHint', '对截图进行 AI 智能分析，自动生成对话摘要、行动项等结构化内容。需配置支持图片输入的多模态模型。')}
      </div>
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
    </Card>
  );
}
