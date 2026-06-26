//! Screenshot + text multimodal context analysis.

use std::io::Cursor;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::GenericImageView;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::persistence::{pending_analysis_result, ContextAnalysisStore, ContextCaptureStore};
use crate::types::{
    ContextAnalysisActionItem, ContextAnalysisActivityType, ContextAnalysisContextType,
    ContextAnalysisResult, ContextAnalysisStatus, ContextCaptureEntry, ContextCaptureHistoryType,
    DictationSession, RewriteHistoryEntry,
};

pub const PROMPT_VERSION: &str = "context-vision-analysis-v1";
const IMAGE_MAX_EDGE: u32 = 1600;
const JPEG_QUALITY: u8 = 80;
const REQUEST_TIMEOUT_SECS: u64 = 120;
const CONTEXT_WAIT_MS: u64 = 5_000;
const CONTEXT_POLL_MS: u64 = 200;

static ANALYSIS_REQUEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

const SYSTEM_PROMPT: &str = r#"你是 VoxHub 的“多模态上下文分析器”。

你的任务是根据用户提供的桌面截图、原始录入文本、处理后文本和窗口元数据，识别当前工作上下文，并输出可被程序稳定解析的 JSON。你的结果将用于后续语音润色、文本重写、历史归档、待办、日报、周报和月报生成。

你会收到以下输入：
- screenshot：当前窗口或全屏截图。
- rawInputText：本次语音输入原文，或本次文本重写前的原文。
- finalText：语音润色后的文本，语音场景可能存在。
- rewrittenText：文本重写后的文本，重写场景可能存在。
- historyType：voice 或 rewrite。
- windowTitle：系统读取到的窗口标题，可能为空或不准确。
- capturedApp：系统初步识别的应用名称，可能为空或不准确。
- capturedConversationWindow：系统初步识别的窗口、会话、频道、网页或文档名称，可能为空或不准确。

分析规则：
1. 必须同时参考截图和 rawInputText。finalText 或 rewrittenText 只能作为辅助理解，不得替代 rawInputText。
2. 优先从截图中识别具体上下文，例如聊天标题、联系人、群名、频道名、网页标题、文档名、编辑器项目、表单页面或任务页面。
3. 如果截图信息不足，再参考 windowTitle、capturedApp 和 capturedConversationWindow。
4. 不要把截图当作完整 OCR 任务。只提取对“当前上下文识别、摘要和后续归档”必要的少量文本线索。
5. 不要完整复述聊天记录、文档正文、账号、手机号、邮箱、地址、密钥、验证码、订单号等敏感内容。
6. 如果截图包含敏感信息，只做泛化描述，例如“界面中包含账号或身份信息”，不要输出原文。
7. 如果无法可靠判断，不要编造。请降低 confidence，并在 uncertaintyReason 中说明原因。
8. 如果截图和窗口标题冲突，优先相信截图中更具体、更可见的信息；但需要在 visualEvidence 中简短说明依据。
9. 输出语言使用简体中文。
10. 只输出一个合法 JSON 对象，不要输出 Markdown，不要添加解释性前后缀，不要输出推理过程。

摘要要求：
- briefSummary：用于后续提示词上下文。必须短、具体、低干扰，建议 30 到 80 个中文字符。说明“用户正在什么上下文中表达什么意图或确认什么事项”。
- fullSummary：用于日报、周报、月报和历史回顾。应比 briefSummary 更完整，建议 100 到 300 个中文字符。说明背景、参与对象、讨论主题、用户本次输入的含义、已形成的结论或后续价值。
- briefSummary 不要包含太多细节。
- fullSummary 可以包含必要细节，但不要复述大段截图文本或泄露敏感信息。

JSON 输出格式：
{
  "conversationName": "string | null",
  "briefSummary": "string",
  "fullSummary": "string",
  "detectedApp": "string | null",
  "detectedContextType": "chat | ai_chat | document | browser | editor | email | meeting | task | settings | unknown",
  "topic": "string | null",
  "userIntent": "string | null",
  "activityType": "decision | action_request | question | discussion | research | planning | implementation | review | note | unknown",
  "decision": "string | null",
  "actionItems": [
    {
      "text": "string",
      "owner": "string | null",
      "dueDate": "string | null",
      "confidence": 0.0
    }
  ],
  "relatedPeople": ["string"],
  "projectOrDomain": "string | null",
  "visualEvidence": ["string"],
  "sensitiveContentVisible": true,
  "confidence": 0.0,
  "uncertaintyReason": "string | null"
}
"#;

