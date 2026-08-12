use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures_util::stream::{self, Stream};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const WAV_HEADER_BYTES: u64 = 44;
const PCM_CHUNK_BYTES: usize = 64 * 1024;
const PCM_FORMAT: u16 = 1;
const CHANNELS: u16 = 1;
const SAMPLE_RATE: u32 = 16_000;
const BITS_PER_SAMPLE: u16 = 16;
const BLOCK_ALIGN: u16 = CHANNELS * (BITS_PER_SAMPLE / 8);
const BYTE_RATE: u32 = SAMPLE_RATE * BLOCK_ALIGN as u32;

pub type MeetingAudioByteStream =
    Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send + 'static>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeetingAudioSource {
    SingleWav(PathBuf),
    SegmentedWav(Vec<PathBuf>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeetingAudioInfo {
    pub pcm_bytes: u64,
    pub content_length: u64,
    pub duration_ms: u64,
}

#[derive(Debug)]
struct StreamState {
    parts: Vec<WavPart>,
    part_index: usize,
    current: Option<tokio::fs::File>,
    remaining: u64,
    header: Option<Bytes>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
struct WavPart {
    path: PathBuf,
    pcm_bytes: u64,
}

impl MeetingAudioSource {
    pub fn from_path(path: &Path) -> Result<Self> {
        if path.is_file() {
            return Ok(Self::SingleWav(path.to_path_buf()));
        }
        if !path.is_dir() {
            anyhow::bail!("meeting recording not found");
        }

        let mut indexed_parts = std::fs::read_dir(path)
            .with_context(|| format!("read meeting recording dir failed: {}", path.display()))?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let path = entry.path();
                let index = path
                    .is_file()
                    .then(|| part_index(path.file_name()?.to_str()?))??;
                Some((index, path))
            })
            .collect::<Vec<_>>();
        indexed_parts.sort_by_key(|(index, _)| *index);
        if indexed_parts.is_empty() {
            anyhow::bail!("meeting recording not found");
        }
        for (expected, (actual, _)) in (1u64..).zip(&indexed_parts) {
            if *actual != expected {
                anyhow::bail!(
                    "meeting recording part sequence is incomplete: expected part-{expected:04}.wav"
                );
            }
        }
        Ok(Self::SegmentedWav(
            indexed_parts.into_iter().map(|(_, path)| path).collect(),
        ))
    }

    pub fn inspect(&self) -> Result<MeetingAudioInfo> {
        let parts = self.inspect_parts()?;
        let pcm_bytes = parts.iter().try_fold(0u64, |total, part| {
            total
                .checked_add(part.pcm_bytes)
                .context("meeting audio length overflow")
        })?;
        if pcm_bytes == 0 {
            anyhow::bail!("meeting recording is empty");
        }
        if pcm_bytes > u32::MAX as u64 {
            anyhow::bail!("meeting recording exceeds WAV size limit");
        }
        Ok(MeetingAudioInfo {
            pcm_bytes,
            content_length: WAV_HEADER_BYTES + pcm_bytes,
            duration_ms: pcm_bytes.saturating_mul(1000) / BYTE_RATE as u64,
        })
    }

    pub fn into_stream(
        self,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(MeetingAudioInfo, MeetingAudioByteStream)> {
        let parts = self.inspect_parts()?;
        let pcm_bytes = parts.iter().try_fold(0u64, |total, part| {
            total
                .checked_add(part.pcm_bytes)
                .context("meeting audio length overflow")
        })?;
        if pcm_bytes == 0 {
            anyhow::bail!("meeting recording is empty");
        }
        if pcm_bytes > u32::MAX as u64 {
            anyhow::bail!("meeting recording exceeds WAV size limit");
        }
        let info = MeetingAudioInfo {
            pcm_bytes,
            content_length: WAV_HEADER_BYTES + pcm_bytes,
            duration_ms: pcm_bytes.saturating_mul(1000) / BYTE_RATE as u64,
        };
        let state = StreamState {
            parts,
            part_index: 0,
            current: None,
            remaining: 0,
            header: Some(Bytes::copy_from_slice(&wav_header(pcm_bytes as u32))),
            cancelled,
        };
        let stream = stream::unfold(state, |mut state| async move {
            if state.cancelled.load(Ordering::Acquire) {
                return Some((
                    Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "meeting audio streaming cancelled",
                    )),
                    state,
                ));
            }
            if let Some(header) = state.header.take() {
                return Some((Ok(header), state));
            }
            loop {
                if state.current.is_none() {
                    let part = state.parts.get(state.part_index)?.clone();
                    let mut file = match tokio::fs::File::open(&part.path).await {
                        Ok(file) => file,
                        Err(error) => return Some((Err(error), state)),
                    };
                    if let Err(error) = file.seek(io::SeekFrom::Start(WAV_HEADER_BYTES)).await {
                        return Some((Err(error), state));
                    }
                    state.remaining = part.pcm_bytes;
                    state.current = Some(file);
                }

                if state.remaining == 0 {
                    state.current = None;
                    state.part_index += 1;
                    if state.part_index >= state.parts.len() {
                        return None;
                    }
                    continue;
                }

                let chunk_len = state.remaining.min(PCM_CHUNK_BYTES as u64) as usize;
                let mut chunk = vec![0u8; chunk_len];
                let read = match state.current.as_mut().unwrap().read(&mut chunk).await {
                    Ok(0) => {
                        return Some((
                            Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "meeting WAV ended before declared data length",
                            )),
                            state,
                        ));
                    }
                    Ok(read) => read,
                    Err(error) => return Some((Err(error), state)),
                };
                chunk.truncate(read);
                state.remaining -= read as u64;
                return Some((Ok(Bytes::from(chunk)), state));
            }
        });
        Ok((info, Box::pin(stream)))
    }

    fn paths(&self) -> &[PathBuf] {
        match self {
            Self::SingleWav(path) => std::slice::from_ref(path),
            Self::SegmentedWav(parts) => parts,
        }
    }

    fn inspect_parts(&self) -> Result<Vec<WavPart>> {
        self.paths()
            .iter()
            .map(|path| {
                let pcm_bytes = inspect_wav(path)?;
                Ok(WavPart {
                    path: path.clone(),
                    pcm_bytes,
                })
            })
            .collect()
    }
}

