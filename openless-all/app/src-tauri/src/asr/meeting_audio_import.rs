use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};

const PCM_FORMAT: u16 = 1;
const TARGET_CHANNELS: u16 = 1;
const TARGET_SAMPLE_RATE: u32 = 16_000;
const TARGET_BITS_PER_SAMPLE: u16 = 16;
const TARGET_BLOCK_ALIGN: u16 = TARGET_CHANNELS * (TARGET_BITS_PER_SAMPLE / 8);
const TARGET_BYTE_RATE: u32 = TARGET_SAMPLE_RATE * TARGET_BLOCK_ALIGN as u32;
const WAV_HEADER_BYTES: u64 = 44;
const INPUT_BUFFER_BYTES: usize = 64 * 1024;
const OUTPUT_BUFFER_SAMPLES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmWavProbe {
    pub path: PathBuf,
    pub file_name: String,
    pub size_bytes: u64,
    pub duration_ms: u64,
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    data_offset: u64,
    data_bytes: u64,
    block_align: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalizedWavInfo {
    pub pcm_bytes: u64,
    pub duration_ms: u64,
}

pub fn probe_pcm_wav(path: &Path) -> Result<PcmWavProbe> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("read audio metadata failed: {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("selected audio is not a file");
    }
    let size_bytes = metadata.len();
    if size_bytes < 12 {
        anyhow::bail!("audio file is empty or corrupt");
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("audio file name is invalid"))?
        .to_string();
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| !extension.eq_ignore_ascii_case("wav"))
        .unwrap_or(true)
    {
        anyhow::bail!("only PCM WAV audio is supported");
    }

    let mut file =
        File::open(path).with_context(|| format!("open audio file failed: {}", path.display()))?;
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff)
        .context("audio file is empty or corrupt")?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        anyhow::bail!("selected file is not a WAV file");
    }
    let declared_riff_end = u64::from(u32::from_le_bytes(riff[4..8].try_into().unwrap())) + 8;
    if declared_riff_end > size_bytes || declared_riff_end < 12 {
        anyhow::bail!("WAV RIFF length is invalid");
    }

    let mut format = None;
    let mut data = None;
    let mut cursor = 12u64;
    while cursor + 8 <= declared_riff_end {
        file.seek(SeekFrom::Start(cursor))
            .context("seek WAV chunk failed")?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header)
            .context("read WAV chunk header failed")?;
        let chunk_size = u64::from(u32::from_le_bytes(header[4..8].try_into().unwrap()));
        let chunk_data_offset = cursor + 8;
        let chunk_end = chunk_data_offset
            .checked_add(chunk_size)
            .context("WAV chunk length overflow")?;
        if chunk_end > declared_riff_end || chunk_end > size_bytes {
            anyhow::bail!("WAV chunk length is invalid");
        }
        match &header[0..4] {
            b"fmt " if format.is_none() => {
                if chunk_size < 16 {
                    anyhow::bail!("WAV fmt chunk is invalid");
                }
                let mut bytes = [0u8; 16];
                file.read_exact(&mut bytes)
                    .context("read WAV fmt chunk failed")?;
                format = Some(WavFormat {
                    audio_format: u16::from_le_bytes(bytes[0..2].try_into().unwrap()),
                    channels: u16::from_le_bytes(bytes[2..4].try_into().unwrap()),
                    sample_rate: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                    byte_rate: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                    block_align: u16::from_le_bytes(bytes[12..14].try_into().unwrap()),
                    bits_per_sample: u16::from_le_bytes(bytes[14..16].try_into().unwrap()),
                });
            }
            b"data" if data.is_none() => data = Some((chunk_data_offset, chunk_size)),
            _ => {}
        }
        cursor = chunk_end + (chunk_size & 1);
    }

    let format = format.ok_or_else(|| anyhow::anyhow!("WAV fmt chunk is missing"))?;
    let (data_offset, data_bytes) =
        data.ok_or_else(|| anyhow::anyhow!("WAV data chunk is missing"))?;
    validate_pcm_format(format, data_bytes)?;
    let frames = data_bytes / u64::from(format.block_align);
    let duration_ms =
        frames.checked_mul(1000).context("WAV duration overflow")? / u64::from(format.sample_rate);

    Ok(PcmWavProbe {
        path: path.to_path_buf(),
        file_name,
        size_bytes,
        duration_ms,
        channels: format.channels,
        sample_rate: format.sample_rate,
        bits_per_sample: format.bits_per_sample,
        data_offset,
        data_bytes,
        block_align: format.block_align,
    })
}

