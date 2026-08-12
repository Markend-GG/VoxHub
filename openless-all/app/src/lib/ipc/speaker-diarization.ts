import { invokeOrMock } from './shared'

export type SpeakerDiarizationModelReadiness =
  | 'missing'
  | 'downloading'
  | 'ready'
  | 'invalid'

export interface SpeakerDiarizationModelDescriptor {
  id: string
  displayName: string
  version: string
  source: string
  supportedPlatforms: string[]
  readiness: SpeakerDiarizationModelReadiness
  downloadedBytes: number
  totalBytes: number
  sampleRate: number
  clusteringThreshold: number
  maxRecommendedDurationMs: number | null
  memoryTier: string | null
  experimental: boolean
  error: string | null
}

export type SpeakerDiarizationDownloadPhase =
  | 'started'
  | 'progress'
  | 'finished'
  | 'cancelled'
  | 'failed'

export interface SpeakerDiarizationDownloadProgress {
  modelId: string
  file: string
  fileIndex: number
  fileCount: number
  bytesDownloaded: number
  bytesTotal: number
  phase: SpeakerDiarizationDownloadPhase
  error: string | null
}

const MOCK_MODEL_ID = 'sherpa-pyannote-3dspeaker-zh-v1'
const MOCK_TOTAL_BYTES = 46_552_205
let mockReadiness: SpeakerDiarizationModelReadiness = 'missing'

export function listSpeakerDiarizationModels(): Promise<SpeakerDiarizationModelDescriptor[]> {
  return invokeOrMock('list_speaker_diarization_models', undefined, () => [{
    id: MOCK_MODEL_ID,
    displayName: 'Sherpa-ONNX Pyannote 3.0 + 3D-Speaker (中文会议)',
    version: '1',
    source: 'sherpa-onnx official releases: Pyannote 3.0 + 3D-Speaker',
    supportedPlatforms: ['windows-x86_64'],
    readiness: mockReadiness,
    downloadedBytes: mockReadiness === 'ready' ? MOCK_TOTAL_BYTES : 0,
    totalBytes: MOCK_TOTAL_BYTES,
    sampleRate: 16_000,
    clusteringThreshold: 0.9,
    maxRecommendedDurationMs: null,
    memoryTier: null,
    experimental: true,
    error: null,
  }])
}

export function downloadSpeakerDiarizationModel(modelId: string): Promise<void> {
  return invokeOrMock('download_speaker_diarization_model', { modelId }, () => {
    mockReadiness = 'ready'
  })
}

export function cancelSpeakerDiarizationModelDownload(modelId: string): Promise<void> {
  return invokeOrMock(
    'cancel_speaker_diarization_model_download',
    { modelId },
    () => {
      mockReadiness = 'missing'
    },
  )
}

export function deleteSpeakerDiarizationModel(modelId: string): Promise<void> {
  return invokeOrMock('delete_speaker_diarization_model', { modelId }, () => {
    mockReadiness = 'missing'
  })
}