fn part_index(name: &str) -> Option<u64> {
    name.strip_prefix("part-")?
        .strip_suffix(".wav")?
        .parse()
        .ok()
}

fn inspect_wav(path: &Path) -> Result<u64> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("read meeting WAV failed: {}", path.display()))?;
    let mut header = [0u8; WAV_HEADER_BYTES as usize];
    std::io::Read::read_exact(&mut file, &mut header)
        .context("meeting recording is empty or corrupt")?;
    if &header[0..4] != b"RIFF"
        || &header[8..12] != b"WAVE"
        || &header[12..16] != b"fmt "
        || &header[36..40] != b"data"
        || u32::from_le_bytes(header[16..20].try_into().unwrap()) != 16
        || u16::from_le_bytes(header[20..22].try_into().unwrap()) != PCM_FORMAT
        || u16::from_le_bytes(header[22..24].try_into().unwrap()) != CHANNELS
        || u32::from_le_bytes(header[24..28].try_into().unwrap()) != SAMPLE_RATE
        || u32::from_le_bytes(header[28..32].try_into().unwrap()) != BYTE_RATE
        || u16::from_le_bytes(header[32..34].try_into().unwrap()) != BLOCK_ALIGN
        || u16::from_le_bytes(header[34..36].try_into().unwrap()) != BITS_PER_SAMPLE
    {
        anyhow::bail!("meeting recording must be 16 kHz mono 16-bit PCM WAV");
    }
    let pcm_bytes = u32::from_le_bytes(header[40..44].try_into().unwrap()) as u64;
    let file_len = file
        .metadata()
        .with_context(|| format!("read meeting WAV metadata failed: {}", path.display()))?
        .len();
    if pcm_bytes == 0 || pcm_bytes % BLOCK_ALIGN as u64 != 0 || file_len != WAV_HEADER_BYTES + pcm_bytes
    {
        anyhow::bail!("meeting recording is empty or corrupt");
    }
    Ok(pcm_bytes)
}

