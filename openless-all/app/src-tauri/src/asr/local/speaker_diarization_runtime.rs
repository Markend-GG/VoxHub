use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result};
use parking_lot::Mutex;

use crate::asr::MeetingAudioSource;
use crate::types::SpeakerTurn;

use super::speaker_diarization::{
    embedding_model_path, ensure_package_ready, segmentation_model_path, CLUSTERING_THRESHOLD,
    SAMPLE_RATE,
};

const LOCAL_DIARIZATION_RUNTIME_HEADROOM_BYTES: u64 = 512 * 1024 * 1024;
static LOCAL_DIARIZATION_ACTIVE_MODEL: LazyLock<Mutex<Option<String>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone, PartialEq)]
pub struct LocalDiarizationOutput {
    pub turns: Vec<SpeakerTurn>,
    pub detected_speaker_count: u32,
}

struct LocalDiarizationSlot {
    model_id: String,
}

impl LocalDiarizationSlot {
    fn acquire(model_id: &str) -> Result<Self> {
        let mut active = LOCAL_DIARIZATION_ACTIVE_MODEL.lock();
        if active.is_some() {
            anyhow::bail!("another local diarization job is already running");
        }
        *active = Some(model_id.to_string());
        Ok(Self {
            model_id: model_id.to_string(),
        })
    }
}

impl Drop for LocalDiarizationSlot {
    fn drop(&mut self) {
        let mut active = LOCAL_DIARIZATION_ACTIVE_MODEL.lock();
        if active.as_deref() == Some(self.model_id.as_str()) {
            *active = None;
        }
    }
}

pub fn model_is_active(model_id: &str) -> bool {
    LOCAL_DIARIZATION_ACTIVE_MODEL.lock().as_deref() == Some(model_id)
}

pub fn run_local_diarization(
    source: MeetingAudioSource,
    model_id: &str,
    expected_speaker_count: Option<u32>,
    cancelled: Arc<AtomicBool>,
) -> Result<LocalDiarizationOutput> {
    let _slot = LocalDiarizationSlot::acquire(model_id)?;
    ensure_package_ready(model_id)?;
    let info = source.inspect()?;
    preflight_waveform_allocation(info.pcm_bytes)?;
    if cancelled.load(Ordering::Acquire) {
        anyhow::bail!("local diarization cancelled");
    }
    let (_, waveform) = source.read_waveform(&cancelled)?;
    if cancelled.load(Ordering::Acquire) {
        anyhow::bail!("local diarization cancelled");
    }

    #[cfg(target_os = "windows")]
    {
        let output = run_sherpa_diarization(
            &waveform,
            model_id,
            expected_speaker_count,
            info.duration_ms,
        )?;
        if cancelled.load(Ordering::Acquire) {
            anyhow::bail!("local diarization cancelled");
        }
        Ok(output)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (waveform, expected_speaker_count, info);
        anyhow::bail!("local speaker diarization currently supports Windows only")
    }
}

fn preflight_waveform_allocation(pcm_bytes: u64) -> Result<()> {
    let required = required_waveform_memory_bytes(pcm_bytes)?;
    if let Some(available) = available_physical_memory_bytes() {
        if available < required {
            anyhow::bail!(
                "insufficient memory for local diarization: available={available} required={required}"
            );
        }
    }
    Ok(())
}

fn required_waveform_memory_bytes(pcm_bytes: u64) -> Result<u64> {
    let sample_count = pcm_bytes / 2;
    if sample_count > i32::MAX as u64 {
        anyhow::bail!("meeting audio exceeds the local diarization sample limit");
    }
    let waveform_bytes = sample_count
        .checked_mul(std::mem::size_of::<f32>() as u64)
        .context("local diarization waveform size overflow")?;
    waveform_bytes
        .checked_add(LOCAL_DIARIZATION_RUNTIME_HEADROOM_BYTES)
        .context("local diarization memory estimate overflow")
}

#[cfg(target_os = "windows")]
fn available_physical_memory_bytes() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status) }
        .ok()
        .map(|_| status.ullAvailPhys)
}

