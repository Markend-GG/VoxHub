use super::*;

use crate::asr::local::speaker_diarization::{
    self, SpeakerDiarizationModelDescriptor, DEFAULT_PACKAGE_ID,
};
use crate::persistence::MeetingStore;
use crate::types::{MeetingDiarizationMode, MeetingPostProcessingStatus, MeetingStatus};

pub(crate) fn active_sherpa_model_from_prefs(prefs: &UserPreferences) -> String {
    if sherpa_model_alias_is_known(&prefs.sherpa_onnx_model) {
        prefs.sherpa_onnx_model.clone()
    } else {
        SHERPA_DEFAULT_MODEL_ALIAS.to_string()
    }
}

pub(crate) fn validate_sherpa_model_alias(model_alias: &str) -> Result<(), String> {
    if sherpa_model_alias_is_known(model_alias) {
        Ok(())
    } else {
        Err(format!("unknown sherpa-onnx model alias: {model_alias}"))
    }
}

pub(crate) fn normalize_sherpa_language_hint(language_hint: &str) -> Result<String, String> {
    let normalized = language_hint.trim().to_lowercase();
    if normalized.is_empty()
        || normalized
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '-')
    {
        Ok(normalized)
    } else {
        Err("language hint must be empty or BCP-47 lowercase code".to_string())
    }
}

#[tauri::command]
pub async fn sherpa_onnx_asr_status(
    coord: CoordinatorState<'_>,
    runtime: State<'_, Arc<SherpaOnnxRuntime>>,
) -> Result<SherpaRuntimeStatus, String> {
    let prefs = coord.prefs().get();
    let active_model = active_sherpa_model_from_prefs(&prefs);
    Ok(runtime.status_snapshot(&active_model).await)
}

