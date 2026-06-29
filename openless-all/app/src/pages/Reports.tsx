import { useEffect, useMemo, useRef, useState, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import {
  deleteGeneratedReport,
  deleteReportTemplate,
  generateReport,
  listGeneratedReports,
  listReportTemplates,
  saveReportTemplate,
  updateGeneratedReport,
} from '../lib/ipc';
import { emitSaved } from '../lib/savedEvent';
import type { GeneratedReport, ReportTemplate, ReportType } from '../lib/types';
import { useHotkeySettings } from '../state/HotkeySettingsContext';
import { Btn, Card, PageHeader, Pill } from './_atoms';
import { inputStyle } from './settings/shared';

const REPORT_TYPES: ReportType[] = ['daily', 'weekly', 'monthly'];

export function Reports() {
  const { t } = useTranslation();
  const { prefs, updatePrefs } = useHotkeySettings();
  const [reportType, setReportType] = useState<ReportType>('daily');
  const [templates, setTemplates] = useState<ReportTemplate[]>([]);
  const [reports, setReports] = useState<GeneratedReport[]>([]);
  const [templateId, setTemplateId] = useState('');
  const [rangeStart, setRangeStart] = useState(() => localDateTimeValue(startOfToday()));
  const [rangeEnd, setRangeEnd] = useState(() => localDateTimeValue(new Date()));
  const [mainWork, setMainWork] = useState('');
  const [selectedReportId, setSelectedReportId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [templateDraft, setTemplateDraft] = useState('');
  const [templateNameDraft, setTemplateNameDraft] = useState('');
  const [showGenerateModal, setShowGenerateModal] = useState(false);

  const selectedTemplate = useMemo(
    () => templates.find(template => template.id === templateId) ?? null,
    [templates, templateId],
  );
  const selectedReport = useMemo(
    () => reports.find(report => report.id === selectedReportId) ?? reports[0] ?? null,
    [reports, selectedReportId],
  );
  const typedTemplates = templates.filter(template => template.reportType === reportType);

  const refresh = async () => {
    setLoading(true);
    setError(null);
    try {
      const [templateList, reportList] = await Promise.all([
        listReportTemplates(),
        listGeneratedReports(),
      ]);
      setTemplates(templateList);
      setReports(reportList);
      setSelectedReportId(prev => (prev && reportList.some(report => report.id === prev) ? prev : reportList[0]?.id ?? null));
      const preferred = preferredTemplateId(reportType, prefs);
      const nextTemplate =
        templateList.find(template => template.id === preferred && template.reportType === reportType)
        ?? templateList.find(template => template.reportType === reportType)
        ?? null;
      setTemplateId(prev => {
        if (prev && templateList.some(template => template.id === prev && template.reportType === reportType)) return prev;
        return nextTemplate?.id ?? '';
      });
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  useEffect(() => {
    const preferred = preferredTemplateId(reportType, prefs);
    const next =
      templates.find(template => template.id === preferred && template.reportType === reportType)
      ?? templates.find(template => template.reportType === reportType)
      ?? null;
    setTemplateId(next?.id ?? '');
  }, [reportType, templates, prefs?.selectedDailyReportTemplateId, prefs?.selectedWeeklyReportTemplateId, prefs?.selectedMonthlyReportTemplateId]);

  useEffect(() => {
    setTemplateDraft(selectedTemplate?.content ?? '');
    setTemplateNameDraft(selectedTemplate?.name ?? '');
  }, [selectedTemplate?.id]);

  const onTemplateChange = async (id: string) => {
    setTemplateId(id);
    if (!prefs) return;
    const nextPrefs = { ...prefs };
    if (reportType === 'daily') nextPrefs.selectedDailyReportTemplateId = id;
    if (reportType === 'weekly') nextPrefs.selectedWeeklyReportTemplateId = id;
    if (reportType === 'monthly') nextPrefs.selectedMonthlyReportTemplateId = id;
    await updatePrefs(nextPrefs);
  };

  const onSaveTemplate = async () => {
    if (!selectedTemplate) return;
    const name = templateNameDraft.trim() || selectedTemplate.name;
    const id = selectedTemplate.isBuiltin ? crypto.randomUUID() : selectedTemplate.id;
    const saved = await saveReportTemplate({
      ...selectedTemplate,
      id,
      name,
      content: templateDraft,
      isBuiltin: false,
      createdAt: selectedTemplate.isBuiltin ? '' : selectedTemplate.createdAt,
      updatedAt: '',
    });
    const list = await listReportTemplates();
    setTemplates(list);
    await onTemplateChange(saved.id);
  };

  const onDeleteTemplate = async () => {
    if (!selectedTemplate || selectedTemplate.isBuiltin) return;
    await deleteReportTemplate(selectedTemplate.id);
    const list = await listReportTemplates();
    setTemplates(list);
  };

  const onGenerate = async () => {
    if (!templateId) return;
    setError(null);

    // 1. 立即关闭弹窗
    setShowGenerateModal(false);

    // 2. Toast 提示
    emitSaved('saving', t('reports.generatingToast', '报告生成中，请稍后再看'));

    // 3. 生成占位记录 ID 并插入本地列表
    const placeholderId = crypto.randomUUID();
    const now = new Date().toISOString();
    const placeholder: GeneratedReport = {
      id: placeholderId,
      reportType,
      title: `${reportTypeLabel(reportType)} ${new Date().toLocaleDateString()}`,
      rangeStart: new Date(rangeStart).toISOString(),
      rangeEnd: new Date(rangeEnd).toISOString(),
      templateId,
      templateName: selectedTemplate?.name ?? '',
      templateContent: selectedTemplate?.content ?? '',
      userMainWork: mainWork.trim() || null,
      status: 'pending',
      content: null,
      sourceStats: { voiceCount: 0, rewriteCount: 0, screenshotRecordCount: 0, analyzedScreenshotRecordCount: 0 },
      errorCode: null,
      errorMessage: null,
      createdAt: now,
      updatedAt: now,
    };
    setReports(prev => [placeholder, ...prev]);
    setSelectedReportId(placeholderId);

    // 4. 后台执行生成（fire-and-forget）
    generateReport({
      reportType,
      rangeStart: new Date(rangeStart).toISOString(),
      rangeEnd: new Date(rangeEnd).toISOString(),
      templateId,
      userMainWork: mainWork.trim() || null,
    }).then(async (result) => {
      // 生成完成，刷新报告列表
      try {
        const list = await listGeneratedReports();
        setReports(list);
        // 如果后端返回的 ID 与占位不同，选中新记录
        setSelectedReportId(result.id);
        if (result.status === 'success') {
          emitSaved('saved', t('reports.generateSuccess', '报告生成完成'));
        } else if (result.status === 'failed') {
          emitSaved('failed', result.errorMessage ?? t('reports.generateFailed', '报告生成失败'));
        }
      } catch (err) {
        setError(errorMessage(err));
      }
    }).catch((err) => {
      // 生成异常：移除占位记录，同步修正选中项（避免闭包中 reports 过期）
      setReports(prev => {
        const next = prev.filter(r => r.id !== placeholderId);
        setSelectedReportId(cur => cur === placeholderId ? (next[0]?.id ?? null) : cur);
        return next;
      });
      setError(errorMessage(err));
      emitSaved('failed', t('reports.generateFailed', '报告生成失败'));
    });
  };

  const onDeleteReport = async (id: string) => {
    await deleteGeneratedReport(id);
    const list = await listGeneratedReports();
    setReports(list);
    setSelectedReportId(prev => (prev === id ? list[0]?.id ?? null : prev));
  };

  const onSaveReport = async (id: string, title: string, content: string) => {
    try {
      const updated = await updateGeneratedReport(id, title, content);
      const list = await listGeneratedReports();
      setReports(list);
      setSelectedReportId(updated.id);
      emitSaved('saved', t('common.saved'));
    } catch (err) {
      emitSaved('failed', t('common.operationFailed'));
      setError(errorMessage(err));
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}>
      <PageHeader
        kicker={t('reports.kicker', 'REPORTS')}
        title={t('reports.title', '报告')}
        desc={t('reports.desc', '基于语音历史、重写历史、截图记录和完整摘要生成日报、周报、月报。')}
        right={
          <div style={{ display: 'flex', gap: 6 }}>
            <Btn variant="blue" size="sm" icon="sparkle" onClick={() => setShowGenerateModal(true)}>
              {t('reports.generate', '生成报告')}
            </Btn>
            <Btn icon="refresh" variant="ghost" size="sm" onClick={() => void refresh()}>{t('common.refresh')}</Btn>
          </div>
        }
      />
      {error && (
        <div style={{ marginBottom: 12, padding: '9px 10px', borderRadius: 8, background: 'rgba(239,68,68,0.08)', color: 'var(--ol-red, #ef4444)', fontSize: 12 }}>
          {error}
        </div>
      )}
      {loading ? (
        <Card><div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div></Card>
      ) : (
        <Card padding={0} style={{ display: 'grid', gridTemplateColumns: '220px minmax(0, 1fr)', flex: 1, minHeight: 0, overflow: 'hidden' }}>
          <div style={{ borderRight: '0.5px solid var(--ol-line)', overflow: 'auto' }} className="ol-thinscroll">
            <div style={{ padding: '12px 14px', borderBottom: '0.5px solid var(--ol-line)', fontSize: 12, fontWeight: 600 }}>
              {t('reports.history', '历史报告')}
            </div>
            {reports.length === 0 && (
              <div style={{ padding: 16, fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('reports.historyEmpty', '暂无历史报告')}</div>
            )}
            {reports.map(report => (
              <button
                key={report.id}
                onClick={() => setSelectedReportId(report.id)}
                style={{
                  width: '100%',
                  padding: '10px 14px',
                  border: 0,
                  borderBottom: '0.5px solid var(--ol-line)',
                  background: selectedReport?.id === report.id ? 'rgba(37,99,235,0.06)' : 'transparent',
                  boxShadow: selectedReport?.id === report.id ? 'inset 2px 0 0 var(--ol-blue)' : 'none',
                  textAlign: 'left',
                  fontFamily: 'inherit',
                  cursor: 'default',
                }}
              >
                <div style={{ fontSize: 12, color: 'var(--ol-ink-2)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{report.title}</div>
                <div style={{ display: 'flex', gap: 6, marginTop: 5, alignItems: 'center', flexWrap: 'wrap' }}>
                  <Pill size="sm" tone={report.status === 'success' ? 'blue' : report.status === 'pending' ? 'outline' : 'outline'}>
                    {reportStatusLabel(report.status)}
                  </Pill>
                  <span style={{ fontSize: 10, color: 'var(--ol-ink-4)' }}>{formatDate(report.createdAt)}</span>
                  {report.status === 'pending' && <span style={pendingDotStyle} />}
                </div>
              </button>
            ))}
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', minHeight: 0, overflow: 'auto', padding: 18 }} className="ol-thinscroll">
            {selectedReport ? (
              <ReportDetail
                report={selectedReport}
                onDelete={() => void onDeleteReport(selectedReport.id)}
                onSave={(title, content) => void onSaveReport(selectedReport.id, title, content)}
              />
            ) : (
              <div style={{ padding: 40, textAlign: 'center', fontSize: 13, color: 'var(--ol-ink-4)' }}>{t('reports.selectHint', '选择一条报告查看详情')}</div>
            )}
          </div>
        </Card>
      )}
      {showGenerateModal && (
        <GenerateReportModal
          reportType={reportType}
          setReportType={setReportType}
          rangeStart={rangeStart}
          setRangeStart={setRangeStart}
          rangeEnd={rangeEnd}
          setRangeEnd={setRangeEnd}
          typedTemplates={typedTemplates}
          templateId={templateId}
          onTemplateChange={id => void onTemplateChange(id)}
          templateNameDraft={templateNameDraft}
          setTemplateNameDraft={setTemplateNameDraft}
          templateDraft={templateDraft}
          setTemplateDraft={setTemplateDraft}
          selectedTemplate={selectedTemplate}
          onSaveTemplate={() => void onSaveTemplate()}
          onDeleteTemplate={() => void onDeleteTemplate()}
          mainWork={mainWork}
          setMainWork={setMainWork}
          onGenerate={() => void onGenerate()}
          onClose={() => setShowGenerateModal(false)}
        />
      )}
    </div>
  );
}

// ─── 生成报告弹窗 ─────────────────────────────────────────────────

function GenerateReportModal({
  reportType,
  setReportType,
  rangeStart,
  setRangeStart,
  rangeEnd,
  setRangeEnd,
  typedTemplates,
  templateId,
  onTemplateChange,
  templateNameDraft,
  setTemplateNameDraft,
  templateDraft,
  setTemplateDraft,
  selectedTemplate,
  onSaveTemplate,
  onDeleteTemplate,
  mainWork,
  setMainWork,
  onGenerate,
  onClose,
}: {
  reportType: ReportType;
  setReportType: (type: ReportType) => void;
  rangeStart: string;
  setRangeStart: (value: string) => void;
  rangeEnd: string;
  setRangeEnd: (value: string) => void;
  typedTemplates: ReportTemplate[];
  templateId: string;
  onTemplateChange: (id: string) => void;
  templateNameDraft: string;
  setTemplateNameDraft: (value: string) => void;
  templateDraft: string;
  setTemplateDraft: (value: string) => void;
  selectedTemplate: ReportTemplate | null;
  onSaveTemplate: () => void;
  onDeleteTemplate: () => void;
  mainWork: string;
  setMainWork: (value: string) => void;
  onGenerate: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();

  return (
    <div style={modalOverlayStyle} onClick={onClose}>
      <div style={modalCardStyle} onClick={e => e.stopPropagation()}>
        <div style={modalHeaderStyle}>
          <h3 style={{ margin: 0, fontSize: 15, fontWeight: 600, color: 'var(--ol-ink)' }}>
            {t('reports.generateModal', '生成报告')}
          </h3>
          <Btn variant="ghost" size="sm" onClick={onClose} style={{ minWidth: 28, padding: '4px 6px' }}>
            ✕
          </Btn>
        </div>

        {/* Body — 可滚动区域 */}
        <div style={{ flex: 1, minHeight: 0, overflow: 'auto', paddingRight: 4 }} className="ol-thinscroll">
          <div style={{ display: 'flex', gap: 8, marginBottom: 16 }}>
            {REPORT_TYPES.map(type => (
              <button
                key={type}
                onClick={() => setReportType(type)}
                style={{
                  padding: '5px 16px',
                  fontSize: 12,
                  borderRadius: 6,
                  border: 0,
                  fontFamily: 'inherit',
                  cursor: 'pointer',
                  fontWeight: 500,
                  background: reportType === type ? 'var(--ol-blue)' : 'var(--ol-surface-2)',
                  color: reportType === type ? '#fff' : 'var(--ol-ink-3)',
                  transition: 'background 0.16s var(--ol-motion-quick), color 0.16s var(--ol-motion-quick)',
                }}
              >
                {reportTypeLabel(type)}
              </button>
            ))}
          </div>

          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
            <Field label={t('reports.rangeStart', '开始时间')}>
              <input type="datetime-local" value={rangeStart} onChange={event => setRangeStart(event.target.value)} style={{ ...inputStyle, width: '100%' }} />
            </Field>
            <Field label={t('reports.rangeEnd', '结束时间')}>
              <input type="datetime-local" value={rangeEnd} onChange={event => setRangeEnd(event.target.value)} style={{ ...inputStyle, width: '100%' }} />
            </Field>
          </div>
          <Field label={t('reports.template', '报告模板')}>
            <select value={templateId} onChange={event => onTemplateChange(event.target.value)} style={{ ...inputStyle, width: '100%' }}>
              {typedTemplates.map(template => (
                <option key={template.id} value={template.id}>{template.name}{template.isBuiltin ? ' · 内置' : ''}</option>
              ))}
            </select>
          </Field>
          <Field label={t('reports.templateName', '模板名称')}>
            <input value={templateNameDraft} onChange={event => setTemplateNameDraft(event.target.value)} style={{ ...inputStyle, width: '100%' }} />
          </Field>
          <Field label={t('reports.templateContent', '模板内容')}>
            <textarea value={templateDraft} onChange={event => setTemplateDraft(event.target.value)} style={{ ...inputStyle, width: '100%', minHeight: 120, resize: 'vertical', lineHeight: 1.55 }} />
          </Field>
          <div style={{ display: 'flex', gap: 8, marginBottom: 16 }}>
            <Btn size="sm" variant="ghost" icon="doc" onClick={onSaveTemplate}>
              {selectedTemplate?.isBuiltin ? t('reports.saveAsTemplate', '另存模板') : t('common.save')}
            </Btn>
            <Btn size="sm" variant="ghost" icon="trash" disabled={!selectedTemplate || selectedTemplate.isBuiltin} onClick={onDeleteTemplate}>{t('common.delete')}</Btn>
          </div>
          <Field label={t('reports.mainWork', '主要工作')}>
            <textarea
              value={mainWork}
              onChange={event => setMainWork(event.target.value)}
              placeholder={t('reports.mainWorkPlaceholder', '可选。用户主动输入的主要工作会优先于历史摘要。')}
              style={{ ...inputStyle, width: '100%', minHeight: 100, resize: 'vertical', lineHeight: 1.55 }}
            />
          </Field>
        </div>

        {/* Footer — 固定操作栏 */}
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, paddingTop: 16, marginTop: 8, borderTop: '0.5px solid var(--ol-line-soft)', flexShrink: 0 }}>
          <Btn variant="ghost" size="sm" onClick={onClose}>{t('common.cancel', '取消')}</Btn>
          <Btn variant="blue" icon="sparkle" disabled={!templateId} onClick={onGenerate}>
            {t('reports.generate', '生成报告')}
          </Btn>
        </div>
      </div>
    </div>
  );
}

// ─── 报告详情（查看 / 编辑） ──────────────────────────────────────

function ReportDetail({
  report,
  onDelete,
  onSave,
}: {
  report: GeneratedReport;
  onDelete: () => void;
  onSave: (title: string, content: string) => void;
}) {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [editTitle, setEditTitle] = useState('');
  const [editContent, setEditContent] = useState('');
  const [copied, setCopied] = useState(false);
  const copyTimerRef = useRef<number | null>(null);

  // 切换报告时退出编辑模式并初始化临时状态
  useEffect(() => {
    setEditing(false);
    setEditTitle(report.title);
    setEditContent(report.content ?? '');
  }, [report.id]);

  useEffect(() => () => {
    if (copyTimerRef.current) clearTimeout(copyTimerRef.current);
  }, []);

  const startEditing = () => setEditing(true);

  const cancelEditing = () => setEditing(false);

  const handleSave = () => {
    if (!editTitle.trim() || !editContent.trim()) return;
    onSave(editTitle.trim(), editContent.trim());
    setEditing(false);
  };

  const handleCopy = async () => {
    if (!report.content) return;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(report.content);
      } else {
        // fallback
        const textarea = document.createElement('textarea');
        textarea.value = report.content;
        textarea.style.position = 'fixed';
        textarea.style.opacity = '0';
        document.body.appendChild(textarea);
        textarea.select();
        document.execCommand('copy');
        document.body.removeChild(textarea);
      }
      setCopied(true);
      emitSaved('saved', t('common.copied'));
      if (copyTimerRef.current) clearTimeout(copyTimerRef.current);
      copyTimerRef.current = window.setTimeout(() => setCopied(false), 2000);
    } catch {
      emitSaved('failed', t('common.operationFailed'));
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, alignItems: 'flex-start', marginBottom: 14, flexShrink: 0 }}>
        {editing ? (
          <input
            value={editTitle}
            onChange={e => setEditTitle(e.target.value)}
            style={{ ...inputStyle, fontSize: 16, fontWeight: 600, flex: 1, minWidth: 0 }}
          />
        ) : (
          <div>
            <div style={{ fontSize: 18, fontWeight: 600, color: 'var(--ol-ink)', marginBottom: 8 }}>{report.title}</div>
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
              <Pill size="sm" tone="blue">{reportTypeLabel(report.reportType)}</Pill>
              <Pill size="sm" tone={report.status === 'success' ? 'blue' : 'outline'}>{reportStatusLabel(report.status)}</Pill>
              <Pill size="sm" tone="outline">{report.templateName}</Pill>
            </div>
          </div>
        )}
        {report.status !== 'pending' && (
        <div style={{ display: 'flex', gap: 6, flexShrink: 0 }}>
          {editing ? (
            <>
              <Btn variant="blue" size="sm" onClick={handleSave} disabled={!editTitle.trim() || !editContent.trim()}>{t('reports.save', '保存')}</Btn>
              <Btn variant="ghost" size="sm" onClick={cancelEditing}>{t('reports.cancel', '取消')}</Btn>
            </>
          ) : (
            <>
              <Btn icon="edit" variant="ghost" size="sm" onClick={startEditing}>{t('reports.edit', '编辑')}</Btn>
              <Btn
                icon="copy"
                variant="ghost"
                size="sm"
                disabled={!report.content}
                onClick={() => void handleCopy()}
              >
                {copied ? t('reports.copied', '已复制') : t('reports.copy', '复制')}
              </Btn>
              <Btn icon="trash" variant="ghost" size="sm" onClick={onDelete}>{t('common.delete')}</Btn>
            </>
          )}
        </div>
        )}
      </div>
      {report.status !== 'pending' && (
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(130px, 1fr))', gap: 10, marginBottom: 14, flexShrink: 0 }}>
        <Meta label="语音历史" value={String(report.sourceStats.voiceCount)} />
        <Meta label="重写历史" value={String(report.sourceStats.rewriteCount)} />
        <Meta label="截图记录" value={String(report.sourceStats.screenshotRecordCount)} />
        <Meta label="已分析截图" value={String(report.sourceStats.analyzedScreenshotRecordCount)} />
      </div>
      )}
      {report.status !== 'pending' && report.userMainWork && <div style={{ flexShrink: 0 }}><Block label="主要工作" value={report.userMainWork} /></div>}
      {report.status === 'pending' ? (
        <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', flex: 1, gap: 12, color: 'var(--ol-ink-4)' }}>
          <div style={pendingSpinnerStyle} />
          <div style={{ fontSize: 13 }}>{t('reports.generating', '生成中')}</div>
          <div style={{ fontSize: 11 }}>{t('reports.generatingHint', '报告正在生成中，完成后将自动刷新')}</div>
        </div>
      ) : editing ? (
        <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0, marginBottom: 14 }}>
          <div style={{ fontSize: 11, color: 'var(--ol-ink-4)', marginBottom: 6, flexShrink: 0 }}>{t('reports.contentLabel', '报告正文')}</div>
          <textarea
            value={editContent}
            onChange={e => setEditContent(e.target.value)}
            style={{
              ...inputStyle,
              width: '100%',
              maxWidth: 'none',
              flex: 1,
              minHeight: 200,
              resize: 'none',
              lineHeight: 1.7,
              fontSize: 13,
              fontFamily: 'inherit',
              padding: '10px 12px',
            }}
          />
        </div>
      ) : report.content ? (
        <Block label={t('reports.contentLabel', '报告正文')} value={report.content} />
      ) : (
        <Block label="生成状态" value={report.errorMessage || report.errorCode || '无可用内容'} />
      )}
    </div>
  );
}

