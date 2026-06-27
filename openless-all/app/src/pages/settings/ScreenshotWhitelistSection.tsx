// 设置 → 隐私 → 截图白名单管理。
// V1 仅在 Windows 展示；macOS/Linux 不展示此入口。

import { useState, useEffect, useCallback, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { useHotkeySettings } from '../../state/HotkeySettingsContext';
import { Card } from '../_atoms';
import { SettingRow, SectionTitle } from './shared';
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
  const { prefs, updatePrefs: savePrefs, refresh } = useHotkeySettings();
  const [showModal, setShowModal] = useState(false);
  const os = detectOS();

  // V1 只在 Windows 展示
  if (os !== 'win' || !prefs) return null;

  return (
    <>
      <Card>
        <SectionTitle>{t('settings.screenshotWhitelist.title')}</SectionTitle>
        <SettingRow
          label={t('settings.screenshotWhitelist.desc')}
        >
          <button
            type="button"
            onClick={() => setShowModal(true)}
            style={manageBtnStyle}
          >
            {t('settings.screenshotWhitelist.manage')}
          </button>
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
          <h3 style={{ margin: 0, fontSize: 16, fontWeight: 600 }}>
            {t('settings.screenshotWhitelist.modalTitle')}
          </h3>
          <button type="button" onClick={onClose} style={closeBtnStyle}>
            ✕
          </button>
        </div>

        {/* 开关 */}
        <div style={toggleRowStyle}>
          <div style={{ flex: 1 }}>
            <div style={{ fontSize: 13, fontWeight: 600 }}>
              {t('settings.screenshotWhitelist.toggleLabel')}
            </div>
            <div style={{ fontSize: 11.5, color: 'var(--ol-ink-3)', marginTop: 2 }}>
              {enabled
                ? t('settings.screenshotWhitelist.toggleDescOn')
                : t('settings.screenshotWhitelist.toggleDescOff')}
            </div>
          </div>
          <label style={{ position: 'relative', display: 'inline-block', width: 40, height: 22 }}>
            <input
              type="checkbox"
              checked={enabled}
              onChange={e => handleToggle(e.target.checked)}
              style={{ opacity: 0, width: 0, height: 0, position: 'absolute' }}
            />
            <span style={{
              position: 'absolute', cursor: 'pointer', inset: 0,
              background: enabled ? 'var(--ol-blue)' : 'var(--ol-ink-4)',
              borderRadius: 22, transition: 'background 0.2s',
            }}>
              <span style={{
                position: 'absolute', height: 16, width: 16, left: enabled ? 20 : 3, top: 3,
                background: '#fff', borderRadius: '50%', transition: 'left 0.2s',
              }} />
            </span>
          </label>
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
        <div style={{ flex: 1, minHeight: 0, overflow: 'auto', margin: '10px 0' }}>
          {apps.length === 0 ? (
            <div style={{ fontSize: 12, color: 'var(--ol-ink-4)', padding: '16px 0', textAlign: 'center' }}>
              {t('settings.screenshotWhitelist.emptyState')}
            </div>
          ) : (
            <table style={tableStyle}>
              <thead>
                <tr>
                  <th style={thStyle}>{t('settings.screenshotWhitelist.appName')}</th>
                  <th style={thStyle}>{t('settings.screenshotWhitelist.processName')}</th>
                  <th style={{ ...thStyle, width: 60 }}>{t('settings.screenshotWhitelist.source')}</th>
                  <th style={{ ...thStyle, width: 50 }} />
                </tr>
              </thead>
              <tbody>
                {apps.map(app => (
                  <tr key={app.id}>
                    <td style={tdStyle}>{app.displayName || app.processName}</td>
                    <td style={{ ...tdStyle, color: 'var(--ol-ink-4)', fontSize: 11.5 }}>
                      {app.processName}
                    </td>
                    <td style={{ ...tdStyle, fontSize: 11, color: 'var(--ol-ink-4)' }}>
                      {app.source === 'default'
                        ? t('settings.screenshotWhitelist.sourceDefault')
                        : t('settings.screenshotWhitelist.sourceUser')}
                    </td>
                    <td style={tdStyle}>
                      <button
                        type="button"
                        onClick={() => handleDelete(app.processName)}
                        style={deleteBtnStyle}
                        title={t('settings.screenshotWhitelist.delete')}
                      >
                        ✕
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        {/* 操作按钮 */}
        <div style={actionsStyle}>
          <button type="button" onClick={() => setShowAddModal(true)} style={primaryBtnStyle}>
            {t('settings.screenshotWhitelist.addApp')}
          </button>
          <button type="button" onClick={handleRestoreDefaults} style={secondaryBtnStyle}>
            {t('settings.screenshotWhitelist.restoreDefaults')}
          </button>
          <button type="button" onClick={onClose} style={secondaryBtnStyle}>
            {t('settings.screenshotWhitelist.close')}
          </button>
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
      <div style={{ ...modalCardStyle, maxWidth: 500 }} onClick={e => e.stopPropagation()}>
        <div style={modalHeaderStyle}>
          <h3 style={{ margin: 0, fontSize: 15, fontWeight: 600 }}>
            {t('settings.screenshotWhitelist.addAppTitle')}
          </h3>
          <button type="button" onClick={onClose} style={closeBtnStyle}>✕</button>
        </div>
        <div style={{ fontSize: 12, color: 'var(--ol-ink-3)', marginBottom: 8 }}>
          {t('settings.screenshotWhitelist.addAppDesc')}
        </div>
        <div style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: 8 }}>
          <button type="button" onClick={loadApps} style={refreshBtnStyle} disabled={loading}>
            {t('settings.screenshotWhitelist.refresh')}
          </button>
        </div>
        {error && (
          <div style={{ ...warningStyle, marginBottom: 8 }}>{error}</div>
        )}
        <div style={{ flex: 1, minHeight: 0, overflow: 'auto', maxHeight: 300 }}>
          {loading ? (
            <div style={{ fontSize: 12, color: 'var(--ol-ink-4)', textAlign: 'center', padding: 20 }}>
              {t('common.loading')}
            </div>
          ) : openApps.length === 0 ? (
            <div style={{ fontSize: 12, color: 'var(--ol-ink-4)', textAlign: 'center', padding: 20 }}>
              {t('settings.screenshotWhitelist.noVisibleApps')}
            </div>
          ) : (
            <table style={tableStyle}>
              <thead>
                <tr>
                  <th style={thStyle}>{t('settings.screenshotWhitelist.appName')}</th>
                  <th style={thStyle}>{t('settings.screenshotWhitelist.processName')}</th>
                  <th style={{ ...thStyle, maxWidth: 120 }}>{t('settings.screenshotWhitelist.windowTitle')}</th>
                  <th style={{ ...thStyle, width: 70 }} />
                </tr>
              </thead>
              <tbody>
                {openApps.map((app, idx) => {
                  const alreadyAdded = existingProcessNames.has(app.processName.toLowerCase());
                  return (
                    <tr key={`${app.processName}-${app.processId}-${idx}`}>
                      <td style={tdStyle}>{app.displayName}</td>
                      <td style={{ ...tdStyle, color: 'var(--ol-ink-4)', fontSize: 11.5 }}>
                        {app.processName}
                      </td>
                      <td style={{ ...tdStyle, fontSize: 11, color: 'var(--ol-ink-4)', maxWidth: 120, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                        {app.windowTitle ?? ''}
                      </td>
                      <td style={tdStyle}>
                        {alreadyAdded ? (
                          <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>
                            {t('settings.screenshotWhitelist.alreadyAdded')}
                          </span>
                        ) : (
                          <button
                            type="button"
                            onClick={() => handleAdd(app)}
                            style={addBtnStyle}
                          >
                            {t('settings.screenshotWhitelist.add')}
                          </button>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </div>
      </div>
    </div>
  );
}

// ─── 样式 ──────────────────────────────────────────────────────────

const manageBtnStyle: CSSProperties = {
  padding: '5px 14px',
  borderRadius: 8,
  border: '0.5px solid var(--ol-line)',
  background: 'var(--ol-surface)',
  color: 'var(--ol-ink)',
  fontSize: 12,
  fontWeight: 500,
  cursor: 'default',
  fontFamily: 'inherit',
};

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
  borderRadius: 14,
  border: '0.5px solid var(--ol-line)',
  boxShadow: 'var(--ol-shadow-xl)',
  display: 'flex',
  flexDirection: 'column',
  padding: 20,
  animation: 'ol-modal-card-in 0.24s var(--ol-motion-spring)',
};

const modalHeaderStyle: CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  marginBottom: 14,
};

const closeBtnStyle: CSSProperties = {
  width: 28,
  height: 28,
  border: 0,
  borderRadius: 999,
  background: 'transparent',
  color: 'var(--ol-ink-3)',
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  cursor: 'default',
  fontSize: 14,
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

const tableStyle: CSSProperties = {
  width: '100%',
  borderCollapse: 'collapse',
  fontSize: 12.5,
};

const thStyle: CSSProperties = {
  textAlign: 'left',
  padding: '6px 8px',
  fontWeight: 600,
  fontSize: 11,
  color: 'var(--ol-ink-4)',
  borderBottom: '0.5px solid var(--ol-line-soft)',
};

const tdStyle: CSSProperties = {
  padding: '6px 8px',
  borderBottom: '0.5px solid var(--ol-line-soft)',
};

const deleteBtnStyle: CSSProperties = {
  width: 22,
  height: 22,
  border: 0,
  borderRadius: 999,
  background: 'transparent',
  color: 'var(--ol-ink-4)',
  cursor: 'default',
  fontSize: 11,
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
};

const actionsStyle: CSSProperties = {
  display: 'flex',
  gap: 8,
  marginTop: 12,
  paddingTop: 12,
  borderTop: '0.5px solid var(--ol-line-soft)',
};

const primaryBtnStyle: CSSProperties = {
  padding: '7px 16px',
  borderRadius: 8,
  border: 0,
  background: 'var(--ol-blue)',
  color: '#fff',
  fontSize: 12.5,
  fontWeight: 600,
  cursor: 'default',
  fontFamily: 'inherit',
};

const secondaryBtnStyle: CSSProperties = {
  padding: '7px 16px',
  borderRadius: 8,
  border: '0.5px solid var(--ol-line)',
  background: 'var(--ol-surface)',
  color: 'var(--ol-ink)',
  fontSize: 12.5,
  fontWeight: 500,
  cursor: 'default',
  fontFamily: 'inherit',
};

const refreshBtnStyle: CSSProperties = {
  padding: '4px 10px',
  borderRadius: 6,
  border: '0.5px solid var(--ol-line)',
  background: 'var(--ol-surface)',
  color: 'var(--ol-ink-3)',
  fontSize: 11.5,
  cursor: 'default',
  fontFamily: 'inherit',
};

const addBtnStyle: CSSProperties = {
  padding: '3px 10px',
  borderRadius: 6,
  border: 0,
  background: 'var(--ol-blue)',
  color: '#fff',
  fontSize: 11.5,
  fontWeight: 600,
  cursor: 'default',
  fontFamily: 'inherit',
};