#[derive(Clone)]
pub struct ContextAnalysisTextInput {
    pub raw_input_text: String,
    pub final_text: Option<String>,
    pub rewritten_text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedVisionImage {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub width: u32,
    pub height: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelAnalysisJson {
    conversation_name: Option<String>,
    brief_summary: String,
    full_summary: String,
    detected_app: Option<String>,
    detected_context_type: Option<ContextAnalysisContextType>,
    topic: Option<String>,
    user_intent: Option<String>,
    activity_type: Option<ContextAnalysisActivityType>,
    decision: Option<String>,
    #[serde(default)]
    action_items: Vec<ContextAnalysisActionItem>,
    #[serde(default)]
    related_people: Vec<String>,
    project_or_domain: Option<String>,
    #[serde(default)]
    visual_evidence: Vec<String>,
    sensitive_content_visible: Option<bool>,
    confidence: Option<f32>,
    uncertainty_reason: Option<String>,
}

pub fn prepare_image_for_vision(path: &Path) -> Result<PreparedVisionImage> {
    let image = image::open(path).with_context(|| format!("decode image failed: {}", path.display()))?;
    let (width, height) = image.dimensions();
    let longest = width.max(height);
    let output = if longest > IMAGE_MAX_EDGE {
        let ratio = IMAGE_MAX_EDGE as f32 / longest as f32;
        let next_width = ((width as f32 * ratio).round() as u32).max(1);
        let next_height = ((height as f32 * ratio).round() as u32).max(1);
        image.resize(next_width, next_height, FilterType::Lanczos3)
    } else {
        image
    };
    let (out_width, out_height) = output.dimensions();
    let mut bytes = Vec::new();
    {
        let mut cursor = Cursor::new(&mut bytes);
        let mut encoder = JpegEncoder::new_with_quality(&mut cursor, JPEG_QUALITY);
        encoder
            .encode_image(&output)
            .context("encode jpeg failed")?;
    }
    Ok(PreparedVisionImage {
        bytes,
        mime_type: "image/jpeg",
        width: out_width,
        height: out_height,
    })
}

pub fn spawn_analysis_for_voice(
    context_store: ContextCaptureStore,
    analysis_store: ContextAnalysisStore,
    entry: DictationSession,
) {
    spawn_analysis_task(
        context_store,
        analysis_store,
        ContextCaptureHistoryType::Voice,
        entry.id,
        ContextAnalysisTextInput {
            raw_input_text: entry.raw_transcript,
            final_text: Some(entry.final_text),
            rewritten_text: None,
        },
    );
}

pub fn spawn_analysis_for_rewrite(
    context_store: ContextCaptureStore,
    analysis_store: ContextAnalysisStore,
    entry: RewriteHistoryEntry,
) {
    spawn_analysis_task(
        context_store,
        analysis_store,
        ContextCaptureHistoryType::Rewrite,
        entry.id,
        ContextAnalysisTextInput {
            raw_input_text: entry.source_text,
            final_text: None,
            rewritten_text: Some(entry.rewritten_text),
        },
    );
}

pub fn spawn_reanalysis(
    context_store: ContextCaptureStore,
    analysis_store: ContextAnalysisStore,
    history_type: ContextCaptureHistoryType,
    history_id: String,
    input: ContextAnalysisTextInput,
) {
    spawn_analysis_task(context_store, analysis_store, history_type, history_id, input);
}

fn spawn_analysis_task(
    context_store: ContextCaptureStore,
    analysis_store: ContextAnalysisStore,
    history_type: ContextCaptureHistoryType,
    history_id: String,
    input: ContextAnalysisTextInput,
) {
    std::thread::Builder::new()
        .name("openless-context-vision-analysis".into())
        .spawn(move || {
            tauri::async_runtime::block_on(async move {
                if let Err(error) =
                    run_analysis_task(context_store, analysis_store, history_type, history_id, input).await
                {
                    log::warn!("[context-analysis] task failed: {error:#}");
                }
            });
        })
        .ok();
}

async fn run_analysis_task(
    context_store: ContextCaptureStore,
    analysis_store: ContextAnalysisStore,
    history_type: ContextCaptureHistoryType,
    history_id: String,
    input: ContextAnalysisTextInput,
) -> Result<()> {
    let prefs = crate::persistence::PreferencesStore::new()
        .unwrap_or_else(|_| crate::persistence::PreferencesStore::new_fallback())
        .get();
    if !prefs.context_vision_analysis_enabled {
        return Ok(());
    }
    if !prefs.context_vision_analysis_consent_accepted {
        return Ok(());
    }

    let context = wait_for_context(&context_store, history_type, &history_id)?;
    let Some(context) = context else {
        let result = skipped_result(history_type, &history_id, "skipped:screenshotUnavailable");
        upsert_if_context_current(
            &context_store,
            &analysis_store,
            result,
            history_type,
            &history_id,
            None,
        )?;
        return Ok(());
    };
    let pending_result = pending_analysis_result(&context);
    upsert_if_context_current(
        &context_store,
        &analysis_store,
        pending_result.clone(),
        history_type,
        &history_id,
        Some(&context.id),
    )?;

    let Some(screenshot_ref) = context.screenshot_ref.as_deref() else {
        upsert_if_context_current(
            &context_store,
            &analysis_store,
            with_error(
                pending_result.clone(),
                ContextAnalysisStatus::Skipped,
                "skipped:screenshotUnavailable",
            ),
            history_type,
            &history_id,
            Some(&context.id),
        )?;
        return Ok(());
    };

    let active_provider = crate::persistence::CredentialsVault::get_active_llm();
    if active_provider == crate::polish::CODEX_OAUTH_PROVIDER_ID || active_provider == "gemini" {
        upsert_if_context_current(
            &context_store,
            &analysis_store,
            with_provider_error(
                pending_result.clone(),
                ContextAnalysisStatus::Skipped,
                "skipped:unsupportedProvider",
                Some(active_provider),
                None,
            ),
            history_type,
            &history_id,
            Some(&context.id),
        )?;
        return Ok(());
    }

    let api_key = crate::persistence::CredentialsVault::get(
        crate::persistence::CredentialAccount::ArkApiKey,
    )?
    .unwrap_or_default();
    let endpoint = crate::persistence::CredentialsVault::get(
        crate::persistence::CredentialAccount::ArkEndpoint,
    )?
    .unwrap_or_default();
    let model = crate::persistence::CredentialsVault::get(
        crate::persistence::CredentialAccount::ArkContextVisionModelId,
    )?
    .unwrap_or_default();
    if endpoint.trim().is_empty() {
        upsert_if_context_current(
            &context_store,
            &analysis_store,
            with_provider_error(
                pending_result.clone(),
                ContextAnalysisStatus::Skipped,
                "skipped:providerNotConfigured",
                Some(active_provider),
                Some(model),
            ),
            history_type,
            &history_id,
            Some(&context.id),
        )?;
        return Ok(());
    }
    if model.trim().is_empty() {
        upsert_if_context_current(
            &context_store,
            &analysis_store,
            with_provider_error(
                pending_result.clone(),
                ContextAnalysisStatus::Skipped,
                "skipped:modelNotConfigured",
                Some(active_provider),
                None,
            ),
            history_type,
            &history_id,
            Some(&context.id),
        )?;
        return Ok(());
    }
    if api_key.trim().is_empty() {
        upsert_if_context_current(
            &context_store,
            &analysis_store,
            with_provider_error(
                pending_result.clone(),
                ContextAnalysisStatus::Skipped,
                "skipped:providerNotConfigured",
                Some(active_provider),
                Some(model),
            ),
            history_type,
            &history_id,
            Some(&context.id),
        )?;
        return Ok(());
    }

    let screenshot_path = context_store.screenshot_path_for_ref(screenshot_ref)?;
    let prepared = match prepare_image_for_vision(&screenshot_path) {
        Ok(image) => image,
        Err(error) => {
            log::warn!("[context-analysis] image prepare failed: {error:#}");
            upsert_if_context_current(
                &context_store,
                &analysis_store,
                with_provider_error(
                    pending_result.clone(),
                    ContextAnalysisStatus::Failed,
                    "failed:imagePrepareFailed",
                    Some(active_provider),
                    Some(model),
                ),
                history_type,
                &history_id,
                Some(&context.id),
            )?;
            return Ok(());
        }
    };

    let response = match {
        let _request_guard = analysis_request_lock().lock().await;
        request_context_analysis(
            &active_provider,
            &endpoint,
            &api_key,
            &model,
            &context,
            &input,
            &prepared,
        )
        .await
    }
    {
        Ok(text) => text,
        Err(AnalysisRequestError::ModelNotVisionCapable) => {
            upsert_if_context_current(
                &context_store,
                &analysis_store,
                with_image_error(
                    pending_result.clone(),
                    ContextAnalysisStatus::Failed,
                    "failed:modelNotVisionCapable",
                    &active_provider,
                    &model,
                    &prepared,
                ),
                history_type,
                &history_id,
                Some(&context.id),
            )?;
            return Ok(());
        }
        Err(AnalysisRequestError::Timeout(error)) => {
            log::warn!("[context-analysis] vision request timed out: {error}");
            upsert_if_context_current(
                &context_store,
                &analysis_store,
                with_image_error(
                    pending_result.clone(),
                    ContextAnalysisStatus::Failed,
                    "failed:visionRequestTimeout",
                    &active_provider,
                    &model,
                    &prepared,
                ),
                history_type,
                &history_id,
                Some(&context.id),
            )?;
            return Ok(());
        }
        Err(error) => {
            log::warn!("[context-analysis] vision request failed: {error}");
            upsert_if_context_current(
                &context_store,
                &analysis_store,
                with_image_error(
                    pending_result.clone(),
                    ContextAnalysisStatus::Failed,
                    "failed:visionRequestFailed",
                    &active_provider,
                    &model,
                    &prepared,
                ),
                history_type,
                &history_id,
                Some(&context.id),
            )?;
            return Ok(());
        }
    };

    let parsed = match parse_model_output(&response) {
        Ok(parsed) => parsed,
        Err(error) => {
            log::warn!("[context-analysis] invalid model output: {error:#}");
            upsert_if_context_current(
                &context_store,
                &analysis_store,
                with_image_error(
                    pending_result.clone(),
                    ContextAnalysisStatus::Failed,
                    "failed:invalidModelOutput",
                    &active_provider,
                    &model,
                    &prepared,
                ),
                history_type,
                &history_id,
                Some(&context.id),
            )?;
            return Ok(());
        }
    };

    let result = success_result(
        pending_result,
        parsed,
        &active_provider,
        &model,
        &prepared,
    );
    upsert_if_context_current(
        &context_store,
        &analysis_store,
        result,
        history_type,
        &history_id,
        Some(&context.id),
    )?;
    Ok(())
}

fn analysis_request_lock() -> &'static tokio::sync::Mutex<()> {
    ANALYSIS_REQUEST_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn upsert_if_context_current(
    context_store: &ContextCaptureStore,
    analysis_store: &ContextAnalysisStore,
    result: ContextAnalysisResult,
    history_type: ContextCaptureHistoryType,
    history_id: &str,
    expected_context_id: Option<&str>,
) -> Result<()> {
    match expected_context_id {
        Some(context_id) => {
            let current = context_store.latest_for_history(history_type, history_id)?;
            let still_current = current
                .as_ref()
                .map(|entry| entry.id.as_str() == context_id)
                .unwrap_or(false);
            if !still_current {
                log::info!(
                    "[context-analysis] skip stale result for history_type={history_type:?} history_id={history_id}"
                );
                return Ok(());
            }
        }
        None => {
            if context_store
                .latest_for_history(history_type, history_id)?
                .is_some()
            {
                return Ok(());
            }
        }
    }
    if !linked_history_exists(history_type, history_id)? {
        log::info!(
            "[context-analysis] skip orphan result for history_type={history_type:?} history_id={history_id}"
        );
        return Ok(());
    }
    analysis_store.upsert(result)
}

fn linked_history_exists(
    history_type: ContextCaptureHistoryType,
    history_id: &str,
) -> Result<bool> {
    match history_type {
        ContextCaptureHistoryType::Voice => {
            let store = crate::persistence::HistoryStore::new()
                .unwrap_or_else(|_| crate::persistence::HistoryStore::new_fallback());
            Ok(store.list()?.iter().any(|entry| entry.id == history_id))
        }
        ContextCaptureHistoryType::Rewrite => {
            let store = crate::persistence::RewriteHistoryStore::new()
                .unwrap_or_else(|_| crate::persistence::RewriteHistoryStore::new_fallback());
            Ok(store.list()?.iter().any(|entry| entry.id == history_id))
        }
    }
}

fn wait_for_context(
    store: &ContextCaptureStore,
    history_type: ContextCaptureHistoryType,
    history_id: &str,
) -> Result<Option<ContextCaptureEntry>> {
    let deadline = std::time::Instant::now() + Duration::from_millis(CONTEXT_WAIT_MS);
    loop {
        if let Some(context) = store.latest_for_history(history_type, history_id)? {
            return Ok(Some(context));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(CONTEXT_POLL_MS));
    }
}

#[derive(Debug)]
enum AnalysisRequestError {
    Network(String),
    Timeout(String),
    ModelNotVisionCapable,
    InvalidResponse(String),
}

impl std::fmt::Display for AnalysisRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(value) => write!(f, "{value}"),
            Self::Timeout(value) => write!(f, "{value}"),
            Self::ModelNotVisionCapable => write!(f, "model not vision capable"),
            Self::InvalidResponse(value) => write!(f, "{value}"),
        }
    }
}