pub fn normalize_pcm_wav<F>(
    probe: &PcmWavProbe,
    partial_path: &Path,
    final_path: &Path,
    cancelled: &AtomicBool,
    mut on_progress: F,
) -> Result<NormalizedWavInfo>
where
    F: FnMut(f32),
{
    if cancelled.load(Ordering::Acquire) {
        anyhow::bail!("meeting audio import cancelled");
    }
    if let Some(parent) = partial_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "create audio staging directory failed: {}",
                parent.display()
            )
        })?;
    }
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "create meeting audio directory failed: {}",
                parent.display()
            )
        })?;
    }
    if final_path.exists() {
        anyhow::bail!("managed meeting audio already exists");
    }
    if partial_path.exists() {
        std::fs::remove_file(partial_path).with_context(|| {
            format!(
                "remove stale audio staging file failed: {}",
                partial_path.display()
            )
        })?;
    }

    let result = normalize_pcm_wav_inner(probe, partial_path, cancelled, &mut on_progress)
        .and_then(|info| {
            std::fs::rename(partial_path, final_path).with_context(|| {
                format!(
                    "commit managed meeting audio failed: {} -> {}",
                    partial_path.display(),
                    final_path.display()
                )
            })?;
            Ok(info)
        });
    if result.is_err() {
        let _ = std::fs::remove_file(partial_path);
    }
    result
}

fn normalize_pcm_wav_inner<F>(
    probe: &PcmWavProbe,
    partial_path: &Path,
    cancelled: &AtomicBool,
    on_progress: &mut F,
) -> Result<NormalizedWavInfo>
where
    F: FnMut(f32),
{
    let mut input = File::open(&probe.path)
        .with_context(|| format!("open source audio failed: {}", probe.path.display()))?;
    input
        .seek(SeekFrom::Start(probe.data_offset))
        .context("seek source WAV data failed")?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(partial_path)
        .with_context(|| {
            format!(
                "create audio staging file failed: {}",
                partial_path.display()
            )
        })?;
    output
        .write_all(&canonical_wav_header(0))
        .context("write staging WAV header failed")?;

    let frame_bytes = usize::from(probe.block_align);
    let chunk_bytes = (INPUT_BUFFER_BYTES / frame_bytes).max(1) * frame_bytes;
    let mut input_buffer = vec![0u8; chunk_bytes];
    let mut output_buffer = Vec::<i16>::with_capacity(OUTPUT_BUFFER_SAMPLES);
    let mut resampler = LinearResampler::new(probe.sample_rate, TARGET_SAMPLE_RATE);
    let mut remaining = probe.data_bytes;
    let mut consumed = 0u64;
    let mut output_samples = 0u64;
    on_progress(0.0);

    while remaining > 0 {
        if cancelled.load(Ordering::Acquire) {
            anyhow::bail!("meeting audio import cancelled");
        }
        let read_len = remaining.min(input_buffer.len() as u64) as usize;
        input
            .read_exact(&mut input_buffer[..read_len])
            .context("source WAV ended before declared data length")?;
        for frame in input_buffer[..read_len].chunks_exact(frame_bytes) {
            let mono = downmix_frame(frame, probe.channels)?;
            resampler.push(mono, |sample| {
                output_buffer.push(sample);
                output_samples += 1;
                if output_buffer.len() >= OUTPUT_BUFFER_SAMPLES {
                    write_samples(&mut output, &mut output_buffer)?;
                }
                Ok(())
            })?;
        }
        remaining -= read_len as u64;
        consumed += read_len as u64;
        on_progress((consumed as f64 / probe.data_bytes as f64) as f32);
    }
    resampler.finish(|sample| {
        output_buffer.push(sample);
        output_samples += 1;
        if output_buffer.len() >= OUTPUT_BUFFER_SAMPLES {
            write_samples(&mut output, &mut output_buffer)?;
        }
        Ok(())
    })?;
    write_samples(&mut output, &mut output_buffer)?;

    let pcm_bytes = output_samples
        .checked_mul(u64::from(TARGET_BLOCK_ALIGN))
        .context("normalized WAV length overflow")?;
    let pcm_bytes_u32 =
        u32::try_from(pcm_bytes).context("normalized WAV exceeds RIFF size limit")?;
    output
        .seek(SeekFrom::Start(0))
        .context("seek staging WAV header failed")?;
    output
        .write_all(&canonical_wav_header(pcm_bytes_u32))
        .context("finalize staging WAV header failed")?;
    output.flush().context("flush staging WAV failed")?;
    output.sync_all().context("sync staging WAV failed")?;
    on_progress(1.0);

    Ok(NormalizedWavInfo {
        pcm_bytes,
        duration_ms: pcm_bytes.saturating_mul(1000) / u64::from(TARGET_BYTE_RATE),
    })
}

