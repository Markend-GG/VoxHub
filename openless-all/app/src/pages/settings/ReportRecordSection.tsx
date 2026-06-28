// 服务 → 报告记录：截图白名单管理、按应用聚合分析、定时日报。
// 截图白名单从 PrivacyTab 迁入，聚合开关从 ScreenshotWhitelistSection 迁入，
// 日报功能从 DebugToolsSection 迁入。

import { useTranslation } from 'react-i18next';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { Card } from '../_atoms';
import { SettingRow, Toggle, SectionTitle, inputStyle } from './shared';
import { ScreenshotWhitelistSection } from './ScreenshotWhitelistSection';
import {
  setScreenshotAppAggregationEnabled,
} from '../../lib/ipc';

export function ReportRecordSection() {
  const { t } = useTranslation();
  const { prefs, updatePrefs: savePrefs, refresh } = useHotkeySettings();

  if (!prefs) {
    return (
      <Card>
        <div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div>
      </Card>
    );
  }

  return (
    <>
      <Card>
        <SectionTitle>{t('settings.reportRecord.title')}</SectionTitle>
        <SettingRow label={t('settings.screenshotAggregation.label')}>
          <Toggle
            on={prefs.screenshotAppAggregationEnabled ?? false}
            onToggle={async (next) => {
              try {
                await setScreenshotAppAggregationEnabled(next);
                await refresh();
              } catch (err) {
                console.error('[agg] toggle failed:', err);
              }
            }}
          />
        </SettingRow>
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
      </Card>
      <ScreenshotWhitelistSection />
    </>
  );
}