fn request_error(error: reqwest::Error) -> AnalysisRequestError {
    if error.is_timeout() {
        AnalysisRequestError::Timeout(error.to_string())
    } else {
        AnalysisRequestError::Network(error.to_string())
    }
}

async fn request_context_analysis(
    provider_id: &str,
    endpoint: &str,
    api_key: &str,
    model: &str,
    context: &ContextCaptureEntry,
    input: &ContextAnalysisTextInput,
    image: &PreparedVisionImage,
) -> std::result::Result<String, AnalysisRequestError> {
    let url = chat_completions_url(endpoint);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|error| AnalysisRequestError::Network(error.to_string()))?;
    let image_base64 = base64::engine::general_purpose::STANDARD.encode(&image.bytes);
    let metadata = json!({
        "historyType": match context.linked_history_type {
            ContextCaptureHistoryType::Voice => "voice",
            ContextCaptureHistoryType::Rewrite => "rewrite",
        },
        "rawInputText": input.raw_input_text,
        "finalText": input.final_text,
        "rewrittenText": input.rewritten_text,
        "windowTitle": context.window_title,
        "capturedApp": context.context_app,
        "capturedConversationWindow": context.conversation_window,
    });
    let body = json!({
        "model": model,
        "stream": false,
        "temperature": 0.2,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": serde_json::to_string_pretty(&metadata).unwrap_or_else(|_| "{}".to_string())
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", image.mime_type, image_base64)
                        }
                    }
                ]
            }
        ]
    });

    log::info!(
        "[context-analysis] POST {} provider={} model={} image={}x{} bytes={}",
        redacted_url_for_log(&url),
        provider_id,
        model,
        image.width,
        image.height,
        image.bytes.len()
    );

    let mut request = client.post(url).header("Content-Type", "application/json");
    if !api_key.trim().is_empty() {
        request = request.header("Authorization", format!("Bearer {api_key}"));
    }
    let response = request
        .json(&body)
        .send()
        .await
        .map_err(request_error)?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(request_error)?;
    if !status.is_success() {
        if status == StatusCode::BAD_REQUEST || status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
            return Err(AnalysisRequestError::ModelNotVisionCapable);
        }
        return Err(AnalysisRequestError::InvalidResponse(format!(
            "http {}",
            status.as_u16()
        )));
    }
    extract_assistant_content(&text)
}

