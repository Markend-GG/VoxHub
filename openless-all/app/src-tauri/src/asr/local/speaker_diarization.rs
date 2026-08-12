use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

use super::download::{
    build_client, download_one, partial_actual_size, DownloadPhase, DownloadProgress,
};

pub const DEFAULT_PACKAGE_ID: &str = "sherpa-pyannote-3dspeaker-zh-v1";
pub const DOWNLOAD_EVENT: &str = "speaker-diarization-model-download-progress";

pub const SEGMENTATION_ARCHIVE_NAME: &str =
    "sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
pub const SEGMENTATION_ARCHIVE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
pub const SEGMENTATION_ARCHIVE_SIZE: u64 = 6_958_444;
pub const SEGMENTATION_ARCHIVE_SHA256: &str =
    "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488";
pub const SEGMENTATION_ARCHIVE_ROOT: &str = "sherpa-onnx-pyannote-segmentation-3-0";
pub const SEGMENTATION_ARCHIVE_MODEL_PATH: &str = "model.int8.onnx";
pub const SEGMENTATION_MODEL_NAME: &str = "segmentation.int8.onnx";
pub const SEGMENTATION_MODEL_SIZE: u64 = 1_540_506;
pub const SEGMENTATION_MODEL_SHA256: &str =
    "d582f4b4c6b48205de7e0643c57df0df5615a3c176189be3fc461e9d18827b5d";

pub const EMBEDDING_SOURCE_NAME: &str =
    "3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx";
pub const EMBEDDING_SOURCE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx";
pub const EMBEDDING_SOURCE_SIZE: u64 = 39_593_761;
pub const EMBEDDING_SOURCE_SHA256: &str =
    "1a331345f04805badbb495c775a6ddffcdd1a732567d5ec8b3d5749e3c7a5e4b";
pub const EMBEDDING_MODEL_NAME: &str = "embedding.onnx";