fn wav_header(pcm_bytes: u32) -> [u8; WAV_HEADER_BYTES as usize] {
    let mut header = [0u8; WAV_HEADER_BYTES as usize];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(36 + pcm_bytes).to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&PCM_FORMAT.to_le_bytes());
    header[22..24].copy_from_slice(&CHANNELS.to_le_bytes());
    header[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    header[28..32].copy_from_slice(&BYTE_RATE.to_le_bytes());
    header[32..34].copy_from_slice(&BLOCK_ALIGN.to_le_bytes());
    header[34..36].copy_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&pcm_bytes.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::wav::encode_wav_16k_mono;
    use futures_util::TryStreamExt;
    use uuid::Uuid;

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("meeting-audio-source-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn segmented_source_streams_one_header_and_all_pcm() {
        let dir = temp_dir();
        std::fs::write(dir.join("part-0001.wav"), encode_wav_16k_mono(&[1, 2])).unwrap();
        std::fs::write(dir.join("part-0002.wav"), encode_wav_16k_mono(&[3, 4, 5])).unwrap();
        let source = MeetingAudioSource::from_path(&dir).unwrap();
        let (info, stream) = source.into_stream(Arc::new(AtomicBool::new(false))).unwrap();
        let bytes = stream
            .try_fold(Vec::new(), |mut output, chunk| async move {
                output.extend_from_slice(&chunk);
                Ok(output)
            })
            .await
            .unwrap();

        assert_eq!(info.pcm_bytes, 10);
        assert_eq!(info.content_length, 54);
        assert_eq!(bytes.len() as u64, info.content_length);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(bytes.windows(4).filter(|window| *window == b"RIFF").count(), 1);
        assert_eq!(&bytes[44..], &encode_wav_16k_mono(&[1, 2, 3, 4, 5])[44..]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn segmented_source_rejects_missing_part_number() {
        let dir = temp_dir();
        std::fs::write(dir.join("part-0001.wav"), encode_wav_16k_mono(&[1])).unwrap();
        std::fs::write(dir.join("part-0003.wav"), encode_wav_16k_mono(&[2])).unwrap();
        let error = MeetingAudioSource::from_path(&dir).unwrap_err().to_string();
        assert!(error.contains("part-0002.wav"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn source_rejects_noncanonical_wav_format() {
        let dir = temp_dir();
        let path = dir.join("audio.wav");
        let mut wav = encode_wav_16k_mono(&[1, 2]);
        wav[24..28].copy_from_slice(&8_000u32.to_le_bytes());
        std::fs::write(&path, wav).unwrap();
        let error = MeetingAudioSource::from_path(&path)
            .unwrap()
            .inspect()
            .unwrap_err()
            .to_string();
        assert!(error.contains("16 kHz mono 16-bit PCM WAV"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn source_stops_before_emitting_when_cancelled() {
        let dir = temp_dir();
        let path = dir.join("audio.wav");
        std::fs::write(&path, encode_wav_16k_mono(&[1, 2])).unwrap();
        let cancelled = Arc::new(AtomicBool::new(true));
        let (_, mut stream) = MeetingAudioSource::from_path(&path)
            .unwrap()
            .into_stream(cancelled)
            .unwrap();
        assert_eq!(stream.try_next().await.unwrap_err().kind(), io::ErrorKind::Interrupted);
        let _ = std::fs::remove_dir_all(dir);
    }
}
