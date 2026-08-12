//! Streaming ASR providers.
//!
//! Mirrors the Swift `OpenLessASR` library. The Volcengine SAUC bigmodel
//! client is the reference implementation; the wire protocol lives in
//! `frame.rs` (binary frame codec) and the session lifecycle in
//! `volcengine.rs`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::types::TranscriptTokenTimestamp;

pub mod bailian;
pub mod dashscope_multimodal;
pub mod elevenlabs;
mod frame;
pub mod local;
pub mod meeting_audio_source;
pub mod mimo;
pub mod pcm;
pub mod qwen_realtime;
pub mod stepfun_realtime;
pub mod volcengine;
pub mod wav;
pub mod whisper;
pub mod xfyun;

pub use bailian::{BailianCredentials, BailianRealtimeASR};
pub use dashscope_multimodal::DashScopeMultimodalASR;
pub use elevenlabs::ElevenLabsBatchASR;
pub use meeting_audio_source::MeetingAudioSource;
pub use mimo::MimoBatchASR;
pub use qwen_realtime::{Qwen3RealtimeASR, Qwen3RealtimeCredentials};
pub use stepfun_realtime::{StepfunRealtimeASR, StepfunRealtimeCredentials};
pub use volcengine::{VolcengineCredentials, VolcengineStreamingASR};
pub use whisper::WhisperBatchASR;
pub use xfyun::{XfyunCredentials, XfyunStreamingASR};

/// Sink for raw 16 kHz / 16-bit / mono PCM bytes coming off the recorder.
///
/// The Recorder pushes chunks here as soon as it has them; the ASR session
/// is free to batch internally before flushing to the network.
pub trait AudioConsumer: Send + Sync {
    fn consume_pcm_chunk(&self, pcm: &[u8]);
}

/// What the ASR session yielded once the stream closed.
#[derive(Debug, Clone)]
pub struct RawTranscript {
    pub text: String,
    pub duration_ms: u64,
}

/// Provider-neutral final segment emitted before the whole ASR session closes.
#[derive(Debug, Clone)]
pub struct AsrFinalSegment {
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub provider_id: Option<String>,
    pub provider_session_id: Option<String>,
    pub provider_segment_id: Option<String>,
    pub sentence_id: Option<String>,
    pub sequence: Option<u64>,
    pub audio_part_index: Option<u32>,
    pub session_start_ms: Option<u64>,
    pub provider_start_ms: Option<u64>,
    pub provider_end_ms: Option<u64>,
    pub token_timestamps: Vec<TranscriptTokenTimestamp>,
}

pub type AsrFinalSegmentSink = Arc<dyn Fn(AsrFinalSegment) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct AsrDraftSegment {
    pub provider_id: String,
    pub provider_session_id: Option<String>,
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub sequence: Option<u64>,
    pub audio_part_index: Option<u32>,
    pub session_start_ms: Option<u64>,
    pub clear: bool,
}

pub type AsrDraftSegmentSink = Arc<dyn Fn(AsrDraftSegment) + Send + Sync>;

pub type AsrInterruptionSink = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Debug, Clone, Default)]
pub struct AsrSessionMetadata {
    pub provider_session_id: Option<String>,
    pub audio_part_index: Option<u32>,
    pub session_start_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrProviderCapabilities {
    pub provider_id: String,
    pub supports_realtime: bool,
    pub supports_draft_result: bool,
    pub supports_final_segment_event: bool,
    pub supports_batch_result: bool,
    pub supports_server_vad: bool,
    pub supports_vad_silence_preset: bool,
    pub supports_sentence_timestamp: bool,
    pub supports_word_timestamp: bool,
    pub supports_punctuation: bool,
    pub supports_itn: bool,
}

pub fn list_asr_provider_capabilities() -> Vec<AsrProviderCapabilities> {
    [
        "volcengine",
        bailian::PROVIDER_ID,
        "siliconflow",
        "zhipu",
        "groq",
        "whisper",
        "openrouter",
        mimo::PROVIDER_ID,
        "foundry-local-whisper",
        local::sherpa::PROVIDER_ID,
        "local-qwen3",
        "apple-speech",
    ]
    .into_iter()
    .map(asr_provider_capabilities)
    .collect()
}

pub fn asr_provider_capabilities(provider_id: &str) -> AsrProviderCapabilities {
    match provider_id {
        bailian::PROVIDER_ID => AsrProviderCapabilities {
            provider_id: provider_id.to_string(),
            supports_realtime: true,
            supports_draft_result: true,
            supports_final_segment_event: true,
            supports_batch_result: false,
            supports_server_vad: true,
            supports_vad_silence_preset: true,
            supports_sentence_timestamp: true,
            supports_word_timestamp: true,
            supports_punctuation: true,
            supports_itn: true,
        },
        "volcengine" => AsrProviderCapabilities {
            provider_id: provider_id.to_string(),
            supports_realtime: true,
            supports_draft_result: false,
            supports_final_segment_event: true,
            supports_batch_result: false,
            supports_server_vad: false,
            supports_vad_silence_preset: false,
            supports_sentence_timestamp: true,
            supports_word_timestamp: false,
            supports_punctuation: true,
            supports_itn: true,
        },
        other => AsrProviderCapabilities {
            provider_id: other.to_string(),
            supports_realtime: false,
            supports_draft_result: false,
            supports_final_segment_event: false,
            supports_batch_result: true,
            supports_server_vad: false,
            supports_vad_silence_preset: false,
            supports_sentence_timestamp: false,
            supports_word_timestamp: false,
            supports_punctuation: false,
            supports_itn: false,
        },
    }
}

/// User-defined hotword the ASR provider may use to bias decoding.
#[derive(Debug, Clone)]
pub struct DictionaryHotword {
    pub phrase: String,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bailian_declares_realtime_draft_timestamp_and_silence_preset() {
        let capability = asr_provider_capabilities(bailian::PROVIDER_ID);

        assert!(capability.supports_realtime);
        assert!(capability.supports_draft_result);
        assert!(capability.supports_final_segment_event);
        assert!(capability.supports_vad_silence_preset);
        assert!(capability.supports_sentence_timestamp);
        assert!(capability.supports_word_timestamp);
        assert!(!capability.supports_batch_result);
    }

    #[test]
    fn unknown_provider_defaults_to_batch_fallback() {
        let capability = asr_provider_capabilities("custom-asr");

        assert!(!capability.supports_realtime);
        assert!(!capability.supports_draft_result);
        assert!(capability.supports_batch_result);
    }
}