#[derive(Debug, Clone, Copy)]
struct WavFormat {
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    byte_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
}

fn validate_pcm_format(format: WavFormat, data_bytes: u64) -> Result<()> {
    if format.audio_format != PCM_FORMAT {
        anyhow::bail!("WAV must use uncompressed linear PCM");
    }
    if !(1..=2).contains(&format.channels) {
        anyhow::bail!("PCM WAV must be mono or stereo");
    }
    if format.bits_per_sample != 16 {
        anyhow::bail!("PCM WAV must use 16-bit samples");
    }
    if !(8_000..=192_000).contains(&format.sample_rate) {
        anyhow::bail!("PCM WAV sample rate is unsupported");
    }
    let expected_align = format.channels * (format.bits_per_sample / 8);
    let expected_rate = format.sample_rate * u32::from(expected_align);
    if format.block_align != expected_align || format.byte_rate != expected_rate {
        anyhow::bail!("PCM WAV format fields are inconsistent");
    }
    if data_bytes == 0 || data_bytes % u64::from(format.block_align) != 0 {
        anyhow::bail!("PCM WAV data length is invalid");
    }
    Ok(())
}

fn downmix_frame(frame: &[u8], channels: u16) -> Result<i16> {
    let mut sum = 0i32;
    for sample in frame.chunks_exact(2).take(usize::from(channels)) {
        sum += i32::from(i16::from_le_bytes([sample[0], sample[1]]));
    }
    let channels = i32::from(channels);
    let rounded = if sum >= 0 {
        (sum + channels / 2) / channels
    } else {
        (sum - channels / 2) / channels
    };
    Ok(rounded.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16)
}

fn write_samples(output: &mut File, samples: &mut Vec<i16>) -> Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples.drain(..) {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    output
        .write_all(&bytes)
        .context("write normalized WAV samples failed")
}

struct LinearResampler {
    input_rate: u32,
    output_rate: u32,
    input_index: u64,
    output_index: u64,
    previous: Option<i16>,
}

impl LinearResampler {
    fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            input_rate,
            output_rate,
            input_index: 0,
            output_index: 0,
            previous: None,
        }
    }

    fn push<F>(&mut self, current: i16, mut emit: F) -> Result<()>
    where
        F: FnMut(i16) -> Result<()>,
    {
        let current_index = self.input_index;
        if let Some(previous) = self.previous {
            let current_position = current_index * u64::from(self.output_rate);
            loop {
                let position = self.output_index * u64::from(self.input_rate);
                if position > current_position {
                    break;
                }
                let floor = position / u64::from(self.output_rate);
                let remainder = position % u64::from(self.output_rate);
                let sample = if floor == current_index {
                    current
                } else {
                    interpolate(previous, current, remainder, self.output_rate)
                };
                emit(sample)?;
                self.output_index += 1;
            }
        } else {
            emit(current)?;
            self.output_index = 1;
        }
        self.previous = Some(current);
        self.input_index += 1;
        Ok(())
    }

    fn finish<F>(&mut self, mut emit: F) -> Result<()>
    where
        F: FnMut(i16) -> Result<()>,
    {
        let Some(last) = self.previous else {
            return Ok(());
        };
        let target_samples = self
            .input_index
            .saturating_mul(u64::from(self.output_rate))
            .div_ceil(u64::from(self.input_rate));
        while self.output_index < target_samples {
            emit(last)?;
            self.output_index += 1;
        }
        Ok(())
    }
}