fn chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim();
    if trimmed.ends_with("/chat/completions") {
        return trimmed.to_string();
    }
    let without_trailing = trimmed.trim_end_matches('/');
    format!("{without_trailing}/chat/completions")
}

fn redacted_url_for_log(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(parsed) => {
            let port = parsed
                .port()
                .map(|value| format!(":{value}"))
                .unwrap_or_default();
            format!(
                "{}://{}{}{}",
                parsed.scheme(),
                parsed.host_str().unwrap_or("unknown-host"),
                port,
                parsed.path()
            )
        }
        Err(_) => "<invalid-url>".to_string(),
    }
}

fn extract_assistant_content(body_text: &str) -> std::result::Result<String, AnalysisRequestError> {
    let value: Value = serde_json::from_str(body_text)
        .map_err(|error| AnalysisRequestError::InvalidResponse(error.to_string()))?;
    value["choices"]
        .as_array()
        .and_then(|choices| choices.first())
        .and_then(|choice| choice["message"]["content"].as_str())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| AnalysisRequestError::InvalidResponse("missing assistant content".into()))
}

fn parse_model_output(text: &str) -> Result<ModelAnalysisJson> {
    let trimmed = text.trim();
    let json_text = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .or_else(|| {
            trimmed
                .strip_prefix("```")
                .and_then(|value| value.strip_suffix("```"))
        })
        .unwrap_or(trimmed)
        .trim();
    let parsed: ModelAnalysisJson =
        serde_json::from_str(json_text).context("parse context analysis json failed")?;
    if parsed.brief_summary.trim().is_empty() || parsed.full_summary.trim().is_empty() {
        anyhow::bail!("summary fields are empty");
    }
    Ok(parsed)
}

