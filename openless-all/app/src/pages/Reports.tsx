import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  deleteGeneratedReport,
  deleteReportTemplate,
  generateReport,
  listGeneratedReports,
  listReportTemplates,
  saveReportTemplate,
} from '../lib/ipc';
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
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [templateDraft, setTemplateDraft] = useState('');
  const [templateNameDraft, setTemplateNameDraft] = useState('');

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
    setBusy(true);
    setError(null);
    try {
      const report = await generateReport({
        reportType,
        rangeStart: new Date(rangeStart).toISOString(),
        rangeEnd: new Date(rangeEnd).toISOString(),
        templateId,
        userMainWork: mainWork.trim() || null,
      });
      const list = await listGeneratedReports();
      setReports(list);
      setSelectedReportId(report.id);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  const onDeleteReport = async (id: string) => {
    await deleteGeneratedReport(id);
    const list = await listGeneratedReports();
    setReports(list);
    setSelectedReportId(prev => (prev === id ? list[0]?.id ?? null : prev));
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}>
      <PageHeader
        kicker={t('reports.kicker', 'REPORTS')}
        title={t('reports.title', '报告')}
        desc={t('reports.desc', '基于语音历史、重写历史、截图记录和完整摘要生成日报、周报、月报。')}
        right={<Btn icon="refresh" variant="ghost" size="sm" onClick={() => void refresh()}>{t('common.refresh')}</Btn>}
      />
      {error && (
        <div style={{ marginBottom: 12, padding: '9px 10px', borderRadius: 8, background: 'rgba(239,68,68,0.08)', color: 'var(--ol-red, #ef4444)', fontSize: 12 }}>
          {error}
        </div>
      )}
      {loading ? (
        <Card><div style={{ fontSize: 12, color: 'var(--ol-ink-4)' }}>{t('common.loading')}</div></Card>
      ) : (
        <div style={{ display: 'grid', gridTemplateColumns: 'minmax(320px, 430px) minmax(0, 1fr)', gap: 14, flex: 1, minHeight: 0 }}>
          <Card className="ol-thinscroll" style={{ overflow: 'auto' }}>
            <div style={{ display: 'flex', gap: 8, marginBottom: 14 }}>
              {REPORT_TYPES.map(type => (
                <button
                  key={type}
                  onClick={() => setReportType(type)}
                  style={{
                    padding: '5px 14px',
                    fontSize: 12,
                    borderRadius: 6,
                    border: 0,
                    fontFamily: 'inherit',
                    cursor: 'pointer',
                    fontWeight: 500,
                    background: reportType === type ? 'var(--ol-blue)' : 'var(--ol-surface-2)',
                    color: reportType === type ? '#fff' : 'var(--ol-ink-3)',
                  }}
                >
                  {reportTypeLabel(type)}
                </button>
              ))}
            </div>
            <Field label={t('reports.rangeStart', '开始时间')}>
              <input type="datetime-local" value={rangeStart} onChange={event => setRangeStart(event.target.value)} style={{ ...inputStyle, width: '100%' }} />
            </Field>
            <Field label={t('reports.rangeEnd', '结束时间')}>
              <input type="datetime-local" value={rangeEnd} onChange={event => setRangeEnd(event.target.value)} style={{ ...inputStyle, width: '100%' }} />
            </Field>
            <Field label={t('reports.template', '报告模板')}>
              <select value={templateId} onChange={event => void onTemplateChange(event.target.value)} style={{ ...inputStyle, width: '100%' }}>
                {typedTemplates.map(template => (
                  <option key={template.id} value={template.id}>{template.name}{template.isBuiltin ? ' · 内置' : ''}</option>
                ))}
              </select>
            </Field>
            <Field label={t('reports.templateName', '模板名称')}>
              <input value={templateNameDraft} onChange={event => setTemplateNameDraft(event.target.value)} style={{ ...inputStyle, width: '100%' }} />
            </Field>
            <Field label={t('reports.templateContent', '模板内容')}>
              <textarea value={templateDraft} onChange={event => setTemplateDraft(event.target.value)} style={{ ...inputStyle, width: '100%', minHeight: 150, resize: 'vertical', lineHeight: 1.55 }} />
            </Field>
            <div style={{ display: 'flex', gap: 8, marginBottom: 16 }}>
              <Btn size="sm" variant="ghost" icon="doc" onClick={() => void onSaveTemplate()}>
                {selectedTemplate?.isBuiltin ? t('reports.saveAsTemplate', '另存模板') : t('common.save')}
              </Btn>
              <Btn size="sm" variant="ghost" icon="trash" disabled={!selectedTemplate || selectedTemplate.isBuiltin} onClick={() => void onDeleteTemplate()}>{t('common.delete')}</Btn>
            </div>
            <Field label={t('reports.mainWork', '主要工作')}>
              <textarea
                value={mainWork}
                onChange={event => setMainWork(event.target.value)}
                placeholder={t('reports.mainWorkPlaceholder', '可选。用户主动输入的主要工作会优先于历史摘要。')}
                style={{ ...inputStyle, width: '100%', minHeight: 120, resize: 'vertical', lineHeight: 1.55 }}
              />
            </Field>
            <Btn variant="primary" icon="sparkle" disabled={busy || !templateId} onClick={() => void onGenerate()}>
              {busy ? t('reports.generating', '生成中') : t('reports.generate', '生成报告')}
            </Btn>
          </Card>
          <Card padding={0} style={{ display: 'grid', gridTemplateColumns: '280px minmax(0, 1fr)', minHeight: 0, overflow: 'hidden' }}>
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
                    <Pill size="sm" tone={report.status === 'success' ? 'blue' : 'outline'}>{reportStatusLabel(report.status)}</Pill>
                    <span style={{ fontSize: 10, color: 'var(--ol-ink-4)' }}>{formatDate(report.createdAt)}</span>
                  </div>
                </button>
              ))}
            </div>
            <div className="ol-thinscroll" style={{ overflow: 'auto', padding: 18 }}>
              {selectedReport ? (
                <ReportDetail report={selectedReport} onDelete={() => void onDeleteReport(selectedReport.id)} />
              ) : (
                <div style={{ padding: 40, textAlign: 'center', fontSize: 13, color: 'var(--ol-ink-4)' }}>{t('reports.selectHint', '选择一条报告查看详情')}</div>
              )}
            </div>
          </Card>
        </div>
      )}
    </div>
  );
}

