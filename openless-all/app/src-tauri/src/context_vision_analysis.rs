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
use sha2::{Digest, Sha256};

use crate::persistence::{
    pending_analysis_result, ContextAnalysisStore, ContextCaptureStore, ScreenshotRecordStore,
};
use crate::types::{
    ContextAnalysisActionItem, ContextAnalysisActivityType, ContextAnalysisContextType,
    ContextAnalysisEvidenceLevel, ContextAnalysisResult, ContextAnalysisStatus,
    ContextAnalysisWorkStatus, ContextCaptureEntry, ContextCaptureHistoryType, DictationSession,
    RewriteHistoryEntry,
    ScreenshotRecord, ScreenshotRecordStatus,
};

pub const PROMPT_VERSION: &str = "context-vision-analysis-v2";
const IMAGE_MAX_EDGE: u32 = 1600;
const JPEG_QUALITY: u8 = 80;
const REQUEST_TIMEOUT_SECS: u64 = 120;
const CONTEXT_WAIT_MS: u64 = 5_000;
const CONTEXT_POLL_MS: u64 = 200;

static ANALYSIS_REQUEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

const BASE_SYSTEM_PROMPT: &str = r#"你是 VoxHub / OpenLess 的多模态工作上下文分析器。

你的任务是根据桌面截图、用户原始输入、处理后文本和窗口元数据，识别用户此刻正在进行的工作活动，并输出一个可被程序稳定解析的 JSON 对象。结果会用于历史归档、后续提示词上下文、待办提取、日报、周报和月报生成。

一、输入说明

你可能收到以下输入：
- screenshot：当前窗口或全屏截图，可能包含聊天、文档、网页、编辑器、任务系统或 AI 工具界面。
- rawInputText：语音输入原文、文本重写前原文；截图记录场景可能为空。
- finalText：语音润色后的文本。
- rewrittenText：文本重写后的文本。
- historyType：voice、rewrite 或 screenshotRecord。
- windowTitle：系统读取到的窗口标题，可能为空或不准确。
- capturedApp：系统初步识别的应用名称，可能为空或不准确。
- capturedConversationWindow：系统初步识别的窗口、会话、频道、网页或文档名称，可能为空或不准确。

二、来源语义

你必须先判断来源类型，再生成摘要：

1. historyType=voice
   - 用户通过语音主动表达、询问、记录、确认或下达指令。
   - rawInputText 是主要事实来源。
   - screenshot 只用于理解上下文、项目、对话窗口和工作场景。
   - 如果 rawInputText 明确提出请求、计划、结论或进展，应优先保留。

2. historyType=rewrite
   - 用户正在改写、整理或准备发送文本。
   - rawInputText / rewrittenText 是主要事实来源。
   - screenshot 用于判断文本发送对象、文档背景、项目场景或任务上下文。
   - 不要把“正在编辑/准备发送”直接写成“已经发送”或“已经完成”。

3. historyType=screenshotRecord
   - 这只是屏幕上下文记录，表示用户当时正在查看、处理、讨论或记录某个页面。
   - 没有明确用户原文时，不要默认认为用户已经完成了截图中的任务。
   - 除非截图中有明确的完成、提交、发布、上线、修复完成、测试通过、已确认等证据，否则不得写成已完成工作。
   - 如果只是看到页面、聊天、文档、计划、列表、代码或链接，通常应判断为 viewed、discussed 或 inProgress，而不是 completed。

三、分析目标

优先提取有日报、周报价值的工作信息：
- 工作主题
- 项目方向
- 需求内容
- 技术问题
- 交付物
- 当前进展
- 明确结论
- 待办事项
- 风险或阻塞
- 协作对象
- 可复用的数据，例如进度百分比、模块名称、测试状态、缺陷状态

不要只描述界面。你需要判断“这条历史对工作复盘有什么价值”。

四、隐私与敏感信息

隐私优先，但不要把工作内容写空。

允许输出：
- 当前工作相关的真实联系人、昵称、群名、头像文字或会话名，用于准确识别沟通对象和上下文。
- 工作任务本身，例如“确认分公司端本周开发项”“整理直播系统进度汇报”。
- 与工作相关的非敏感数据，例如“进度约 90%”“已提测”“正在修复 BUG”。