#[cfg(not(target_os = "windows"))]
fn available_physical_memory_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "windows")]
fn run_sherpa_diarization(
    waveform: &[f32],
    model_id: &str,
    expected_speaker_count: Option<u32>,
    duration_ms: u64,
) -> Result<LocalDiarizationOutput> {
    use sherpa_onnx::{
        FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
        OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
        SpeakerEmbeddingExtractorConfig,
    };

    let num_clusters = expected_speaker_count
        .map(i32::try_from)
        .transpose()
        .context("expected speaker count is too large")?
        .unwrap_or(-1);
    let config = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                model: Some(
                    segmentation_model_path(model_id)?
                        .to_string_lossy()
                        .into_owned(),
                ),
            },
            num_threads: 2,
            debug: false,
            provider: Some("cpu".to_string()),
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: Some(
                embedding_model_path(model_id)?
                    .to_string_lossy()
                    .into_owned(),
            ),
            num_threads: 2,
            debug: false,
            provider: Some("cpu".to_string()),
        },
        clustering: FastClusteringConfig {
            num_clusters,
            threshold: CLUSTERING_THRESHOLD,
        },
        ..Default::default()
    };
    let diarizer = OfflineSpeakerDiarization::create(&config)
        .context("create sherpa-onnx local diarization runtime failed")?;
    if diarizer.sample_rate() != SAMPLE_RATE as i32 {
        anyhow::bail!(
            "local diarization model sample rate mismatch: actual={} expected={SAMPLE_RATE}",
            diarizer.sample_rate()
        );
    }
    let result = diarizer
        .process(waveform)
        .context("sherpa-onnx local diarization returned no result")?;
    let turns = normalize_sherpa_turns(result.sort_by_start_time(), duration_ms);
    if turns.is_empty() {
        anyhow::bail!("local diarization returned no speaker turns");
    }
    let detected_speaker_count = turns
        .iter()
        .map(|turn| turn.speaker_id.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len() as u32;
    Ok(LocalDiarizationOutput {
        turns,
        detected_speaker_count,
    })
}

#[cfg(target_os = "windows")]
fn normalize_sherpa_turns(
    segments: Vec<sherpa_onnx::OfflineSpeakerDiarizationSegment>,
    duration_ms: u64,
) -> Vec<SpeakerTurn> {
    let normalized = segments
        .into_iter()
        .filter_map(|segment| {
            if !segment.start.is_finite()
                || !segment.end.is_finite()
                || segment.speaker < 0
                || segment.end <= segment.start
            {
                return None;
            }
            let start_ms = ((segment.start.max(0.0) * 1000.0).round() as u64).min(duration_ms);
            let end_ms = ((segment.end.max(0.0) * 1000.0).round() as u64).min(duration_ms);
            (end_ms > start_ms).then_some((segment.speaker, start_ms, end_ms))
        })
        .collect::<Vec<_>>();
    normalize_raw_turns(&normalized)
}

fn normalize_raw_turns(raw: &[(i32, u64, u64)]) -> Vec<SpeakerTurn> {
    let mut speaker_map = HashMap::<i32, String>::new();
    let mut turns = raw
        .iter()
        .map(|(speaker, start_ms, end_ms)| {
            let next_index = speaker_map.len();
            let speaker_id = speaker_map
                .entry(*speaker)
                .or_insert_with(|| format!("speaker-{next_index}"))
                .clone();
            SpeakerTurn {
                speaker_id,
                start_ms: *start_ms,
                end_ms: *end_ms,
                confidence: None,
                overlapping: false,
            }
        })
        .collect::<Vec<_>>();
    for index in 0..turns.len() {
        turns[index].overlapping = turns.iter().enumerate().any(|(other_index, other)| {
            index != other_index
                && turns[index].speaker_id != other.speaker_id
                && ranges_overlap(
                    turns[index].start_ms,
                    turns[index].end_ms,
                    other.start_ms,
                    other.end_ms,
                ) > 0
        });
    }
    turns
}

fn ranges_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> u64 {
    a_end.min(b_end).saturating_sub(a_start.max(b_start))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_turns_use_first_seen_stable_ids_and_mark_cross_speaker_overlap() {
        let turns = normalize_raw_turns(&[(7, 0, 1_000), (3, 800, 1_500), (7, 1_500, 2_000)]);
        assert_eq!(turns[0].speaker_id, "speaker-0");
        assert_eq!(turns[1].speaker_id, "speaker-1");
        assert_eq!(turns[2].speaker_id, "speaker-0");
        assert!(turns[0].overlapping);
        assert!(turns[1].overlapping);
        assert!(!turns[2].overlapping);
    }

    #[test]
    fn preflight_rejects_sample_counts_that_sherpa_cannot_address() {
        let pcm_bytes = (i32::MAX as u64 + 1) * 2;
        assert!(preflight_waveform_allocation(pcm_bytes).is_err());
    }

    #[test]
    fn one_hour_waveform_memory_estimate_includes_runtime_headroom() {
        let pcm_bytes = 16_000u64 * 2 * 60 * 60;
        assert_eq!(
            required_waveform_memory_bytes(pcm_bytes).unwrap(),
            16_000u64 * 4 * 60 * 60 + LOCAL_DIARIZATION_RUNTIME_HEADROOM_BYTES
        );
    }
}