function ReportDetail({ report, onDelete }: { report: GeneratedReport; onDelete: () => void }) {
  return (
    <div>
      <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12, alignItems: 'flex-start', marginBottom: 14 }}>
        <div>
          <div style={{ fontSize: 18, fontWeight: 600, color: 'var(--ol-ink)', marginBottom: 8 }}>{report.title}</div>
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <Pill size="sm" tone="blue">{reportTypeLabel(report.reportType)}</Pill>
            <Pill size="sm" tone={report.status === 'success' ? 'blue' : 'outline'}>{reportStatusLabel(report.status)}</Pill>
            <Pill size="sm" tone="outline">{report.templateName}</Pill>
          </div>
        </div>
        <Btn icon="trash" variant="ghost" size="sm" onClick={onDelete}>删除</Btn>
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(130px, 1fr))', gap: 10, marginBottom: 14 }}>
        <Meta label="语音历史" value={String(report.sourceStats.voiceCount)} />
        <Meta label="重写历史" value={String(report.sourceStats.rewriteCount)} />
        <Meta label="截图记录" value={String(report.sourceStats.screenshotRecordCount)} />
        <Meta label="已分析截图" value={String(report.sourceStats.analyzedScreenshotRecordCount)} />
      </div>
      {report.userMainWork && <Block label="主要工作" value={report.userMainWork} />}
      {report.content ? (
        <Block label="报告正文" value={report.content} />
      ) : (
        <Block label="生成状态" value={report.errorMessage || report.errorCode || '无可用内容'} />
      )}
    </div>
  );
}

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