禁止输出：
- 手机号、邮箱、地址、身份证、银行卡、验证码、密码、Token、API Key、Cookie、完整链接。
- 私人聊天细节。
- 客户个人身份敏感信息、员工敏感身份信息、薪酬绩效、敏感财务明细。
- 聊天消息逐字稿、长文逐字稿、大段 OCR。

如果截图是聊天工具：
- 只分析当前打开的会话。
- 不分析左侧会话列表、侧边栏其他联系人或历史会话。
- 如果判断为私人聊天，只输出“当前用户正在进行私人聊天”，不要提取具体内容。
- 如果是工作沟通，提取工作事项和任务含义，不复述聊天原文。

五、工作状态 workStatus

必须保守判断：

- completed：有明确完成、提交、交付、发布、上线、修复完成、测试通过、确认完成等证据。
- inProgress：正在处理、撰写、调试、分析、沟通、修复、测试或整理。
- planned：明确提出后续要做、请求执行、计划安排，但尚未开始或未完成。
- discussed：正在讨论某个工作事项，但没有明确行动、执行状态或完成证据。
- viewed：仅截图记录到某个页面、文档、聊天、链接、任务列表或材料。
- unknown：无法判断。

截图记录的默认倾向：
- 没有用户原文时，优先从 viewed / discussed / inProgress 中选择。
- 只有看到明确完成证据时，才允许 completed。
- 只看到任务、计划、代码、文档、链接或聊天，不等于 completed。

六、证据强度 evidenceLevel

必须反映事实可靠性：

- explicit：用户原文或截图中有明确文字证据。
- inferred：可以从截图、窗口标题、用户文本合理推断，但没有直接完成证据。
- weak：只有弱线索，不能作为确定事实。

判断原则：
- voice / rewrite 中用户明确表达的内容通常是 explicit。
- screenshotRecord 中没有用户原文时，即使能识别页面内容，也通常是 inferred 或 weak。
- 如果只是浏览器新标签页、链接、快捷方式、列表页，通常是 weak 或 inferred。

七、摘要要求

briefSummary：
- 用于后续语音润色和文本重写的上下文。
- 30 到 80 个中文字符。
- 短、具体、低干扰。
- 说明用户正在什么工作上下文中处理什么事项。
- 不输出账号、密钥、完整链接、手机号等敏感字段。

fullSummary：
- 用于日报、周报、月报和历史复盘。
- 100 到 300 个中文字符。
- 必须包含：工作主题、来源语义、工作状态、证据强度、可复用的工作信息。
- 如果没有明确完成证据，必须使用“正在处理”“正在讨论”“正在查看”“待确认”“未形成明确完成事项”等表述。
- 不要把截图记录直接写成已完成工作。
- 不要输出大段 OCR、聊天逐字稿或敏感信息原文。

actionItems：
- 只有存在明确请求、计划、下一步、待办、未完成事项时才输出。
- 不要把截图里出现的菜单、旧消息、文档标题、任务列表项直接当成待办。
- 每个待办都要简洁、可执行。
- 如果没有明确待办，输出空数组。

conversationName：
- 优先识别当前打开的会话、文档、网页、任务页或项目上下文。
- 可以输出真实联系人姓名、昵称、群名或头像文字，以保证后续按会话聚合的准确性。
- 如果无法可靠判断，输出 null。

八、输出要求

只输出一个合法 JSON 对象，不要输出 Markdown，不要解释，不要输出推理过程。
"#;

pub const DEFAULT_FULL_SUMMARY_PROMPT: &str = r#"fullSummary 是日报、周报、月报和历史复盘的上游材料，不是截图说明。请输出 100 到 300 个中文字符，让报告生成模型即使不看截图也能理解这条历史的工作含义：
- 必须交代当前工作背景，例如项目、页面、对话、文档、任务或正在处理的问题。
- 必须说明本条历史的来源语义：语音历史是用户通过语音表达、记录、询问或确认的内容；重写历史是用户对选中文本做表达调整或准备发送；截图记录只是屏幕中正在查看、讨论、处理或记录的上下文线索。
- 必须体现 workStatus 和 evidenceLevel 对应的事实强度。没有明确完成证据时，不要写成“已完成工作”。
- 对语音历史和重写历史，结合用户原文、处理后文本和截图上下文解释业务含义，不要只描述界面。
- 对截图记录，除非截图中有明确完成、提交、发布、上线、修复完成、测试通过或确认完成证据，否则应写成“正在查看/正在讨论/正在处理/待确认”。
- 提取可被报告复用的信息：进展、结论、待办、风险、协作对象、项目、交付物或后续价值；没有明确依据时写“未形成明确完成事项”或“待确认”。
- 保留必要的不确定性，不要把猜测包装成事实，不要补充截图、原文和窗口元数据之外的事实。
- 不输出大段 OCR、聊天逐字稿或与任务无关的 UI 描述。
- 不泄露 API Key、token、验证码、手机号、邮箱、地址、订单号、完整链接等敏感信息原文；如可见敏感信息，只做泛化说明。"#;