pub const MANIFEST_NAME: &str = "model-manifest.json";
pub const TOTAL_DOWNLOAD_BYTES: u64 = SEGMENTATION_ARCHIVE_SIZE + EMBEDDING_SOURCE_SIZE;
pub const SAMPLE_RATE: u32 = 16_000;
pub const CLUSTERING_THRESHOLD: f32 = 0.90;
pub const SUPPORTED_PLATFORMS: &[&str] = &["windows-x86_64"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerDiarizationModelReadiness {
    Missing,
    Downloading,
    Ready,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerDiarizationModelDescriptor {
    pub id: String,
    pub display_name: String,
    pub version: String,
    pub source: String,
    pub supported_platforms: Vec<String>,
    pub readiness: SpeakerDiarizationModelReadiness,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub sample_rate: u32,
    pub clustering_threshold: f32,
    pub max_recommended_duration_ms: Option<u64>,
    pub memory_tier: Option<String>,
    pub experimental: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerDiarizationModelManifest {
    pub package_id: String,
    pub version: String,
    pub segmentation_file: String,
    pub segmentation_sha256: String,
    pub embedding_file: String,
    pub embedding_sha256: String,
    pub sample_rate: u32,
    pub clustering_threshold: f32,
}

impl Default for SpeakerDiarizationModelManifest {
    fn default() -> Self {
        Self {
            package_id: DEFAULT_PACKAGE_ID.to_string(),
            version: "1".to_string(),
            segmentation_file: SEGMENTATION_MODEL_NAME.to_string(),
            segmentation_sha256: SEGMENTATION_MODEL_SHA256.to_string(),
            embedding_file: EMBEDDING_MODEL_NAME.to_string(),
            embedding_sha256: EMBEDDING_SOURCE_SHA256.to_string(),
            sample_rate: SAMPLE_RATE,
            clustering_threshold: CLUSTERING_THRESHOLD,
        }
    }
}

pub fn package_id_is_known(id: &str) -> bool {
    id == DEFAULT_PACKAGE_ID
}

pub fn validate_package_id(id: &str) -> Result<()> {
    if package_id_is_known(id) {
        Ok(())
    } else {
        anyhow::bail!("unknown speaker diarization model package: {id}")
    }
}

pub fn models_root() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        crate::persistence::speaker_diarization_models_root()
    }
    #[cfg(not(target_os = "windows"))]
    {
        let root = std::env::temp_dir().join("openless-speaker-diarization");
        std::fs::create_dir_all(&root)
            .with_context(|| format!("create speaker diarization root failed: {}", root.display()))?;
        Ok(root)
    }
}

pub fn package_dir(id: &str) -> Result<PathBuf> {
    validate_package_id(id)?;
    Ok(models_root()?.join(id))
}

pub fn partial_package_dir(id: &str) -> Result<PathBuf> {
    validate_package_id(id)?;
    Ok(models_root()?.join(format!("{id}.partial")))
}

pub fn segmentation_model_path(id: &str) -> Result<PathBuf> {
    Ok(package_dir(id)?.join(SEGMENTATION_MODEL_NAME))
}

pub fn embedding_model_path(id: &str) -> Result<PathBuf> {
    Ok(package_dir(id)?.join(EMBEDDING_MODEL_NAME))
}

pub fn package_descriptor(id: &str, downloading: bool) -> Result<SpeakerDiarizationModelDescriptor> {
    validate_package_id(id)?;
    let dir = package_dir(id)?;
    let partial_dir = partial_package_dir(id)?;
    let validation = validate_package_dir(&dir);
    let (readiness, error) = if downloading {
        (SpeakerDiarizationModelReadiness::Downloading, None)
    } else if validation.is_ok() {
        (SpeakerDiarizationModelReadiness::Ready, None)
    } else if package_has_any_files(&dir) {
        (
            SpeakerDiarizationModelReadiness::Invalid,
            validation.err().map(|error| format!("{error:#}")),
        )
    } else {
        (SpeakerDiarizationModelReadiness::Missing, None)
    };
    let downloaded_bytes = if readiness == SpeakerDiarizationModelReadiness::Ready {
        TOTAL_DOWNLOAD_BYTES
    } else {
        downloaded_bytes(&dir, &partial_dir)
    };
    Ok(SpeakerDiarizationModelDescriptor {
        id: id.to_string(),
        display_name: "Sherpa-ONNX Pyannote 3.0 + 3D-Speaker (中文会议)".to_string(),
        version: "1".to_string(),
        source: format!(
            "sherpa-onnx official releases: {SEGMENTATION_ARCHIVE_NAME} + {EMBEDDING_SOURCE_NAME}"
        ),
        supported_platforms: SUPPORTED_PLATFORMS
            .iter()
            .map(|platform| (*platform).to_string())
            .collect(),
        readiness,
        downloaded_bytes,
        total_bytes: TOTAL_DOWNLOAD_BYTES,
        sample_rate: SAMPLE_RATE,
        clustering_threshold: CLUSTERING_THRESHOLD,
        max_recommended_duration_ms: None,
        memory_tier: None,
        experimental: true,
        error,
    })
}

pub fn ensure_package_ready(id: &str) -> Result<()> {
    validate_package_dir(&package_dir(id)?)
}

pub fn validate_package_dir(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        anyhow::bail!("speaker diarization model package is missing: {}", dir.display());
    }
    verify_file(
        &dir.join(SEGMENTATION_MODEL_NAME),
        SEGMENTATION_MODEL_SIZE,
        SEGMENTATION_MODEL_SHA256,
    )?;
    verify_file(
        &dir.join(EMBEDDING_MODEL_NAME),
        EMBEDDING_SOURCE_SIZE,
        EMBEDDING_SOURCE_SHA256,
    )?;
    let manifest_path = dir.join(MANIFEST_NAME);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read model manifest failed: {}", manifest_path.display()))?;
    let manifest: SpeakerDiarizationModelManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("decode model manifest failed: {}", manifest_path.display()))?;
    if manifest != SpeakerDiarizationModelManifest::default() {
        anyhow::bail!("speaker diarization model manifest does not match the catalog");
    }
    Ok(())
}

