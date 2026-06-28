// 设置 → 报告记录 → 截图白名单管理。
// V1 仅在 Windows 展示；macOS/Linux 不展示此入口。

import { useState, useEffect, useCallback, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { Card, Btn, Pill } from '../_atoms';
import { SettingRow, SectionTitle, Toggle } from './shared';
import {
  listOpenWindowApps,
  setScreenshotWhitelistEnabled,
  addScreenshotWhitelistApp,
  removeScreenshotWhitelistApp,
  restoreDefaultScreenshotWhitelistApps,
} from '../../lib/ipc';
import type {
  OpenWindowApp,
  ScreenshotWhitelistApp,
} from '../../lib/types';
import { detectOS } from '../../components/WindowChrome';

// ─── 浏览器进程名列表（用于风险提示） ────────────────────────────
const BROWSER_PROCESS_NAMES = new Set(['chrome.exe', 'msedge.exe', 'firefox.exe', 'opera.exe', 'brave.exe']);

export function ScreenshotWhitelistSection() {
  const { t } = useTranslation();
  const { prefs } = useHotkeySettings();
  const [showModal, setShowModal] = useState(false);
  const os = detectOS();

  // V1 只在 Windows 展示
  if (os !== 'win' || !prefs) return null;

  return (
    <>
      <Card>
        <SectionTitle>{t('settings.screenshotWhitelist.title')}</SectionTitle>
        <div style={{ fontSize: 11.5, color: 'var(--ol-ink-4)', lineHeight: 1.6, marginBottom: 6 }}>
          {t('settings.screenshotWhitelist.desc')}
        </div>
        <SettingRow
          label={t('settings.screenshotWhitelist.title')}
        >
          <Btn variant="ghost" size="sm" onClick={() => setShowModal(true)}>
            {t('settings.screenshotWhitelist.manage')}
          </Btn>
        </SettingRow>
      </Card>
      {showModal && (
        <WhitelistModal
          onClose={() => setShowModal(false)}
        />
      )}
    </>
  );
}

// ─── 白名单管理弹窗 ───────────────────────────────────────────────

function WhitelistModal({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const { prefs, refresh } = useHotkeySettings();
  const [showAddModal, setShowAddModal] = useState(false);

  if (!prefs) return null;

  const apps = prefs.screenshotWhitelistApps;
  const enabled = prefs.screenshotWhitelistEnabled;

  // 检查是否有浏览器在白名单中
  const hasBrowserInWhitelist = apps.some(app =>
    BROWSER_PROCESS_NAMES.has(app.processName.toLowerCase())
  );

  const handleToggle = async (nextEnabled: boolean) => {
    try {
      await setScreenshotWhitelistEnabled(nextEnabled);
      await refresh();
    } catch (err) {
      console.error('[whitelist] toggle failed:', err);
    }
  };

  const handleDelete = async (processName: string) => {
    try {
      await removeScreenshotWhitelistApp(processName);
      await refresh();
    } catch (err) {
      console.error('[whitelist] delete failed:', err);
    }
  };

  const handleRestoreDefaults = async () => {
    try {
      await restoreDefaultScreenshotWhitelistApps();
      await refresh();
    } catch (err) {
      console.error('[whitelist] restore defaults failed:', err);
    }
  };

  return (
    <div style={modalOverlayStyle} onClick={onClose}>
      <div style={modalCardStyle} onClick={e => e.stopPropagation()}>
        <div style={modalHeaderStyle}>
          <h3 style={{ margin: 0, fontSize: 15, fontWeight: 600, color: 'var(--ol-ink)' }}>
            {t('settings.screenshotWhitelist.modalTitle')}
          </h3>
          <Btn variant="ghost" size="sm" onClick={onClose} style={{ minWidth: 28, padding: '4px 6px' }}>
            ✕
          </Btn>
        </div>

        {/* 开关 */}
        <div style={toggleRowStyle}>
          <div style={{ flex: 1 }}>
            <div style={{ fontSize: 13, fontWeight: 500, color: 'var(--ol-ink)' }}>
              {t('settings.screenshotWhitelist.toggleLabel')}
            </div>
            <div style={{ fontSize: 11.5, color: 'var(--ol-ink-3)', marginTop: 2 }}>
              {enabled
                ? t('settings.screenshotWhitelist.toggleDescOn')
                : t('settings.screenshotWhitelist.toggleDescOff')}
            </div>
          </div>
          <Toggle on={enabled} onToggle={handleToggle} />
        </div>

        {/* 关闭白名单时的警告 */}
        {!enabled && (
          <div style={warningStyle}>
            {t('settings.screenshotWhitelist.disableWarning')}
          </div>
        )}

        {/* 浏览器风险提示 */}
        {hasBrowserInWhitelist && enabled && (
          <div style={browserWarningStyle}>
            {t('settings.screenshotWhitelist.browserWarning')}
          </div>
        )}

        {/* 白名单列表 */}
        <div style={listContainerStyle}>
          {apps.length === 0 ? (
            <div style={{ fontSize: 12, color: 'var(--ol-ink-4)', padding: '20px 0', textAlign: 'center' }}>
              {t('settings.screenshotWhitelist.emptyState')}
            </div>
          ) : (
            <div>
              {/* 表头 */}
              <div style={listHeaderStyle}>
                <span style={{ flex: '1.2', minWidth: 0 }}>{t('settings.screenshotWhitelist.appName')}</span>
                <span style={{ flex: '1', minWidth: 0 }}>{t('settings.screenshotWhitelist.processName')}</span>
                <span style={{ width: 56, textAlign: 'center' }}>{t('settings.screenshotWhitelist.source')}</span>
                <span style={{ width: 36 }} />
              </div>
              {/* 列表项 */}
              {apps.map(app => (
                <div key={app.id} style={listItemStyle}>
                  <span style={{ flex: '1.2', minWidth: 0, fontSize: 12.5, fontWeight: 500, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {app.displayName || app.processName}
                  </span>
                  <span style={{ flex: '1', minWidth: 0, fontSize: 11.5, color: 'var(--ol-ink-4)', fontFamily: 'var(--ol-font-mono)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {app.processName}
                  </span>
                  <span style={{ width: 56, textAlign: 'center' }}>
                    <Pill size="sm" tone={app.source === 'default' ? 'outline' : 'blue'}>
                      {app.source === 'default'
                        ? t('settings.screenshotWhitelist.sourceDefault')
                        : t('settings.screenshotWhitelist.sourceUser')}
                    </Pill>
                  </span>
                  <span style={{ width: 36, display: 'flex', justifyContent: 'center' }}>
                    <Btn
                      variant="ghost"
                      size="sm"
                      onClick={() => handleDelete(app.processName)}
                      style={{ minWidth: 24, padding: '3px 5px', fontSize: 11, color: 'var(--ol-ink-4)' }}
                    >
                      ✕
                    </Btn>
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* 操作按钮 */}
        <div style={actionsStyle}>
          <Btn variant="blue" size="sm" onClick={() => setShowAddModal(true)}>
            {t('settings.screenshotWhitelist.addApp')}
          </Btn>
          <Btn variant="ghost" size="sm" onClick={handleRestoreDefaults}>
            {t('settings.screenshotWhitelist.restoreDefaults')}
          </Btn>
          <Btn variant="ghost" size="sm" onClick={onClose}>
            {t('settings.screenshotWhitelist.close')}
          </Btn>
        </div>
      </div>

      {showAddModal && (
        <AddAppModal
          currentApps={apps}
          onClose={() => setShowAddModal(false)}
        />
      )}
    </div>
  );
}

// ─── 添加应用弹窗 ──────────────────────────────────────────────────

function AddAppModal({
  currentApps,
  onClose,
}: {
  currentApps: ScreenshotWhitelistApp[];
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const { refresh } = useHotkeySettings();
  const [openApps, setOpenApps] = useState<OpenWindowApp[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadApps = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const apps = await listOpenWindowApps();
      setOpenApps(apps);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { void loadApps(); }, [loadApps]);

  const existingProcessNames = new Set(
    currentApps.map(a => a.processName.toLowerCase())
  );

  const handleAdd = async (app: OpenWindowApp) => {
    try {
      await addScreenshotWhitelistApp({
        displayName: app.displayName,
        processName: app.processName,
        exePath: app.exePath ?? null,
      });
      await refresh();
      onClose();
    } catch (err) {
      const errCode = String(err);
      setError(
        errCode === 'appAlreadyInWhitelist'
          ? t('settings.screenshotWhitelist.alreadyAdded')
          : t('settings.screenshotWhitelist.addFailed', { message: errCode })
      );
    }
  };

  return (
    <div style={modalOverlayStyle} onClick={onClose}>
      <div style={{ ...modalCardStyle, maxWidth: 520 }} onClick={e => e.stopPropagation()}>
        <div style={modalHeaderStyle}>
          <h3 style={{ margin: 0, fontSize: 15, fontWeight: 600, color: 'var(--ol-ink)' }}>
            {t('settings.screenshotWhitelist.addAppTitle')}
          </h3>
          <Btn variant="ghost" size="sm" onClick={onClose} style={{ minWidth: 28, padding: '4px 6px' }}>
            ✕
          </Btn>
        </div>
        <div style={{ fontSize: 12, color: 'var(--ol-ink-3)', marginBottom: 8 }}>
          {t('settings.screenshotWhitelist.addAppDesc')}
        </div>
        <div style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: 8 }}>
          <Btn variant="ghost" size="sm" onClick={loadApps} disabled={loading}>
            {t('settings.screenshotWhitelist.refresh')}
          </Btn>
        </div>
        {error && (
          <div style={{ ...warningStyle, marginBottom: 8 }}>{error}</div>
        )}
        <div style={{ flex: 1, minHeight: 0, overflow: 'auto', maxHeight: 320 }}>
          {loading ? (
            <div style={{ fontSize: 12, color: 'var(--ol-ink-4)', textAlign: 'center', padding: 24 }}>
              {t('common.loading')}
            </div>
          ) : openApps.length === 0 ? (
            <div style={{ fontSize: 12, color: 'var(--ol-ink-4)', textAlign: 'center', padding: 24 }}>
              {t('settings.screenshotWhitelist.noVisibleApps')}
            </div>
          ) : (
            <div>
              {/* 表头 */}
              <div style={listHeaderStyle}>
                <span style={{ flex: '1.2', minWidth: 0 }}>{t('settings.screenshotWhitelist.appName')}</span>
                <span style={{ flex: '1', minWidth: 0 }}>{t('settings.screenshotWhitelist.processName')}</span>
                <span style={{ flex: '1.2', minWidth: 0 }}>{t('settings.screenshotWhitelist.windowTitle')}</span>
                <span style={{ width: 60, textAlign: 'center' }} />
              </div>
              {/* 列表项 */}
              {openApps.map((app, idx) => {
                const alreadyAdded = existingProcessNames.has(app.processName.toLowerCase());
                return (
                  <div key={`${app.processName}-${app.processId}-${idx}`} style={listItemStyle}>
                    <span style={{ flex: '1.2', minWidth: 0, fontSize: 12.5, fontWeight: 500, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                      {app.displayName}
                    </span>
                    <span style={{ flex: '1', minWidth: 0, fontSize: 11.5, color: 'var(--ol-ink-4)', fontFamily: 'var(--ol-font-mono)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                      {app.processName}
                    </span>
                    <span style={{ flex: '1.2', minWidth: 0, fontSize: 11, color: 'var(--ol-ink-4)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                      {app.windowTitle ?? ''}
                    </span>
                    <span style={{ width: 60, display: 'flex', justifyContent: 'center' }}>
                      {alreadyAdded ? (
                        <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>
                          {t('settings.screenshotWhitelist.alreadyAdded')}
                        </span>
                      ) : (
                        <Btn variant="blue" size="sm" onClick={() => handleAdd(app)} style={{ padding: '3px 10px', fontSize: 11.5 }}>
                          {t('settings.screenshotWhitelist.add')}
                        </Btn>
                      )}
                    </span>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// ─── 样式 ──────────────────────────────────────────────────────────

const modalOverlayStyle: CSSProperties = {
  position: 'fixed',
  inset: 0,
  background: 'var(--ol-overlay-bg)',
  backdropFilter: 'blur(8px) saturate(140%)',
  WebkitBackdropFilter: 'blur(8px) saturate(140%)',
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'center',
  zIndex: 100,
  animation: 'ol-modal-backdrop-in 0.18s var(--ol-motion-soft)',
};

const modalCardStyle: CSSProperties = {
  width: '90%',
  maxWidth: 640,
  maxHeight: '80vh',
  background: 'var(--ol-surface)',
  borderRadius: 'var(--ol-r-lg)',
  border: '0.5px solid var(--ol-line)',
  boxShadow: 'var(--ol-shadow-xl)',
  display: 'flex',
  flexDirection: 'column' as const,
  padding: 20,
  animation: 'ol-modal-card-in 0.24s var(--ol-motion-spring)',
};

const modalHeaderStyle: CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  marginBottom: 14,
};

const toggleRowStyle: CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: 12,
  padding: '10px 0',
  borderBottom: '0.5px solid var(--ol-line-soft)',
};

const warningStyle: CSSProperties = {
  fontSize: 11.5,
  color: 'var(--ol-red, #d32f2f)',
  background: 'rgba(211, 47, 47, 0.06)',
  borderRadius: 8,
  padding: '8px 10px',
  marginTop: 8,
  lineHeight: 1.5,
};

const browserWarningStyle: CSSProperties = {
  fontSize: 11.5,
  color: 'var(--ol-amber, #f57c00)',
  background: 'rgba(245, 124, 0, 0.06)',
  borderRadius: 8,
  padding: '8px 10px',
  marginTop: 8,
  lineHeight: 1.5,
};

const listContainerStyle: CSSProperties = {
  flex: 1,
  minHeight: 0,
  overflow: 'auto',
  margin: '10px 0',
  borderRadius: 8,
  border: '0.5px solid var(--ol-line-soft)',
};

const listHeaderStyle: CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: 8,
  padding: '7px 10px',
  fontSize: 11,
  fontWeight: 600,
  color: 'var(--ol-ink-4)',
  background: 'var(--ol-surface-2)',
  borderBottom: '0.5px solid var(--ol-line-soft)',
};

const listItemStyle: CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: 8,
  padding: '7px 10px',
  borderBottom: '0.5px solid var(--ol-line-soft)',
  transition: 'background 0.12s',
};

const actionsStyle: CSSProperties = {
  display: 'flex',
  gap: 8,
  marginTop: 12,
  paddingTop: 12,
  borderTop: '0.5px solid var(--ol-line-soft)',
};