const OUTPUT_SCHEMA_PROMPT: &str = r#"

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
  "workStatus": "completed | inProgress | planned | discussed | viewed | unknown",
  "evidenceLevel": "explicit | inferred | weak",
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

#[derive(Debug, Clone)]
struct PromptSnapshot {
    system_prompt: String,
    hash: String,
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
    work_status: Option<ContextAnalysisWorkStatus>,
    evidence_level: Option<ContextAnalysisEvidenceLevel>,
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

pub fn spawn_analysis_for_screenshot_record(
    context_store: ContextCaptureStore,
    analysis_store: ContextAnalysisStore,
    screenshot_store: ScreenshotRecordStore,
    record: ScreenshotRecord,
) {
    std::thread::Builder::new()
        .name("openless-screenshot-record-analysis".into())
        .spawn(move || {
            tauri::async_runtime::block_on(async move {
                if let Err(error) =
                    run_screenshot_record_analysis(context_store, analysis_store, screenshot_store, record).await
                {
                    log::warn!("[screenshot-record] analysis task failed: {error:#}");
                }
            });
        })
        .ok();
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
    let prompt = prompt_snapshot(prefs.context_analysis_full_summary_prompt.as_deref());

    let context = wait_for_context(&context_store, history_type, &history_id)?;
    let Some(context) = context else {
        let result = skipped_result(
            history_type,
            &history_id,
            "skipped:screenshotUnavailable",
            &prompt,
        );
        upsert_if_context_current(
            &context_store,
            &analysis_store,
            result,
            history_type,
            &history_id,
            None,
            None,
        )?;
        return Ok(());
    };
    let pending_result = pending_analysis_result(&context);
    let pending_result = with_prompt_snapshot(pending_result, &prompt);
    upsert_if_context_current(
        &context_store,
        &analysis_store,
        pending_result.clone(),
        history_type,
        &history_id,
        Some(&context.id),
        None,
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
            Some(prompt.hash.as_str()),
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
            Some(prompt.hash.as_str()),
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
            Some(prompt.hash.as_str()),
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
            Some(prompt.hash.as_str()),
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
            Some(prompt.hash.as_str()),
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
                Some(prompt.hash.as_str()),
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
            &prompt,
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
                Some(prompt.hash.as_str()),
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
                Some(prompt.hash.as_str()),
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
                Some(prompt.hash.as_str()),
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
                Some(prompt.hash.as_str()),
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
        Some(prompt.hash.as_str()),
    )?;
    Ok(())
}

async fn run_screenshot_record_analysis(
    context_store: ContextCaptureStore,
    analysis_store: ContextAnalysisStore,
    screenshot_store: ScreenshotRecordStore,
    record: ScreenshotRecord,
) -> Result<()> {
    let prefs = crate::persistence::PreferencesStore::new()
        .unwrap_or_else(|_| crate::persistence::PreferencesStore::new_fallback())
        .get();
    if !prefs.context_vision_analysis_enabled || !prefs.context_vision_analysis_consent_accepted {
        update_screenshot_record_status_if_unchanged(
            &screenshot_store,
            &record,
            ScreenshotRecordStatus::Skipped,
            None,
            Some("skipped:consentRequired".into()),
            None,
        )?;
        return Ok(());
    }
    let prompt = prompt_snapshot(prefs.context_analysis_full_summary_prompt.as_deref());

    let Some(first_context_id) = record.submitted_screenshot_ids.first().cloned() else {
        update_screenshot_record_status_if_unchanged(
            &screenshot_store,
            &record,
            ScreenshotRecordStatus::Failed,
            None,
            Some("failed:noSubmittedScreenshot".into()),
            None,
        )?;
        return Ok(());
    };
    let contexts = context_store.list()?;
    let Some(first_context) = contexts.iter().find(|entry| entry.id == first_context_id).cloned() else {
        update_screenshot_record_status_if_unchanged(
            &screenshot_store,
            &record,
            ScreenshotRecordStatus::Failed,
            None,
            Some("failed:contextMissing".into()),
            None,
        )?;
        return Ok(());
    };

    let mut pending_result = pending_analysis_result(&first_context);
    pending_result = with_prompt_snapshot(pending_result, &prompt);
    pending_result.linked_history_type = ContextCaptureHistoryType::ScreenshotRecord;
    pending_result.linked_history_id = record.id.clone();
    pending_result.input_mode = "screenshot_record".into();
    let generation = pending_result
        .analysis_generation
        .clone()
        .unwrap_or_else(|| pending_result.id.clone());
    let pending_updated = screenshot_store.update_analysis_if_generation_newer(
        &record.id,
        ScreenshotRecordStatus::Analyzing,
        pending_result.clone(),
    )?;
    if !pending_updated {
        log::info!("[screenshot-record] skip stale pending analysis for record_id={}", record.id);
        return Ok(());
    }
    analysis_store.upsert_if_generation_newer(pending_result.clone())?;

    let active_provider = crate::persistence::CredentialsVault::get_active_llm();
    if active_provider == crate::polish::CODEX_OAUTH_PROVIDER_ID || active_provider == "gemini" {
        let result = with_provider_error(
            pending_result,
            ContextAnalysisStatus::Skipped,
            "skipped:unsupportedProvider",
            Some(active_provider),
            None,
        );
        let _ = update_screenshot_record_analysis_if_current(
            &analysis_store,
            &screenshot_store,
            &record.id,
            prompt.hash.as_str(),
            generation.as_str(),
            ScreenshotRecordStatus::Skipped,
            result,
            Some("skipped:unsupportedProvider".into()),
            None,
        );
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
    if endpoint.trim().is_empty() || api_key.trim().is_empty() || model.trim().is_empty() {
        let error_code = if model.trim().is_empty() {
            "skipped:modelNotConfigured"
        } else {
            "skipped:providerNotConfigured"
        };
        let result = with_provider_error(
            pending_result,
            ContextAnalysisStatus::Skipped,
            error_code,
            Some(active_provider),
            if model.trim().is_empty() { None } else { Some(model) },
        );
        let _ = update_screenshot_record_analysis_if_current(
            &analysis_store,
            &screenshot_store,
            &record.id,
            prompt.hash.as_str(),
            generation.as_str(),
            ScreenshotRecordStatus::Skipped,
            result,
            Some(error_code.into()),
            None,
        );
        return Ok(());
    }

    let mut prepared_images = Vec::new();
    for context_id in &record.submitted_screenshot_ids {
        let Some(context) = contexts.iter().find(|entry| entry.id == *context_id) else {
            continue;
        };
        let Some(screenshot_ref) = context.screenshot_ref.as_deref() else {
            continue;
        };
        let path = context_store.screenshot_path_for_ref(screenshot_ref)?;
        match prepare_image_for_vision(&path) {
            Ok(image) => prepared_images.push(image),
            Err(error) => log::warn!("[screenshot-record] prepare image failed: {error:#}"),
        }
    }
    if prepared_images.is_empty() {
        let result = with_provider_error(
            pending_result,
            ContextAnalysisStatus::Failed,
            "failed:imagePrepareFailed",
            Some(active_provider),
            Some(model),
        );
        let _ = update_screenshot_record_analysis_if_current(
            &analysis_store,
            &screenshot_store,
            &record.id,
            prompt.hash.as_str(),
            generation.as_str(),
            ScreenshotRecordStatus::Failed,
            result,
            Some("failed:imagePrepareFailed".into()),
            None,
        );
        return Ok(());
    }

    let input = ContextAnalysisTextInput {
        raw_input_text: "该记录来自截图记录，无显式语音/重写原文。".into(),
        final_text: None,
        rewritten_text: None,
    };
    let response = match {
        let _request_guard = analysis_request_lock().lock().await;
        if !screenshot_store.analysis_is_current(
            &record.id,
            prompt.hash.as_str(),
            generation.as_str(),
            ScreenshotRecordStatus::Analyzing,
        )? {
            log::info!(
                "[screenshot-record] skip stale request for record_id={} generation={}",
                record.id,
                generation
            );
            return Ok(());
        }
        request_context_analysis_multi(
            &active_provider,
            &endpoint,
            &api_key,
            &model,
            &first_context,
            &input,
            &prepared_images,
            &prompt,
        )
        .await
    } {
        Ok(text) => text,
        Err(AnalysisRequestError::ModelNotVisionCapable) => {
            let result = with_images_error(
                pending_result,
                ContextAnalysisStatus::Failed,
                "failed:modelNotVisionCapable",
                &active_provider,
                &model,
                &prepared_images,
            );
            let _ = update_screenshot_record_analysis_if_current(
                &analysis_store,
                &screenshot_store,
                &record.id,
                prompt.hash.as_str(),
                generation.as_str(),
                ScreenshotRecordStatus::Failed,
                result,
                Some("failed:modelNotVisionCapable".into()),
                None,
            );
            return Ok(());
        }
        Err(AnalysisRequestError::Timeout(error)) => {
            let result = with_images_error(
                pending_result,
                ContextAnalysisStatus::Failed,
                "failed:visionRequestTimeout",
                &active_provider,
                &model,
                &prepared_images,
            );
            let _ = update_screenshot_record_analysis_if_current(
                &analysis_store,
                &screenshot_store,
                &record.id,
                prompt.hash.as_str(),
                generation.as_str(),
                ScreenshotRecordStatus::Failed,
                result,
                Some("failed:visionRequestTimeout".into()),
                Some(error),
            );
            return Ok(());
        }
        Err(error) => {
            let result = with_images_error(
                pending_result,
                ContextAnalysisStatus::Failed,
                "failed:visionRequestFailed",
                &active_provider,
                &model,
                &prepared_images,
            );
            let _ = update_screenshot_record_analysis_if_current(
                &analysis_store,
                &screenshot_store,
                &record.id,
                prompt.hash.as_str(),
                generation.as_str(),
                ScreenshotRecordStatus::Failed,
                result,
                Some("failed:visionRequestFailed".into()),
                Some(error.to_string()),
            );
            return Ok(());
        }
    };

    let parsed = match parse_model_output(&response) {
        Ok(parsed) => parsed,
        Err(error) => {
            let result = with_images_error(
                pending_result,
                ContextAnalysisStatus::Failed,
                "failed:invalidModelOutput",
                &active_provider,
                &model,
                &prepared_images,
            );
            let _ = update_screenshot_record_analysis_if_current(
                &analysis_store,
                &screenshot_store,
                &record.id,
                prompt.hash.as_str(),
                generation.as_str(),
                ScreenshotRecordStatus::Failed,
                result,
                Some("failed:invalidModelOutput".into()),
                Some(error.to_string()),
            );
            return Ok(());
        }
    };

    let result = success_result_multi(
        pending_result,
        parsed,
        &active_provider,
        &model,
        &prepared_images,
    );
    update_screenshot_record_analysis_if_current(
        &analysis_store,
        &screenshot_store,
        &record.id,
        prompt.hash.as_str(),
        generation.as_str(),
        ScreenshotRecordStatus::Success,
        result,
        None,
        None,
    )?;
    Ok(())
}

fn update_screenshot_record_status_if_unchanged(
    screenshot_store: &ScreenshotRecordStore,
    record: &ScreenshotRecord,
    status: ScreenshotRecordStatus,
    analysis: Option<ContextAnalysisResult>,
    error_code: Option<String>,
    error_message: Option<String>,
) -> Result<()> {
    let expected_prompt_hash = record
        .analysis
        .as_ref()
        .and_then(|analysis| analysis.prompt_hash.as_deref());
    let expected_generation = record
        .analysis
        .as_ref()
        .and_then(|analysis| analysis.analysis_generation.as_deref());
    let updated = screenshot_store.update_analysis_if_current(
        &record.id,
        expected_prompt_hash,
        expected_generation,
        status,
        analysis,
        error_code,
        error_message,
    )?;
    if !updated {
        log::info!(
            "[screenshot-record] skip stale early analysis state for record_id={}",
            record.id
        );
    }
    Ok(())
}

fn update_screenshot_record_analysis_if_current(
    analysis_store: &ContextAnalysisStore,
    screenshot_store: &ScreenshotRecordStore,
    record_id: &str,
    expected_prompt_hash: &str,
    expected_generation: &str,
    status: ScreenshotRecordStatus,
    result: ContextAnalysisResult,
    error_code: Option<String>,
    error_message: Option<String>,
) -> Result<()> {
    let updated = screenshot_store.update_analysis_if_current(
        record_id,
        Some(expected_prompt_hash),
        Some(expected_generation),
        status,
        Some(result.clone()),
        error_code,
        error_message,
    )?;
    if updated {
        analysis_store.upsert_if_generation_current(result, Some(expected_generation))?;
    } else {
        log::info!("[screenshot-record] skip stale analysis result for record_id={record_id}");
    }
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
    expected_prompt_hash: Option<&str>,
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
    if let Some(context_id) = expected_context_id {
        let latest = analysis_store.latest_for_context(history_type, history_id, context_id)?;
        if let Some(prompt_hash) = expected_prompt_hash {
            let latest_hash = latest.as_ref().and_then(|entry| entry.prompt_hash.as_deref());
            if latest_hash != Some(prompt_hash) {
                log::info!(
                    "[context-analysis] skip stale prompt result for history_type={history_type:?} history_id={history_id}"
                );
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
    if let Some(context_id) = expected_context_id {
        if expected_prompt_hash.is_none() {
            let updated = analysis_store.upsert_if_generation_newer(result)?;
            if !updated {
                log::info!(
                    "[context-analysis] skip stale pending upsert for history_type={history_type:?} history_id={history_id}"
                );
            }
            Ok(())
        } else {
            let expected_generation = result.analysis_generation.clone();
            let updated = analysis_store.upsert_if_generation_current(
                result,
                expected_generation.as_deref(),
            )?;
            if !updated {
                log::info!(
                    "[context-analysis] skip stale final upsert for history_type={history_type:?} history_id={history_id} context_id={context_id}"
                );
            }
            Ok(())
        }
    } else {
        analysis_store.upsert(result)
    }
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
        ContextCaptureHistoryType::ScreenshotRecord => {
            let store = crate::persistence::ScreenshotRecordStore::new()
                .unwrap_or_else(|_| crate::persistence::ScreenshotRecordStore::new_fallback());
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
    prompt: &PromptSnapshot,
) -> std::result::Result<String, AnalysisRequestError> {
    request_context_analysis_multi(
        provider_id,
        endpoint,
        api_key,
        model,
        context,
        input,
        std::slice::from_ref(image),
        prompt,
    )
    .await
}

async fn request_context_analysis_multi(
    provider_id: &str,
    endpoint: &str,
    api_key: &str,
    model: &str,
    context: &ContextCaptureEntry,
    input: &ContextAnalysisTextInput,
    images: &[PreparedVisionImage],
    prompt: &PromptSnapshot,
) -> std::result::Result<String, AnalysisRequestError> {
    let url = chat_completions_url(endpoint);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|error| AnalysisRequestError::Network(error.to_string()))?;
    let metadata = json!({
        "historyType": match context.linked_history_type {
            ContextCaptureHistoryType::Voice => "voice",
            ContextCaptureHistoryType::Rewrite => "rewrite",
            ContextCaptureHistoryType::ScreenshotRecord => "screenshotRecord",
        },
        "rawInputText": input.raw_input_text,
        "finalText": input.final_text,
        "rewrittenText": input.rewritten_text,
        "windowTitle": context.window_title,
        "capturedApp": context.context_app,
        "capturedConversationWindow": context.conversation_window,
    });
    let mut content = vec![json!({
        "type": "text",
        "text": serde_json::to_string_pretty(&metadata).unwrap_or_else(|_| "{}".to_string())
    })];
    for image in images {
        let image_base64 = base64::engine::general_purpose::STANDARD.encode(&image.bytes);
        content.push(json!({
            "type": "image_url",
            "image_url": {
                "url": format!("data:{};base64,{}", image.mime_type, image_base64)
            }
        }));
    }
    let body = json!({
        "model": model,
        "stream": false,
        "temperature": 0.2,
        "messages": [
            { "role": "system", "content": prompt.system_prompt },
            {
                "role": "user",
                "content": content
            }
        ]
    });

    let total_image_bytes: usize = images.iter().map(|image| image.bytes.len()).sum();
    let first_size = images
        .first()
        .map(|image| format!("{}x{}", image.width, image.height))
        .unwrap_or_else(|| "0x0".into());
    log::info!(
        "[context-analysis] POST {} provider={} model={} images={} first={} bytes={}",
        redacted_url_for_log(&url),
        provider_id,
        model,
        images.len(),
        first_size,
        total_image_bytes
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
    result: ContextAnalysisResult,
    parsed: ModelAnalysisJson,
    provider_id: &str,
    model: &str,
    image: &PreparedVisionImage,
) -> ContextAnalysisResult {
    success_result_multi(result, parsed, provider_id, model, std::slice::from_ref(image))
}

fn success_result_multi(
    mut result: ContextAnalysisResult,
    parsed: ModelAnalysisJson,
    provider_id: &str,
    model: &str,
    images: &[PreparedVisionImage],
) -> ContextAnalysisResult {
    result.status = ContextAnalysisStatus::Success;
    result.analyzed_at = Some(chrono::Utc::now().to_rfc3339());
    result.provider_id = Some(provider_id.to_string());
    result.model = Some(model.to_string());
    if let Some(first) = images.first() {
        result.image_mime_type = Some(first.mime_type.to_string());
        result.image_width = Some(first.width);
        result.image_height = Some(first.height);
    }
    result.image_bytes = Some(images.iter().map(|image| image.bytes.len() as u64).sum());
    result.conversation_name = normalize_optional(parsed.conversation_name);
    result.brief_summary = Some(parsed.brief_summary.trim().to_string());
    result.full_summary = Some(parsed.full_summary.trim().to_string());
    result.detected_app = normalize_optional(parsed.detected_app);
    result.detected_context_type = parsed.detected_context_type.unwrap_or_default();
    result.topic = normalize_optional(parsed.topic);
    result.user_intent = normalize_optional(parsed.user_intent);
    result.activity_type = parsed.activity_type.unwrap_or_default();
    result.work_status = parsed.work_status.unwrap_or_default();
    result.evidence_level = parsed.evidence_level.unwrap_or_default();
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
    prompt: &PromptSnapshot,
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
        prompt_hash: Some(prompt.hash.clone()),
        analysis_generation: Some(new_analysis_generation()),
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
        work_status: Default::default(),
        evidence_level: Default::default(),
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

fn prompt_snapshot(custom_full_summary_prompt: Option<&str>) -> PromptSnapshot {
    let full_summary_prompt = custom_full_summary_prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_FULL_SUMMARY_PROMPT);
    let system_prompt = format!(
        "{}\n\n完整摘要生成规则：\n{}\n\n{}",
        BASE_SYSTEM_PROMPT.trim(),
        full_summary_prompt,
        OUTPUT_SCHEMA_PROMPT.trim()
    );
    let mut hasher = Sha256::new();
    hasher.update(PROMPT_VERSION.as_bytes());
    hasher.update(b"\n");
    hasher.update(system_prompt.as_bytes());
    PromptSnapshot {
        system_prompt,
        hash: format!("{:x}", hasher.finalize()),
    }
}

fn with_prompt_snapshot(
    mut result: ContextAnalysisResult,
    prompt: &PromptSnapshot,
) -> ContextAnalysisResult {
    result.prompt_version = PROMPT_VERSION.to_string();
    result.prompt_hash = Some(prompt.hash.clone());
    if result.analysis_generation.is_none() {
        result.analysis_generation = Some(new_analysis_generation());
    }
    result
}

fn new_analysis_generation() -> String {
    format!(
        "{}-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        uuid::Uuid::new_v4()
    )
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
    result: ContextAnalysisResult,
    status: ContextAnalysisStatus,
    error_code: &str,
    provider_id: &str,
    model: &str,
    image: &PreparedVisionImage,
) -> ContextAnalysisResult {
    with_images_error(result, status, error_code, provider_id, model, std::slice::from_ref(image))
}

fn with_images_error(
    mut result: ContextAnalysisResult,
    status: ContextAnalysisStatus,
    error_code: &str,
    provider_id: &str,
    model: &str,
    images: &[PreparedVisionImage],
) -> ContextAnalysisResult {
    result = with_provider_error(
        result,
        status,
        error_code,
        Some(provider_id.to_string()),
        Some(model.to_string()),
    );
    if let Some(first) = images.first() {
        result.image_mime_type = Some(first.mime_type.to_string());
        result.image_width = Some(first.width);
        result.image_height = Some(first.height);
    }
    result.image_bytes = Some(images.iter().map(|image| image.bytes.len() as u64).sum());
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