// ─── 辅助组件 ──────────────────────────────────────────────────────

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label style={{ display: 'flex', flexDirection: 'column', gap: 6, marginBottom: 12 }}>
      <span style={{ fontSize: 11, color: 'var(--ol-ink-4)' }}>{label}</span>
      {children}
    </label>
  );
}

function Meta({ label, value }: { label: string; value: string }) {
  return (
    <div style={{ padding: 10, border: '0.5px solid var(--ol-line)', borderRadius: 8, background: 'var(--ol-surface-2)' }}>
      <div style={{ fontSize: 10, color: 'var(--ol-ink-4)', marginBottom: 4 }}>{label}</div>
      <div style={{ fontSize: 13, color: 'var(--ol-ink-2)' }}>{value}</div>
    </div>
  );
}

function Block({ label, value }: { label: string; value: string }) {
  return (
    <div style={{ marginBottom: 14 }}>
      <div style={{ fontSize: 11, color: 'var(--ol-ink-4)', marginBottom: 6 }}>{label}</div>
      <div style={{ whiteSpace: 'pre-wrap', lineHeight: 1.7, fontSize: 13, color: 'var(--ol-ink-2)' }}>{value}</div>
    </div>
  );
}

// ─── 工具函数 ──────────────────────────────────────────────────────

