// 隐私 → 数据存储：本地保留的历史会话与对话上下文窗口。
// 自 Settings.tsx 的 RecordingSection「历史与上下文」折叠组拆出，逻辑零改动。

import { useTranslation } from 'react-i18next';
import { detectOS } from '../../components/WindowChrome';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { Card } from '../_atoms';
import { SettingRow, SectionTitle, Toggle, inputStyle } from './shared';

// 范围限制：retention 0-365 天，context window 0-60 分钟（再大对实际对话场景没意义且白烧 token）。
const clamp = (n: number, min: number, max: number) => Math.max(min, Math.min(max, n));

export function DataStorageSection() {
  const { t } = useTranslation();
  const { prefs, updatePrefs: savePrefs } = useHotkeySettings();

  if (!prefs) {
    return (
      <Card>
        <div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div>
      </Card>
    );
  }

  // 空字符串时回滚到默认值。
  const onHistoryRetentionChange = (raw: string) => {
    const parsed = raw === '' ? 0 : Number.parseInt(raw, 10);
    if (Number.isNaN(parsed)) return;
    void savePrefs({ ...prefs, historyRetentionDays: clamp(parsed, 0, 365) });
  };
  const onPolishContextWindowChange = (raw: string) => {
    const parsed = raw === '' ? 0 : Number.parseInt(raw, 10);
    if (Number.isNaN(parsed)) return;
    void savePrefs({ ...prefs, polishContextWindowMinutes: clamp(parsed, 0, 60) });
  };
  const onMeetingAudioRetentionChange = (raw: string) => {
    const parsed = raw === '' ? 0 : Number.parseInt(raw, 10);
    if (Number.isNaN(parsed)) return;
    void savePrefs({ ...prefs, meetingAudioRetentionCount: clamp(parsed, 0, 100) });
  };
  // 历史条数：默认 2000，最大 10000，最小 5。
  // 空字符串视为不限制，落回 null → 后端走 2000 默认。
  const onHistoryMaxEntriesChange = (raw: string) => {
    const trimmed = raw.trim();
    if (trimmed === '') {
      void savePrefs({ ...prefs, historyMaxEntries: null });
      return;
    }
    const parsed = Number.parseInt(trimmed, 10);
    if (Number.isNaN(parsed)) return;
    void savePrefs({ ...prefs, historyMaxEntries: clamp(parsed, 5, 10000) });
  };

  return (
    <Card>
      <SectionTitle>{t('settings.dataStorage.title')}</SectionTitle>
      <SettingRow label={t('settings.recording.historyRetentionLabel')}>
        <input
          type="number"
          min={0}
          max={365}
          value={prefs.historyRetentionDays}
          onChange={e => onHistoryRetentionChange(e.target.value)}
          style={{ ...inputStyle, width: 80, textAlign: 'right' }}
        />
      </SettingRow>
      <SettingRow label={t('settings.recording.historyMaxEntriesLabel')}>
        <input
          type="number"
          min={5}
          max={10000}
          placeholder="2000"
          value={prefs.historyMaxEntries ?? ''}
          onChange={e => onHistoryMaxEntriesChange(e.target.value)}
          style={{ ...inputStyle, width: 80, textAlign: 'right' }}
        />
      </SettingRow>
      <SettingRow label={t('settings.recording.meetingAudioRetentionLabel')}>
        <input
          type="number"
          min={0}
          max={100}
          value={prefs.meetingAudioRetentionCount}
          onChange={e => onMeetingAudioRetentionChange(e.target.value)}
          style={{ ...inputStyle, width: 80, textAlign: 'right' }}
        />
        <span style={{ fontSize: 11, color: 'var(--ol-ink-4)', lineHeight: 1.4 }}>
          {t('settings.recording.meetingAudioRetentionDesc')}
        </span>
      </SettingRow>
      <SettingRow label={t('settings.recording.polishContextWindowLabel')}>
        <input
          type="number"
          min={0}
          max={60}
          value={prefs.polishContextWindowMinutes}
          onChange={e => onPolishContextWindowChange(e.target.value)}
          style={{ ...inputStyle, width: 80, textAlign: 'right' }}
        />
      </SettingRow>
      {/* 光标上下文。放在「隐私」而不是「润色」下是有意的：这个开关真正的代价不是
          token，而是「把别的 app 里的文字发给 LLM 服务商」。只在 macOS 显示——
          其余平台没有实现，摆一个拨不动结果的开关只会误导。 */}
      {detectOS() === 'mac' && (
        <SettingRow
          label={t('settings.dataStorage.cursorContextLabel')}
          desc={t('settings.dataStorage.cursorContextDesc')}
        >
          <Toggle
            on={prefs.cursorContextEnabled}
            onToggle={next => void savePrefs({ ...prefs, cursorContextEnabled: next })}
          />
        </SettingRow>
      )}
    </Card>
  );
}
