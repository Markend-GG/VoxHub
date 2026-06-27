import type {
    GeneratedReport,
    ReportTemplate,
    ReportType,
    ScreenshotRecord,
} from "../types"
import { invokeOrMock } from "./shared"

export interface GenerateReportRequest {
    reportType: ReportType;
    rangeStart: string;
    rangeEnd: string;
    templateId: string;
    userMainWork?: string | null;
}

export function listScreenshotRecords(): Promise<ScreenshotRecord[]> {
    return invokeOrMock("list_screenshot_records", undefined, () => [])
}

export function deleteScreenshotRecord(id: string): Promise<void> {
    return invokeOrMock("delete_screenshot_record", { id }, () => undefined)
}

export function clearScreenshotRecords(): Promise<void> {
    return invokeOrMock("clear_screenshot_records", undefined, () => undefined)
}

export function reanalyzeScreenshotRecord(id: string): Promise<void> {
    return invokeOrMock("reanalyze_screenshot_record", { id }, () => undefined)
}

export function listReportTemplates(): Promise<ReportTemplate[]> {
    return invokeOrMock("list_report_templates", undefined, () => [])
}

export function saveReportTemplate(template: ReportTemplate): Promise<ReportTemplate> {
    return invokeOrMock("save_report_template", { template }, () => template)
}

export function deleteReportTemplate(id: string): Promise<void> {
    return invokeOrMock("delete_report_template", { id }, () => undefined)
}

export function generateReport(request: GenerateReportRequest): Promise<GeneratedReport> {
    return invokeOrMock(
        "generate_report",
        { request },
        () => ({
            id: crypto.randomUUID(),
            reportType: request.reportType,
            title: "Mock report",
            rangeStart: request.rangeStart,
            rangeEnd: request.rangeEnd,
            templateId: request.templateId,
            templateName: "Mock template",
            templateContent: "",
            userMainWork: request.userMainWork ?? null,
            status: "skipped",
            content: null,
            sourceStats: {
                voiceCount: 0,
                rewriteCount: 0,
                screenshotRecordCount: 0,
                analyzedScreenshotRecordCount: 0,
            },
            errorCode: "skipped:noContent",
            errorMessage: null,
            createdAt: new Date().toISOString(),
            updatedAt: new Date().toISOString(),
        }),
    )
}

export function listGeneratedReports(): Promise<GeneratedReport[]> {
    return invokeOrMock("list_generated_reports", undefined, () => [])
}

export function getGeneratedReport(id: string): Promise<GeneratedReport | null> {
    return invokeOrMock("get_generated_report", { id }, () => null)
}

export function deleteGeneratedReport(id: string): Promise<void> {
    return invokeOrMock("delete_generated_report", { id }, () => undefined)
}