pub fn write_manifest(dir: &Path) -> Result<()> {
    let path = dir.join(MANIFEST_NAME);
    let bytes = serde_json::to_vec_pretty(&SpeakerDiarizationModelManifest::default())
        .context("encode speaker diarization model manifest failed")?;
    std::fs::write(&path, bytes)
        .with_context(|| format!("write model manifest failed: {}", path.display()))
}

pub(crate) async fn run_package_download(
    app: &AppHandle,
    id: &str,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    validate_package_id(id)?;
    if ensure_package_ready(id).is_ok() {
        emit_progress(app, id, "", 2, 2, TOTAL_DOWNLOAD_BYTES, DownloadPhase::Finished, None);
        return Ok(());
    }

    let staging = partial_package_dir(id)?;
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("create model staging dir failed: {}", staging.display()))?;
    emit_progress(
        app,
        id,
        "",
        0,
        2,
        staging_downloaded_bytes(&staging),
        DownloadPhase::Started,
        None,
    );

    let client = build_client()?;
    let archive_path = staging.join(SEGMENTATION_ARCHIVE_NAME);
    download_package_file(
        app,
        id,
        &client,
        SEGMENTATION_ARCHIVE_URL,
        &archive_path,
        SEGMENTATION_ARCHIVE_SIZE,
        SEGMENTATION_ARCHIVE_SHA256,
        0,
        0,
        Arc::clone(&cancel),
    )
    .await?;
    if cancel.load(Ordering::SeqCst) {
        emit_cancelled(app, id, &staging);
        return Ok(());
    }

    let embedding_path = staging.join(EMBEDDING_MODEL_NAME);
    download_package_file(
        app,
        id,
        &client,
        EMBEDDING_SOURCE_URL,
        &embedding_path,
        EMBEDDING_SOURCE_SIZE,
        EMBEDDING_SOURCE_SHA256,
        1,
        SEGMENTATION_ARCHIVE_SIZE,
        Arc::clone(&cancel),
    )
    .await?;
    if cancel.load(Ordering::SeqCst) {
        emit_cancelled(app, id, &staging);
        return Ok(());
    }

    let archive_for_extract = archive_path.clone();
    let segmentation_path = staging.join(SEGMENTATION_MODEL_NAME);
    let segmentation_for_extract = segmentation_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        extract_segmentation_model(&archive_for_extract, &segmentation_for_extract)
    })
    .await
    .map_err(|error| anyhow::anyhow!("speaker model extract join failed: {error}"))??;
    verify_file(
        &segmentation_path,
        SEGMENTATION_MODEL_SIZE,
        SEGMENTATION_MODEL_SHA256,
    )?;
    if cancel.load(Ordering::SeqCst) {
        emit_cancelled(app, id, &staging);
        return Ok(());
    }

    let _ = std::fs::remove_file(&archive_path);
    write_manifest(&staging)?;
    validate_package_dir(&staging)?;
    activate_staged_package(id, &staging)?;
    emit_progress(
        app,
        id,
        "",
        2,
        2,
        TOTAL_DOWNLOAD_BYTES,
        DownloadPhase::Finished,
        None,
    );
    Ok(())
}

