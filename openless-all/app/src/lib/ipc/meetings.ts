import type { MeetingRecord, MeetingRecordingSnapshot } from "../types"
import { invokeOrMock } from "./shared"
import { mockMeetings } from "./mock-data"

export function listMeetings(): Promise<MeetingRecord[]> {
    return invokeOrMock("list_meetings", undefined, () => mockMeetings)
}

export function getMeeting(id: string): Promise<MeetingRecord> {
    return invokeOrMock(
        "get_meeting",
        { id },
        () => mockMeetings.find((item) => item.id === id) ?? mockMeetings[0],
    )
}

export function createMeetingRecord(record: MeetingRecord): Promise<MeetingRecord> {
    return invokeOrMock("create_meeting_record", { record }, () => record)
}

export function updateMeetingRecord(record: MeetingRecord): Promise<MeetingRecord> {
    return invokeOrMock("update_meeting_record", { record }, () => record)
}

export function deleteMeetingRecord(id: string): Promise<void> {
    return invokeOrMock("delete_meeting_record", { id }, () => undefined)
}

function mockMeetingRecordingSnapshot(): MeetingRecordingSnapshot {
    return {
        meeting: {
            ...mockMeetings[0],
            status: "recording",
            endedAt: null,
            durationMs: null,
            audio: {
                state: "temporary",
                retained: false,
                path: null,
            },
        },
        phase: "recording",
        elapsedMs: 0,
        activeAsrProvider: "mock",
        activeProviderSessionId: "mock-session-1",
        asrInterrupted: false,
    }
}

export function startMeetingRecording(): Promise<MeetingRecordingSnapshot> {
    return invokeOrMock("start_meeting_recording", undefined, mockMeetingRecordingSnapshot)
}

export function pauseMeetingRecording(id: string): Promise<MeetingRecordingSnapshot> {
    return invokeOrMock("pause_meeting_recording", { id }, () => ({
        ...mockMeetingRecordingSnapshot(),
        phase: "paused",
        meeting: {
            ...mockMeetingRecordingSnapshot().meeting,
            id,
            status: "paused",
        },
    }))
}

export function resumeMeetingRecording(id: string): Promise<MeetingRecordingSnapshot> {
    return invokeOrMock("resume_meeting_recording", { id }, () => ({
        ...mockMeetingRecordingSnapshot(),
        meeting: {
            ...mockMeetingRecordingSnapshot().meeting,
            id,
        },
    }))
}

export function stopMeetingRecording(id: string): Promise<MeetingRecord> {
    return invokeOrMock("stop_meeting_recording", { id }, () => ({
        ...mockMeetings[0],
        id,
        status: "completed",
        endedAt: new Date().toISOString(),
    }))
}

export function getActiveMeetingRecording(): Promise<MeetingRecordingSnapshot | null> {
    return invokeOrMock("get_active_meeting_recording", undefined, () => null)
}

export function generateMeetingSummary(id: string): Promise<MeetingRecord> {
    return invokeOrMock("generate_meeting_summary", { id }, () => ({
        ...mockMeetings[0],
        id,
        status: "completed",
    }))
}

export function retryMeetingSummary(id: string): Promise<MeetingRecord> {
    return invokeOrMock("retry_meeting_summary", { id }, () => ({
        ...mockMeetings[0],
        id,
        status: "completed",
    }))
}

export function exportMeetingMarkdown(id: string, targetPath: string): Promise<void> {
    return invokeOrMock("export_meeting_markdown", { id, targetPath }, () => undefined)
}

export function retranscribeMeeting(id: string): Promise<MeetingRecord> {
    return invokeOrMock("retranscribe_meeting", { id }, () => ({
        ...mockMeetings[0],
        id,
        transcriptSegments: mockMeetings[0].transcriptSegments.map((segment) => ({
            ...segment,
            source: "retranscribed_asr",
        })),
    }))
}

export function hideMainWindowAfterMeetingGuard(): Promise<void> {
    return invokeOrMock("hide_main_window_after_meeting_guard", undefined, () => undefined)
}

export function exitAppAfterMeetingGuard(): Promise<void> {
    return invokeOrMock("exit_app_after_meeting_guard", undefined, () => undefined)
}
