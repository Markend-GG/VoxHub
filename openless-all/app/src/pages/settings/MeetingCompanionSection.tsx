import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { SwitchLite } from '../../components/ui/SwitchLite';
import { emitSaved } from '../../lib/savedEvent';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { Card } from '../_atoms';
import { SectionTitle, SettingRow } from './shared';

type BooleanPreference = 'meetingCompanionEnabled' | 'meetingCompanionPositionLocked';

export function MeetingCompanionSection() {
  const { t } = useTranslation();
  const { prefs, updatePrefs, refresh } = useHotkeySettings();
  const [saving, setSaving] = useState<BooleanPreference | null>(null);

  if (!prefs) {
    return (
      <Card>
        <div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div>
      </Card>
    );
  }

  const saveBooleanPreference = async (key: BooleanPreference, value: boolean) => {
    setSaving(key);
    emitSaved('saving', t('common.saving'));
    try {
      await updatePrefs(current => ({ ...current, [key]: value }));
      emitSaved('saved', t('common.saved'));
    } catch (error) {
      console.error(`[meeting-companion] failed to save ${key}`, error);
      await refresh();
      emitSaved('failed', t('common.operationFailed'));
    } finally {
      setSaving(null);
    }
  };

  return (
    <Card>
      <SectionTitle>{t('settings.meetingCompanion.title')}</SectionTitle>
      <SettingRow label={t('settings.meetingCompanion.enabledLabel')}>
        <SwitchLite
          on={prefs.meetingCompanionEnabled}
          onToggle={next => void saveBooleanPreference('meetingCompanionEnabled', next)}
          disabled={saving !== null}
          ariaLabel={t('settings.meetingCompanion.enabledLabel')}
        />
      </SettingRow>
      <SettingRow label={t('settings.meetingCompanion.positionLockedLabel')}>
        <SwitchLite
          on={prefs.meetingCompanionPositionLocked}
          onToggle={next => void saveBooleanPreference('meetingCompanionPositionLocked', next)}
          disabled={saving !== null}
          ariaLabel={t('settings.meetingCompanion.positionLockedLabel')}
        />
      </SettingRow>
    </Card>
  );
}