async fn download_package_file(
    app: &AppHandle,
    id: &str,
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    expected_size: u64,
    expected_sha256: &str,
    file_index: usize,
    completed_before: u64,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    if verify_file(destination, expected_size, expected_sha256).is_ok() {
        emit_progress(
            app,
            id,
            destination.file_name().and_then(|name| name.to_str()).unwrap_or(""),
            file_index,
            2,
            completed_before + expected_size,
            DownloadPhase::Progress,
            None,
        );
        return Ok(());
    }
    if destination.exists() {
        std::fs::remove_file(destination).with_context(|| {
            format!("remove invalid model asset failed: {}", destination.display())
        })?;
    }
    let app_for_progress = app.clone();
    let id_for_progress = id.to_string();
    let file_for_progress = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();
    let progress = Arc::new(move |bytes_in_file| {
        emit_progress(
            &app_for_progress,
            &id_for_progress,
            &file_for_progress,
            file_index,
            2,
            completed_before.saturating_add(bytes_in_file),
            DownloadPhase::Progress,
            None,
        );
    });
    let result = download_one(
        client,
        url,
        destination,
        expected_size,
        Arc::clone(&cancel),
        progress,
    )
    .await;
    if cancel.load(Ordering::SeqCst) {
        return Ok(());
    }
    result?;
    verify_file(destination, expected_size, expected_sha256)
}

fn extract_segmentation_model(archive_path: &Path, destination: &Path) -> Result<()> {
    let file = File::open(archive_path)
        .with_context(|| format!("open segmentation archive failed: {}", archive_path.display()))?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let expected = Path::new(SEGMENTATION_ARCHIVE_ROOT).join(SEGMENTATION_ARCHIVE_MODEL_PATH);
    for entry in archive.entries().context("read segmentation archive entries failed")? {
        let mut entry = entry.context("read segmentation archive entry failed")?;
        let path = entry.path().context("read segmentation archive path failed")?;
        if path.as_ref() != expected {
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create segmentation destination failed: {}", parent.display())
            })?;
        }
        let mut output = File::create(destination).with_context(|| {
            format!("create segmentation model failed: {}", destination.display())
        })?;
        std::io::copy(&mut entry, &mut output).with_context(|| {
            format!("extract segmentation model failed: {}", destination.display())
        })?;
        return Ok(());
    }
    anyhow::bail!(
        "segmentation archive is missing {}",
        expected.display()
    )
}

fn activate_staged_package(id: &str, staging: &Path) -> Result<()> {
    validate_package_dir(staging)?;
    let destination = package_dir(id)?;
    if destination.exists() {
        std::fs::remove_dir_all(&destination).with_context(|| {
            format!("remove invalid speaker package failed: {}", destination.display())
        })?;
    }
    std::fs::rename(staging, &destination).with_context(|| {
        format!(
            "activate speaker package failed: {} -> {}",
            staging.display(),
            destination.display()
        )
    })?;
    validate_package_dir(&destination)
}

pub fn delete_package(id: &str) -> Result<()> {
    validate_package_id(id)?;
    for path in [package_dir(id)?, partial_package_dir(id)?] {
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("remove speaker model package failed: {}", path.display()))?;
        }
    }
    Ok(())
}

fn staging_downloaded_bytes(staging: &Path) -> u64 {
    [
        (staging.join(SEGMENTATION_ARCHIVE_NAME), SEGMENTATION_ARCHIVE_SIZE),
        (staging.join(EMBEDDING_MODEL_NAME), EMBEDDING_SOURCE_SIZE),
    ]
    .iter()
    .map(|(destination, size)| {
        std::fs::metadata(destination)
            .map(|metadata| metadata.len().min(*size))
            .unwrap_or_else(|_| partial_actual_size(&destination.with_extension("partial")).min(*size))
    })
    .sum()
}

fn emit_cancelled(app: &AppHandle, id: &str, staging: &Path) {
    emit_progress(
        app,
        id,
        "",
        0,
        2,
        staging_downloaded_bytes(staging),
        DownloadPhase::Cancelled,
        None,
    );
}

pub(crate) fn emit_failed(app: &AppHandle, id: &str, error: &anyhow::Error) {
    let downloaded = partial_package_dir(id)
        .map(|path| staging_downloaded_bytes(&path))
        .unwrap_or(0);
    emit_progress(
        app,
        id,
        "",
        0,
        2,
        downloaded,
        DownloadPhase::Failed,
        Some(format!("{error:#}")),
    );
}

