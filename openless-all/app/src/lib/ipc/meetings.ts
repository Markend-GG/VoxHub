import type { MeetingListItem, MeetingRecord, MeetingRecordingSnapshot } from "../types"
import { invokeOrMock } from "./shared"
import { mockMeetings } from "./mock-data"

export type BinaryPayload = Uint8Array | ArrayBuffer | number[]

export function listMeetings(): Promise<MeetingListItem[]> {
    return invokeOrMock("list_meetings", undefined, () => mockMeetings.map(meetingListItemFromRecord))
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

export function prepareMeetingAudioPlayback(id: string): Promise<string | BinaryPayload> {
    return invokeOrMock("prepare_meeting_audio_playback", { id }, mockMeetingAudioWav)
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

function mockMeetingAudioWav(): Uint8Array {
    const sampleCount = 1600
    const dataSize = sampleCount * 2
    const bytes = new Uint8Array(44 + dataSize)
    const view = new DataView(bytes.buffer)
    writeAscii(bytes, 0, "RIFF")
    view.setUint32(4, 36 + dataSize, true)
    writeAscii(bytes, 8, "WAVE")
    writeAscii(bytes, 12, "fmt ")
    view.setUint32(16, 16, true)
    view.setUint16(20, 1, true)
    view.setUint16(22, 1, true)
    view.setUint32(24, 16000, true)
    view.setUint32(28, 32000, true)
    view.setUint16(32, 2, true)
    view.setUint16(34, 16, true)
    writeAscii(bytes, 36, "data")
    view.setUint32(40, dataSize, true)
    return bytes
}

function writeAscii(bytes: Uint8Array, offset: number, value: string): void {
    for (let i = 0; i < value.length; i += 1) {
        bytes[offset + i] = value.charCodeAt(i)
    }
}

function meetingListItemFromRecord(record: MeetingRecord): MeetingListItem {
    return {
        id: record.id,
        title: record.title,
        status: record.status,
        startedAt: record.startedAt,
        endedAt: record.endedAt,
        durationMs: record.durationMs,
        summaryOverview: record.summary.overview,
        transcriptPreview: record.transcriptSegments.find(segment => segment.text.trim())?.text.trim().slice(0, 180) ?? "",
        transcriptSegmentCount: record.transcriptSegments.length,
        audio: record.audio,
        createdAt: record.createdAt,
        updatedAt: record.updatedAt,
    }
}