fn success_result(
    mut result: ContextAnalysisResult,
    parsed: ModelAnalysisJson,
    provider_id: &str,
    model: &str,
    image: &PreparedVisionImage,
) -> ContextAnalysisResult {
    result.status = ContextAnalysisStatus::Success;
    result.analyzed_at = Some(chrono::Utc::now().to_rfc3339());
    result.provider_id = Some(provider_id.to_string());
    result.model = Some(model.to_string());
    result.image_mime_type = Some(image.mime_type.to_string());
    result.image_width = Some(image.width);
    result.image_height = Some(image.height);
    result.image_bytes = Some(image.bytes.len() as u64);
    result.conversation_name = normalize_optional(parsed.conversation_name);
    result.brief_summary = Some(parsed.brief_summary.trim().to_string());
    result.full_summary = Some(parsed.full_summary.trim().to_string());
    result.detected_app = normalize_optional(parsed.detected_app);
    result.detected_context_type = parsed.detected_context_type.unwrap_or_default();
    result.topic = normalize_optional(parsed.topic);
    result.user_intent = normalize_optional(parsed.user_intent);
    result.activity_type = parsed.activity_type.unwrap_or_default();
    result.decision = normalize_optional(parsed.decision);
    result.action_items = parsed.action_items;
    result.related_people = parsed
        .related_people
        .into_iter()
        .filter_map(|value| normalize_optional(Some(value)))
        .collect();
    result.project_or_domain = normalize_optional(parsed.project_or_domain);
    result.visual_evidence = parsed
        .visual_evidence
        .into_iter()
        .filter_map(|value| normalize_optional(Some(value)))
        .take(3)
        .collect();
    result.sensitive_content_visible = parsed.sensitive_content_visible.unwrap_or(false);
    result.confidence = parsed.confidence.unwrap_or(0.0).clamp(0.0, 1.0);
    result.uncertainty_reason = normalize_optional(parsed.uncertainty_reason);
    result.error_code = None;
    result
}