fn emit_progress(
    app: &AppHandle,
    id: &str,
    file: &str,
    file_index: usize,
    file_count: usize,
    bytes_downloaded: u64,
    phase: DownloadPhase,
    error: Option<String>,
) {
    let payload = DownloadProgress {
        model_id: id.to_string(),
        file: file.to_string(),
        file_index,
        file_count,
        bytes_downloaded: bytes_downloaded.min(TOTAL_DOWNLOAD_BYTES),
        bytes_total: TOTAL_DOWNLOAD_BYTES,
        phase,
        error,
    };
    if let Err(error) = app.emit(DOWNLOAD_EVENT, payload) {
        log::warn!("[speaker-diarization] emit download progress failed: {error}");
    }
}

pub fn verify_file(path: &Path, expected_size: u64, expected_sha256: &str) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("stat model file failed: {}", path.display()))?;
    if !metadata.is_file() || metadata.len() != expected_size {
        anyhow::bail!(
            "model file size mismatch: {} actual={} expected={}",
            path.display(),
            metadata.len(),
            expected_size
        );
    }
    let actual = sha256_file(path)?;
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        anyhow::bail!(
            "model file SHA-256 mismatch: {} actual={} expected={}",
            path.display(),
            actual,
            expected_sha256
        );
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("open model file for SHA-256 failed: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read model file for SHA-256 failed: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn package_has_any_files(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

fn downloaded_bytes(dir: &Path, partial_dir: &Path) -> u64 {
    directory_size(dir)
        .max(staging_downloaded_bytes(partial_dir))
        .min(TOTAL_DOWNLOAD_BYTES)
}

fn directory_size(path: &Path) -> u64 {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        Ok(metadata) if metadata.is_dir() => std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| directory_size(&entry.path()))
                    .sum()
            })
            .unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_fixed_verified_upstream_assets() {
        assert_eq!(TOTAL_DOWNLOAD_BYTES, 46_552_205);
        assert_eq!(SEGMENTATION_ARCHIVE_SHA256.len(), 64);
        assert_eq!(SEGMENTATION_MODEL_SHA256.len(), 64);
        assert_eq!(EMBEDDING_SOURCE_SHA256.len(), 64);
        assert_eq!(CLUSTERING_THRESHOLD, 0.90);
        assert!(EMBEDDING_SOURCE_NAME.ends_with(".onnx"));
        assert_eq!(SUPPORTED_PLATFORMS, ["windows-x86_64"]);
    }

    #[test]
    fn unknown_package_is_rejected() {
        assert!(validate_package_id("speaker-model-from-frontend").is_err());
    }

    #[test]
    fn manifest_matches_runtime_files() {
        let manifest = SpeakerDiarizationModelManifest::default();
        assert_eq!(manifest.package_id, DEFAULT_PACKAGE_ID);
        assert_eq!(manifest.segmentation_file, SEGMENTATION_MODEL_NAME);
        assert_eq!(manifest.embedding_file, EMBEDDING_MODEL_NAME);
        assert_eq!(manifest.sample_rate, 16_000);
    }

    #[test]
    fn incomplete_staging_never_activates() {
        let root = std::env::temp_dir().join(format!(
            "openless-speaker-diarization-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let staging = root.join("package.partial");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join(SEGMENTATION_MODEL_NAME), b"invalid").unwrap();

        assert!(validate_package_dir(&staging).is_err());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staging_progress_counts_sparse_partial_chunks_instead_of_logical_size() {
        let root = std::env::temp_dir().join(format!(
            "openless-speaker-diarization-progress-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let destination = root.join(EMBEDDING_MODEL_NAME);
        let partial = destination.with_extension("partial");
        let file = File::create(&partial).unwrap();
        file.set_len(EMBEDDING_SOURCE_SIZE).unwrap();
        std::fs::write(partial.with_extension("partial.idx"), b"0\n").unwrap();

        assert_eq!(staging_downloaded_bytes(&root), 8 * 1024 * 1024);

        std::fs::remove_dir_all(root).unwrap();
    }
}