#[tauri::command]
pub async fn sherpa_onnx_asr_catalog(
    runtime: State<'_, Arc<SherpaOnnxRuntime>>,
) -> Result<Vec<SherpaCatalogModel>, String> {
    runtime
        .catalog_snapshot()
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn sherpa_onnx_asr_fetch_remote_info(
    model_alias: String,
    mirror: Option<String>,
) -> Result<SherpaRemoteInfo, String> {
    validate_sherpa_model_alias(&model_alias)?;
    let mirror = mirror.as_deref().map(Mirror::from_str).unwrap_or_default();
    fetch_sherpa_remote_info(&model_alias, mirror)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn sherpa_onnx_asr_download_model(
    app: AppHandle,
    manager: State<'_, Arc<SherpaDownloadManager>>,
    model_alias: String,
    mirror: Option<String>,
) -> Result<(), String> {
    validate_sherpa_model_alias(&model_alias)?;
    let mirror = mirror.as_deref().map(Mirror::from_str).unwrap_or_default();
    manager.start(app, model_alias, mirror);
    Ok(())
}

#[tauri::command]
pub fn sherpa_onnx_asr_cancel_download(
    manager: State<'_, Arc<SherpaDownloadManager>>,
    model_alias: String,
) -> Result<(), String> {
    validate_sherpa_model_alias(&model_alias)?;
    manager.cancel(&model_alias);
    Ok(())
}

#[tauri::command]
pub fn sherpa_onnx_asr_set_model(
    coord: CoordinatorState<'_>,
    model_alias: String,
) -> Result<(), String> {
    validate_sherpa_model_alias(&model_alias)?;
    let mut prefs = coord.prefs().get();
    if prefs.sherpa_onnx_model == model_alias {
        return Ok(());
    }
    prefs.sherpa_onnx_model = model_alias;
    coord.prefs().set(prefs).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn sherpa_onnx_asr_set_language_hint(
    coord: CoordinatorState<'_>,
    language_hint: String,
) -> Result<(), String> {
    let normalized = normalize_sherpa_language_hint(&language_hint)?;
    let mut prefs = coord.prefs().get();
    if prefs.sherpa_onnx_language_hint == normalized {
        return Ok(());
    }
    prefs.sherpa_onnx_language_hint = normalized;
    coord.prefs().set(prefs).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sherpa_onnx_asr_prepare(
    app: AppHandle,
    runtime: State<'_, Arc<SherpaOnnxRuntime>>,
    model_alias: String,
) -> Result<String, String> {
    validate_sherpa_model_alias(&model_alias)?;
    let progress_app = app.clone();
    let result = runtime
        .ensure_loaded_with_progress(&model_alias, move |payload| {
            emit_sherpa_prepare_progress(&progress_app, payload);
        })
        .await;
    match result {
        Ok(loaded) => Ok(loaded),
        Err(error) => {
            let message = format!("{error:#}");
            emit_sherpa_prepare_progress(
                &app,
                SherpaPrepareProgressPayload::failed(
                    model_alias,
                    "sherpa-onnx prepare failed",
                    message.clone(),
                ),
            );
            Err(message)
        }
    }
}

#[tauri::command]
pub fn sherpa_onnx_asr_cancel_prepare(
    runtime: State<'_, Arc<SherpaOnnxRuntime>>,
) -> Result<(), String> {
    runtime.request_cancel_prepare();
    Ok(())
}

#[tauri::command]
pub async fn sherpa_onnx_asr_release(
    runtime: State<'_, Arc<SherpaOnnxRuntime>>,
) -> Result<(), String> {
    runtime.release_now().await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn sherpa_onnx_asr_model_dir(model_alias: String) -> Result<String, String> {
    validate_sherpa_model_alias(&model_alias)?;
    SherpaOnnxRuntime::model_dir_for_alias(&model_alias)
        .map(|path| path.display().to_string())
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn sherpa_onnx_asr_delete_model(
    runtime: State<'_, Arc<SherpaOnnxRuntime>>,
    model_alias: String,
) -> Result<(), String> {
    validate_sherpa_model_alias(&model_alias)?;
    runtime
        .delete_model(&model_alias)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn sherpa_onnx_asr_reveal_model_dir(model_alias: String) -> Result<(), String> {
    validate_sherpa_model_alias(&model_alias)?;
    let dir = SherpaOnnxRuntime::model_dir_for_alias(&model_alias).map_err(|e| format!("{e:#}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {} failed: {e}", dir.display()))?;
    open_path_in_file_manager(&dir)
}

#[tauri::command]
pub fn list_speaker_diarization_models(
    manager: State<'_, Arc<SherpaDownloadManager>>,
) -> Result<Vec<SpeakerDiarizationModelDescriptor>, String> {
    let descriptor = speaker_diarization::package_descriptor(
        DEFAULT_PACKAGE_ID,
        manager.speaker_diarization_is_active(DEFAULT_PACKAGE_ID),
    )
    .map_err(|error| format!("{error:#}"))?;
    Ok(vec![descriptor])
}

#[tauri::command]
pub fn download_speaker_diarization_model(
    app: AppHandle,
    manager: State<'_, Arc<SherpaDownloadManager>>,
    model_id: String,
) -> Result<(), String> {
    speaker_diarization::validate_package_id(&model_id).map_err(|error| format!("{error:#}"))?;
    manager.start_speaker_diarization(app, model_id);
    Ok(())
}

#[tauri::command]
pub fn cancel_speaker_diarization_model_download(
    manager: State<'_, Arc<SherpaDownloadManager>>,
    model_id: String,
) -> Result<(), String> {
    speaker_diarization::validate_package_id(&model_id).map_err(|error| format!("{error:#}"))?;
    manager.cancel_speaker_diarization(&model_id);
    Ok(())
}

#[tauri::command]
pub fn delete_speaker_diarization_model(
    coord: CoordinatorState<'_>,
    manager: State<'_, Arc<SherpaDownloadManager>>,
    model_id: String,
) -> Result<(), String> {
    speaker_diarization::validate_package_id(&model_id).map_err(|error| format!("{error:#}"))?;
    if manager.speaker_diarization_is_active(&model_id) {
        return Err("speakerDiarizationModelDownloadActive: 请先取消模型下载".to_string());
    }
    if crate::asr::local::speaker_diarization_runtime::model_is_active(&model_id) {
        return Err("speakerDiarizationModelInUse: 模型正在执行本地说话人分析".to_string());
    }
    ensure_speaker_model_not_in_use(&model_id)?;
    speaker_diarization::delete_package(&model_id).map_err(|error| format!("{error:#}"))?;

    let mut prefs = coord.prefs().get();
    if prefs.post_meeting_asr.diarization.local_model_id.as_deref() == Some(model_id.as_str()) {
        prefs.post_meeting_asr.diarization.local_model_id = None;
        coord
            .prefs()
            .set(prefs)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn ensure_speaker_model_not_in_use(model_id: &str) -> Result<(), String> {
    let records = MeetingStore::new()
        .map_err(|error| error.to_string())?
        .list()
        .map_err(|error| error.to_string())?;
    let in_use = records.iter().any(|record| {
        let configured = record
            .post_processing_config
            .as_ref()
            .filter(|config| config.diarization_mode == MeetingDiarizationMode::Local)
            .and_then(|config| config.local_diarization_model_id.as_deref())
            == Some(model_id);
        if !configured {
            return false;
        }
        matches!(
            record.status,
            MeetingStatus::Recording
                | MeetingStatus::Paused
                | MeetingStatus::TranscribingInterrupted
        ) || record.post_processing.as_ref().is_some_and(|state| {
            matches!(
                state.status,
                MeetingPostProcessingStatus::Pending
                    | MeetingPostProcessingStatus::PreparingAudio
                    | MeetingPostProcessingStatus::Uploading
                    | MeetingPostProcessingStatus::Running
                    | MeetingPostProcessingStatus::LocalAnalyzing
                    | MeetingPostProcessingStatus::Applying
            )
        })
    });
    if in_use {
        Err("speakerDiarizationModelInUse: 模型正在被会议使用，暂时不能删除".to_string())
    } else {
        Ok(())
    }
}

fn emit_sherpa_prepare_progress(app: &AppHandle, payload: SherpaPrepareProgressPayload) {
    if let Err(error) = app.emit("sherpa-onnx-asr-prepare-progress", payload) {
        log::warn!("[sherpa-asr] emit prepare progress failed: {error}");
    }
}