fn skipped_result(
    history_type: ContextCaptureHistoryType,
    history_id: &str,
    error_code: &str,
) -> ContextAnalysisResult {
    let now = chrono::Utc::now().to_rfc3339();
    ContextAnalysisResult {
        id: uuid::Uuid::new_v4().to_string(),
        context_capture_id: String::new(),
        linked_history_type: history_type,
        linked_history_id: history_id.to_string(),
        status: ContextAnalysisStatus::Skipped,
        created_at: now.clone(),
        analyzed_at: Some(now),
        provider_id: None,
        model: None,
        prompt_version: PROMPT_VERSION.to_string(),
        schema_version: 1,
        input_mode: "screenshot_text".to_string(),
        image_mime_type: None,
        image_width: None,
        image_height: None,
        image_bytes: None,
        conversation_name: None,
        brief_summary: None,
        full_summary: None,
        detected_app: None,
        detected_context_type: Default::default(),
        topic: None,
        user_intent: None,
        activity_type: Default::default(),
        decision: None,
        action_items: Vec::new(),
        related_people: Vec::new(),
        project_or_domain: None,
        visual_evidence: Vec::new(),
        sensitive_content_visible: false,
        confidence: 0.0,
        uncertainty_reason: None,
        error_code: Some(error_code.to_string()),
    }
}