function reportTypeLabel(type: ReportType): string {
  if (type === 'daily') return '日报';
  if (type === 'weekly') return '周报';
  return '月报';
}

function reportStatusLabel(status: GeneratedReport['status']): string {
  if (status === 'success') return '成功';
  if (status === 'failed') return '失败';
  if (status === 'skipped') return '已跳过';
  return '生成中';
}

function preferredTemplateId(reportType: ReportType, prefs: ReturnType<typeof useHotkeySettings>['prefs']): string | null {
  if (!prefs) return null;
  if (reportType === 'daily') return prefs.selectedDailyReportTemplateId;
  if (reportType === 'weekly') return prefs.selectedWeeklyReportTemplateId;
  return prefs.selectedMonthlyReportTemplateId;
}

function startOfToday(): Date {
  const now = new Date();
  return new Date(now.getFullYear(), now.getMonth(), now.getDate(), 0, 0, 0, 0);
}

function localDateTimeValue(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function formatDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

function errorMessage(error: unknown): string {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

// ─── 弹窗样式常量 ─────────────────────────────────────────────────

const pendingDotStyle: CSSProperties = {
  width: 6,
  height: 6,
  borderRadius: '50%',
  background: 'var(--ol-blue)',
  animation: 'ol-report-pending-pulse 1.4s ease-in-out infinite',
  flexShrink: 0,
};

const pendingSpinnerStyle: CSSProperties = {
  width: 28,
  height: 28,
  border: '2.5px solid var(--ol-line)',
  borderTopColor: 'var(--ol-blue)',
  borderRadius: '50%',
  animation: 'ol-spin 0.8s linear infinite',
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
  maxWidth: 720,
  maxHeight: '85vh',
  background: 'var(--ol-surface)',
  borderRadius: 'var(--ol-r-lg)',
  border: '0.5px solid var(--ol-line)',
  boxShadow: 'var(--ol-shadow-xl)',
  display: 'flex',
  flexDirection: 'column',
  padding: 24,
  animation: 'ol-modal-card-in 0.24s var(--ol-motion-spring)',
};

const modalHeaderStyle: CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  marginBottom: 14,
};