fn interpolate(left: i16, right: i16, numerator: u64, denominator: u32) -> i16 {
    let denominator = i64::from(denominator);
    let numerator = numerator as i64;
    let weighted = i64::from(left) * (denominator - numerator) + i64::from(right) * numerator;
    let rounded = if weighted >= 0 {
        (weighted + denominator / 2) / denominator
    } else {
        (weighted - denominator / 2) / denominator
    };
    rounded.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

fn canonical_wav_header(pcm_bytes: u32) -> [u8; WAV_HEADER_BYTES as usize] {
    let mut header = [0u8; WAV_HEADER_BYTES as usize];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(36 + pcm_bytes).to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&PCM_FORMAT.to_le_bytes());
    header[22..24].copy_from_slice(&TARGET_CHANNELS.to_le_bytes());
    header[24..28].copy_from_slice(&TARGET_SAMPLE_RATE.to_le_bytes());
    header[28..32].copy_from_slice(&TARGET_BYTE_RATE.to_le_bytes());
    header[32..34].copy_from_slice(&TARGET_BLOCK_ALIGN.to_le_bytes());
    header[34..36].copy_from_slice(&TARGET_BITS_PER_SAMPLE.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&pcm_bytes.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("meeting-audio-import-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn wav(channels: u16, sample_rate: u32, frames: &[[i16; 2]]) -> Vec<u8> {
        let frame_bytes = usize::from(channels) * 2;
        let data_bytes = frames.len() * frame_bytes;
        let mut bytes = Vec::with_capacity(44 + data_bytes);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36u32 + data_bytes as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&PCM_FORMAT.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data_bytes as u32).to_le_bytes());
        for frame in frames {
            bytes.extend_from_slice(&frame[0].to_le_bytes());
            if channels == 2 {
                bytes.extend_from_slice(&frame[1].to_le_bytes());
            }
        }
        bytes
    }

    #[test]
    fn probe_accepts_pcm_wav_with_extra_chunk_and_chinese_path() {
        let dir = temp_dir();
        let path = dir.join("会议 录音.wav");
        let mut bytes = wav(1, 16_000, &[[1, 0], [2, 0]]);
        bytes.splice(36..36, [b'J', b'U', b'N', b'K', 2, 0, 0, 0, 7, 8]);
        let riff_len = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&riff_len.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();

        let probe = probe_pcm_wav(&path).unwrap();

        assert_eq!(probe.file_name, "会议 录音.wav");
        assert_eq!(probe.channels, 1);
        assert_eq!(probe.sample_rate, 16_000);
        assert_eq!(probe.data_bytes, 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn probe_rejects_extension_spoof_and_compressed_wav() {
        let dir = temp_dir();
        let spoof = dir.join("audio.wav");
        std::fs::write(&spoof, b"not-a-wave!!").unwrap();
        assert!(probe_pcm_wav(&spoof)
            .unwrap_err()
            .to_string()
            .contains("not a WAV"));

        let compressed = dir.join("compressed.wav");
        let mut bytes = wav(1, 16_000, &[[1, 0]]);
        bytes[20..22].copy_from_slice(&3u16.to_le_bytes());
        std::fs::write(&compressed, bytes).unwrap();
        assert!(probe_pcm_wav(&compressed)
            .unwrap_err()
            .to_string()
            .contains("uncompressed"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn normalize_downmixes_and_resamples_without_touching_source() {
        let dir = temp_dir();
        let source = dir.join("stereo-8k.wav");
        let partial = dir.join("job.partial");
        let final_path = dir.join("managed").join("part-0001.wav");
        std::fs::write(
            &source,
            wav(2, 8_000, &[[1000, -1000], [2000, 0], [3000, 1000]]),
        )
        .unwrap();
        let before = std::fs::read(&source).unwrap();
        let before_hash = Sha256::digest(&before);
        let before_modified = std::fs::metadata(&source).unwrap().modified().unwrap();
        let probe = probe_pcm_wav(&source).unwrap();

        let info = normalize_pcm_wav(
            &probe,
            &partial,
            &final_path,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        let managed = std::fs::read(&final_path).unwrap();
        assert_eq!(&managed[0..4], b"RIFF");
        assert_eq!(u16::from_le_bytes(managed[22..24].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(managed[24..28].try_into().unwrap()),
            16_000
        );
        assert_eq!(u16::from_le_bytes(managed[34..36].try_into().unwrap()), 16);
        assert_eq!(info.pcm_bytes, 12);
        assert!(!partial.exists());
        assert_eq!(Sha256::digest(std::fs::read(&source).unwrap()), before_hash);
        assert_eq!(
            std::fs::metadata(&source).unwrap().modified().unwrap(),
            before_modified
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn normalize_cancel_removes_partial_and_preserves_source() {
        let dir = temp_dir();
        let source = dir.join("source.wav");
        let partial = dir.join("job.partial");
        let final_path = dir.join("managed.wav");
        std::fs::write(&source, wav(1, 16_000, &[[1, 0], [2, 0]])).unwrap();
        let original = std::fs::read(&source).unwrap();
        let probe = probe_pcm_wav(&source).unwrap();
        let cancelled = AtomicBool::new(true);

        assert!(
            normalize_pcm_wav(&probe, &partial, &final_path, &cancelled, |_| {})
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        assert!(!partial.exists());
        assert!(!final_path.exists());
        assert_eq!(std::fs::read(&source).unwrap(), original);
        let _ = std::fs::remove_dir_all(dir);
    }
}