fn with_error(
    mut result: ContextAnalysisResult,
    status: ContextAnalysisStatus,
    error_code: &str,
) -> ContextAnalysisResult {
    result.status = status;
    result.analyzed_at = Some(chrono::Utc::now().to_rfc3339());
    result.error_code = Some(error_code.to_string());
    result
}

fn with_provider_error(
    mut result: ContextAnalysisResult,
    status: ContextAnalysisStatus,
    error_code: &str,
    provider_id: Option<String>,
    model: Option<String>,
) -> ContextAnalysisResult {
    result = with_error(result, status, error_code);
    result.provider_id = provider_id;
    result.model = model;
    result
}

fn with_image_error(
    mut result: ContextAnalysisResult,
    status: ContextAnalysisStatus,
    error_code: &str,
    provider_id: &str,
    model: &str,
    image: &PreparedVisionImage,
) -> ContextAnalysisResult {
    result = with_provider_error(
        result,
        status,
        error_code,
        Some(provider_id.to_string()),
        Some(model.to_string()),
    );
    result.image_mime_type = Some(image.mime_type.to_string());
    result.image_width = Some(image.width);
    result.image_height = Some(image.height);
    result.image_bytes = Some(image.bytes.len() as u64);
    result
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn temp_test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openless-context-vision-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn prepare_image_converts_bmp_to_jpeg_and_limits_longest_edge() {
        let dir = temp_test_dir("large");
        let path = dir.join("large.bmp");
        let image = ImageBuffer::from_fn(1936, 1048, |_, _| Rgba([180u8, 200, 220, 255]));
        image.save(&path).unwrap();

        let prepared = prepare_image_for_vision(&path).unwrap();

        assert_eq!(prepared.mime_type, "image/jpeg");
        assert_eq!(prepared.width.max(prepared.height), 1600);
        assert!(prepared.bytes.starts_with(&[0xFF, 0xD8]));
        assert!(prepared.bytes.len() < std::fs::metadata(&path).unwrap().len() as usize);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prepare_image_does_not_upscale_small_images() {
        let dir = temp_test_dir("small");
        let path = dir.join("small.bmp");
        let image = ImageBuffer::from_fn(320, 200, |_, _| Rgba([120u8, 120, 120, 255]));
        image.save(&path).unwrap();

        let prepared = prepare_image_for_vision(&path).unwrap();

        assert_eq!(prepared.width, 320);
        assert_eq!(prepared.height, 200);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn request_timeout_allows_slow_vision_models() {
        assert!(REQUEST_TIMEOUT_SECS >= 120);
    }

    #[test]
    fn parse_model_output_accepts_valid_json() {
        let parsed = parse_model_output(
            r#"{
              "conversationName": "彭锐南",
              "briefSummary": "用户确认流程可行。",
              "fullSummary": "用户在企业微信中确认小程序手机号绑定流程可行。",
              "detectedApp": "企业微信",
              "detectedContextType": "chat",
              "topic": "小程序手机号绑定流程",
              "userIntent": "确认流程",
              "activityType": "decision",
              "decision": "流程可行",
              "actionItems": [],
              "relatedPeople": ["彭锐南"],
              "projectOrDomain": "小程序登录",
              "visualEvidence": ["聊天窗口显示流程讨论"],
              "sensitiveContentVisible": false,
              "confidence": 0.9,
              "uncertaintyReason": null
            }"#,
        )
        .unwrap();
        assert_eq!(parsed.conversation_name.as_deref(), Some("彭锐南"));
    }
}
