import type {
    MeetingAsrModelDescriptor,
    MeetingAudioSelection,
    MeetingListItem,
    MeetingRecord,
    MeetingRecordingSnapshot,
    PostMeetingAsrModelDescriptor,
    RetryMeetingAudioImportOptions,
    RetryMeetingPostProcessingOptions,
    StartMeetingAudioImportOptions,
    StartMeetingRecordingOptions,
} from "../types"
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

export function startMeetingRecording(
    options?: StartMeetingRecordingOptions,
): Promise<MeetingRecordingSnapshot> {
    return invokeOrMock(
        "start_meeting_recording",
        { options: options ?? null },
        mockMeetingRecordingSnapshot,
    )
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

export function listPostMeetingAsrModels(): Promise<PostMeetingAsrModelDescriptor[]> {
    return invokeOrMock("list_post_meeting_asr_models", undefined, () => [
        {
            providerId: "bailian",
            modelId: "fun-asr",
            displayName: "Fun-ASR",
            runtimeKind: "cloud",
            supportsFileTranscription: true,
            supportsDiarization: true,
            supportsSpeakerCount: true,
            isDefault: true,
        },
        {
            providerId: "bailian",
            modelId: "paraformer-v2",
            displayName: "Paraformer V2",
            runtimeKind: "cloud",
            supportsFileTranscription: true,
            supportsDiarization: true,
            supportsSpeakerCount: true,
            isDefault: false,
        },
    ])
}

export function chooseMeetingAudioFile(): Promise<MeetingAudioSelection | null> {
    return invokeOrMock("choose_meeting_audio_file", undefined, () => ({
        selectionToken: "mock-meeting-audio-selection",
        fileName: "meeting.wav",
        format: "wav",
        sizeBytes: 1_920_044,
        durationMs: 60_000,
        channels: 1,
        sampleRate: 16_000,
        bitsPerSample: 16,
    }))
}

export function listMeetingFileAsrModels(): Promise<MeetingAsrModelDescriptor[]> {
    return invokeOrMock("list_meeting_file_asr_models", undefined, () => [
        {
            providerId: "bailian",
            modelId: "fun-asr",
            displayName: "Fun-ASR",
            runtimeKind: "cloud",
            supportsMeetingFile: true,
            supportsDiarization: true,
            supportsSpeakerCount: true,
            readiness: "ready",
            readinessMessage: null,
            isDefault: true,
        },
        {
            providerId: "bailian",
            modelId: "paraformer-v2",
            displayName: "Paraformer V2",
            runtimeKind: "cloud",
            supportsMeetingFile: true,
            supportsDiarization: true,
            supportsSpeakerCount: true,
            readiness: "ready",
            readinessMessage: null,
            isDefault: false,
        },
        {
            providerId: "sherpa-onnx-local",
            modelId: "sense-voice-small-zh",
            displayName: "SenseVoice Small (local)",
            runtimeKind: "local",
            supportsMeetingFile: true,
            supportsDiarization: false,
            supportsSpeakerCount: false,
            readiness: "missing",
            readinessMessage: "Local model is not downloaded",
            isDefault: false,
        },
    ])
}

export function startMeetingAudioImport(
    options: StartMeetingAudioImportOptions,
): Promise<MeetingRecord> {
    return invokeOrMock(
        "start_meeting_audio_import",
        { options },
        () => ({
            ...mockMeetings[0],
            id: `mock-import-${Date.now()}`,
            title: options.title || "Imported meeting",
            status: "draft",
            transcriptSegments: [],
            audio: { state: "temporary", retained: false, path: null },
            importState: {
                status: "importing",
                importJobId: "mock-import-job",
                progress: 0,
                attempt: 1,
                errorCode: null,
                errorMessage: null,
                createdAt: new Date().toISOString(),
                updatedAt: new Date().toISOString(),
                completedAt: null,
            },
            importConfig: {
                sourceFileName: "meeting.wav",
                sourceFormat: "wav",
                asrModelRef: options.asrModelRef,
                resolvedAsrRuntimeKind: options.asrModelRef.providerId === "bailian" ? "cloud" : "local",
                diarizationMode: options.diarizationMode,
                localDiarizationModelId: options.localDiarizationModelId,
                expectedSpeakerCount: options.expectedSpeakerCount,
                generateSummary: options.generateSummary,
                processingRevision: 1,
            },
        }),
    )
}

export function cancelMeetingAudioImport(id: string): Promise<MeetingRecord> {
    return invokeOrMock(
        "cancel_meeting_audio_import",
        { id },
        () => ({ ...mockMeetings[0], id }),
    )
}

export function retryMeetingAudioImport(
    id: string,
    options?: RetryMeetingAudioImportOptions,
): Promise<MeetingRecord> {
    return invokeOrMock(
        "retry_meeting_audio_import",
        { id, options: options ?? null },
        () => ({ ...mockMeetings[0], id }),
    )
}

export function retryMeetingPostProcessing(
    id: string,
    options?: RetryMeetingPostProcessingOptions,
): Promise<MeetingRecord> {
    return invokeOrMock(
        "retry_meeting_post_processing",
        { id, options: options ?? null },
        () => ({ ...mockMeetings[0], id }),
    )
}

export function cancelMeetingPostProcessing(id: string): Promise<MeetingRecord> {
    return invokeOrMock(
        "cancel_meeting_post_processing",
        { id },
        () => ({ ...mockMeetings[0], id }),
    )
}

export function useRealtimeTranscriptAndSummarize(id: string): Promise<MeetingRecord> {
    return invokeOrMock(
        "use_realtime_transcript_and_summarize",
        { id },
        () => ({ ...mockMeetings[0], id }),
    )
}

export function renameMeetingSpeaker(
    meetingId: string,
    speakerId: string,
    displayName: string,
): Promise<MeetingRecord> {
    return invokeOrMock(
        "rename_meeting_speaker",
        { meetingId, speakerId, displayName },
        () => {
            const index = mockMeetings.findIndex(meeting => meeting.id === meetingId)
            const meeting = index >= 0 ? mockMeetings[index] : mockMeetings[0]
            const updated = {
                ...meeting,
                id: meetingId,
                speakerProfiles: meeting.speakerProfiles?.map(profile => (
                    profile.id === speakerId
                        ? { ...profile, displayName, manuallyNamed: true }
                        : profile
                )),
                updatedAt: new Date().toISOString(),
            }
            if (index >= 0) mockMeetings[index] = updated
            return updated
        },
    )
}

export function showMeetingCompanion(): Promise<void> {
    return invokeOrMock("show_meeting_companion", undefined, () => undefined)
}

export function hideMeetingCompanion(): Promise<void> {
    return invokeOrMock("hide_meeting_companion", undefined, () => undefined)
}

export function startMeetingCompanionDrag(): Promise<boolean> {
    return invokeOrMock("start_meeting_companion_drag", undefined, () => true)
}

export function saveMeetingCompanionPosition(): Promise<void> {
    return invokeOrMock("save_meeting_companion_position", undefined, () => undefined)
}

export function dismissCompletedMeetingCompanion(meetingId: string): Promise<boolean> {
    return invokeOrMock("dismiss_completed_meeting_companion", { meetingId }, () => true)
}

export function setMeetingCompanionPositionLocked(locked: boolean): Promise<boolean> {
    return invokeOrMock("set_meeting_companion_position_locked", { locked }, () => locked)
}

export function openMeetingFromCompanion(meetingId: string): Promise<void> {
    return invokeOrMock("open_meeting_from_companion", { meetingId }, () => undefined)
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
