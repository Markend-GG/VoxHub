//! 阿里云百炼（DashScope）多模态生成同步接口的批量 ASR 客户端。
//!
//! `fun-asr-flash` 与 `qwen-audio-3.0-asr-flash` 系列是**非实时录音文件识别**
//! 模型，走 DashScope 私有的
//! `multimodal-generation/generation` HTTP 接口，既不是实时 WebSocket 双工
//! （见 `bailian.rs`），也不是 OpenAI 兼容的 `/audio/transcriptions`
//! （见 `whisper.rs`）。因此单独成一路批量客户端：录音结束后把整段 PCM 编成
//! WAV、base64 进 JSON body、POST 一次拿整段文本。
//!
//! 结构与 `mimo.rs`（同为「攒 PCM → POST 一段音频 → 解析私有 JSON」）一致，
//! 复用其 `split_pcm_by_duration` / `join_transcript_chunks` 分片与拼接逻辑，
//! 只有请求信封与响应解析不同。

use anyhow::{Context, Result};
use base64::Engine;
use parking_lot::Mutex;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::asr::meeting_audio_source::MeetingAudioSource;
use crate::asr::mimo::{join_transcript_chunks, split_pcm_by_duration};
use crate::asr::wav::encode_wav_16k_mono;
use crate::asr::RawTranscript;

// fun-asr-flash 单条音频上限 5 分钟；但真正的硬约束是 base64 进 JSON 的请求体
// 体积。沿用 mimo 验证过的 180s 预算（16k/16-bit/mono WAV base64 后约 7.7MB），
// 稳稳落在时长和常见网关体积上限之内。超长录音按此切分后逐段识别再拼接。
const DASHSCOPE_MAX_CHUNK_DURATION_MS: u64 = 180_000;
const ASYNC_TASK_POLL_TIMEOUT_SECS: u64 = 600;
const ASYNC_WORKFLOW_OVERHEAD_SECS: u64 = 60;
const ASYNC_UPLOAD_BYTES_PER_SEC: u64 = 64 * 1024;

pub const PROVIDER_ID: &str = "bailian-fun-asr-flash";
pub const DEFAULT_ENDPOINT: &str =
    "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation";
pub const ASYNC_DEFAULT_ENDPOINT: &str =
    "https://dashscope.aliyuncs.com/api/v1/services/audio/asr/transcription";
pub const DEFAULT_MODEL: &str = "fun-asr-flash-2026-06-15";
pub const QWEN_AUDIO_MODEL: &str = "qwen-audio-3.0-asr-flash";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashScopeBatchProtocol {
    Multimodal,
    AsyncTranscription,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashScopeAsyncRequestOptions {
    pub diarization_enabled: bool,
    pub speaker_count: Option<u32>,
}

impl DashScopeAsyncRequestOptions {
    pub fn validate(self) -> Result<Self> {
        if !self.diarization_enabled && self.speaker_count.is_some() {
            anyhow::bail!("speaker_count requires diarization_enabled=true");
        }
        if self
            .speaker_count
            .is_some_and(|count| !(2..=100).contains(&count))
        {
            anyhow::bail!("speaker_count must be between 2 and 100");
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashScopeAsyncSentence {
    pub begin_time_ms: u64,
    pub end_time_ms: u64,
    pub text: String,
    pub sentence_id: Option<String>,
    pub speaker_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashScopeAsyncTranscript {
    pub sentences: Vec<DashScopeAsyncSentence>,
}

#[derive(Debug, Clone)]
struct DashScopeUploadPolicy {
    upload_dir: String,
    upload_host: String,
    oss_access_key_id: String,
    policy: String,
    signature: String,
    object_acl: String,
    forbid_overwrite: String,
}

fn is_realtime_model(model: &str) -> bool {
    model.contains("realtime")
}

fn is_qwen_filetrans_model(model: &str) -> bool {
    model.starts_with("qwen3-asr-flash-filetrans")
}

fn is_qwen_sync_model(model: &str) -> bool {
    model.starts_with("qwen3-asr-flash")
        && !is_qwen_filetrans_model(model)
        && !is_realtime_model(model)
}

fn is_qwen_audio_model(model: &str) -> bool {
    // 同步录音文件模型；`-streaming` 流式变体不在批量协议支持范围内。
    model.starts_with("qwen-audio") && !model.contains("streaming")
}

pub fn protocol_for_model(model: &str) -> Option<DashScopeBatchProtocol> {
    let model = model.trim();
    if model.is_empty() || is_realtime_model(model) {
        return None;
    }
    // qwen3-asr-flash-filetrans 官方仅接受公网音频 URL，与本地录音的临时 OSS
    // 上传 + oss:// 链路不兼容，暂不纳入支持：显式拒绝，避免被误路由到异步协议
    // 造成「验证通过但真实录音必然失败」。
    if is_qwen_filetrans_model(model) {
        return None;
    }
    if model.starts_with("fun-asr-flash") || is_qwen_sync_model(model) || is_qwen_audio_model(model)
    {
        return Some(DashScopeBatchProtocol::Multimodal);
    }
    if model == "fun-asr" || model.starts_with("fun-asr-") || model.starts_with("paraformer") {
        return Some(DashScopeBatchProtocol::AsyncTranscription);
    }
    None
}

pub struct DashScopeMultimodalASR {
    api_key: String,
    base_url: String,
    model: String,
    buffer: Mutex<Vec<u8>>,
}

impl DashScopeMultimodalASR {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            api_key,
            base_url,
            model,
            buffer: Mutex::new(Vec::new()),
        }
    }

    pub fn buffer_duration_ms(&self) -> u64 {
        crate::asr::pcm::pcm_duration_ms(&self.buffer.lock())
    }

    pub fn transcribe_timeout(&self, audio_secs: f64) -> Duration {
        if protocol_for_model(&self.model) == Some(DashScopeBatchProtocol::AsyncTranscription) {
            let pcm_bytes = (audio_secs.max(0.0) * 32_000.0).ceil() as u64;
            return async_upload_timeout(pcm_bytes.saturating_add(44))
                + Duration::from_secs(ASYNC_TASK_POLL_TIMEOUT_SECS + ASYNC_WORKFLOW_OVERHEAD_SECS);
        }
        let secs = ((audio_secs * 0.5).ceil() as u64)
            .saturating_add(20)
            .max(30);
        Duration::from_secs(secs)
    }

    pub async fn transcribe(&self) -> Result<RawTranscript> {
        let pcm = self.buffer.lock().clone();
        if pcm.is_empty() {
            return Ok(RawTranscript {
                text: String::new(),
                duration_ms: 0,
            });
        }

        let result = self.transcribe_inner(&pcm).await;
        if result.is_ok() {
            self.buffer.lock().clear();
        }
        result
    }

    async fn transcribe_inner(&self, pcm: &[u8]) -> Result<RawTranscript> {
        if self.api_key.trim().is_empty() {
            anyhow::bail!("DashScope API key missing");
        }

        let duration_ms = crate::asr::pcm::pcm_duration_ms(pcm);
        if protocol_for_model(&self.model) == Some(DashScopeBatchProtocol::AsyncTranscription) {
            let samples: Vec<i16> = pcm
                .chunks_exact(2)
                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                .collect();
            let wav = encode_wav_16k_mono(&samples);
            let text = self.transcribe_async(&wav).await?;
            return Ok(RawTranscript { text, duration_ms });
        }
        let chunks = split_pcm_by_duration(pcm, DASHSCOPE_MAX_CHUNK_DURATION_MS);
        let mut texts = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            texts.push(self.transcribe_chunk(chunk).await?);
        }

        Ok(RawTranscript {
            text: join_transcript_chunks(&texts),
            duration_ms,
        })
    }

    async fn transcribe_chunk(&self, pcm: &[u8]) -> Result<String> {
        let samples: Vec<i16> = pcm
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        let wav = encode_wav_16k_mono(&samples);
        let body = dashscope_multimodal_body(&self.model, &wav);
        let url = generation_url(&self.base_url)?;
        let request_timeout =
            self.transcribe_timeout(crate::asr::pcm::pcm_duration_ms(pcm) as f64 / 1000.0);
        let resp = crate::net::credential_http()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key.trim()))
            .header("Content-Type", "application/json")
            // multimodal-generation 默认可 SSE 流式；显式关掉走一次性 JSON 响应。
            .header("X-DashScope-SSE", "disable")
            .json(&body)
            .timeout(request_timeout)
            .send()
            .await
            .context("DashScope ASR HTTP request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("DashScope ASR API error {}: {}", status, body);
        }

        let json: Value = resp.json().await.context("parse DashScope ASR response")?;
        Ok(extract_dashscope_text(&json).trim().to_string())
    }

    async fn transcribe_async(&self, wav: &[u8]) -> Result<String> {
        let file_url = self.upload_temporary_wav(wav).await?;
        self.transcribe_async_url(&file_url).await
    }

    async fn upload_temporary_wav(&self, wav: &[u8]) -> Result<String> {
        let policy = self.request_upload_policy().await?;
        let object_key = format!("{}/audio.wav", policy.upload_dir.trim_end_matches('/'));
        let form = upload_form(
            &policy,
            &object_key,
            reqwest::multipart::Part::bytes(wav.to_vec())
                .file_name("audio.wav")
                .mime_str("audio/wav")?,
        );
        self.upload_form(form, &policy.upload_host, wav.len() as u64)
            .await?;
        Ok(format!("oss://{object_key}"))
    }

    pub async fn upload_meeting_audio(
        &self,
        source: MeetingAudioSource,
        cancelled: Arc<AtomicBool>,
    ) -> Result<String> {
        if cancelled.load(Ordering::Acquire) {
            anyhow::bail!("meeting audio upload cancelled");
        }
        let policy = self.request_upload_policy().await?;
        let object_key = format!("{}/audio.wav", policy.upload_dir.trim_end_matches('/'));
        let (info, stream) = source.into_stream(cancelled)?;
        let body = reqwest::Body::wrap_stream(stream);
        let part = reqwest::multipart::Part::stream_with_length(body, info.content_length)
            .file_name("audio.wav")
            .mime_str("audio/wav")?;
        let form = upload_form(&policy, &object_key, part);
        self.upload_form(form, &policy.upload_host, info.content_length)
            .await?;
        Ok(format!("oss://{object_key}"))
    }

    async fn request_upload_policy(&self) -> Result<DashScopeUploadPolicy> {
        let mut policy_url = api_url(&self.base_url, "/api/v1/uploads")?;
        policy_url
            .query_pairs_mut()
            .append_pair("action", "getPolicy")
            .append_pair("model", self.model.trim());
        let response = crate::net::credential_http()
            .get(policy_url)
            .header("Authorization", format!("Bearer {}", self.api_key.trim()))
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("request DashScope temporary upload policy")?;
        let policy_json = response_json(response, "DashScope upload policy").await?;
        let data = policy_json
            .get("data")
            .context("DashScope upload policy missing data")?;
        let field = |name: &str| -> Result<String> {
            data.get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .with_context(|| format!("DashScope upload policy missing {name}"))
        };
        let upload_dir = field("upload_dir")?;
        Ok(DashScopeUploadPolicy {
            upload_dir,
            upload_host: field("upload_host")?,
            oss_access_key_id: field("oss_access_key_id")?,
            policy: field("policy")?,
            signature: field("signature")?,
            object_acl: field("x_oss_object_acl")?,
            forbid_overwrite: field("x_oss_forbid_overwrite")?,
        })
    }

    async fn upload_form(
        &self,
        form: reqwest::multipart::Form,
        upload_host: &str,
        audio_bytes: u64,
    ) -> Result<()> {
        let upload_url = dashscope_transfer_url(upload_host)?;
        let upload = crate::net::anonymous_no_redirect_http()
            .post(upload_url)
            .multipart(form)
            .timeout(async_upload_timeout(audio_bytes))
            .send()
            .await
            .context("upload audio to DashScope temporary storage")?;
        ensure_success(upload, "DashScope temporary upload").await?;
        Ok(())
    }

    pub async fn transcribe_async_url(&self, file_url: &str) -> Result<String> {
        self.transcribe_async_url_with_timeout(
            file_url,
            Duration::from_secs(ASYNC_TASK_POLL_TIMEOUT_SECS),
        )
        .await
    }

    /// 提交异步任务并轮询至完成。`poll_timeout` 是任务轮询阶段的硬截止时间：
    /// 真实转写用长轮询（默认 600s），连通性验证用短轮询以便快速返回，避免
    /// 「验证」按钮在最坏情况下阻塞近 11 分钟。
    pub async fn transcribe_async_url_with_timeout(
        &self,
        file_url: &str,
        poll_timeout: Duration,
    ) -> Result<String> {
        let task_id = self
            .submit_async_task(
                file_url,
                DashScopeAsyncRequestOptions {
                    diarization_enabled: false,
                    speaker_count: None,
                },
            )
            .await?;
        let transcript = self
            .poll_async_task(&task_id, poll_timeout, Arc::new(AtomicBool::new(false)))
            .await?;
        Ok(transcript
            .sentences
            .iter()
            .map(|sentence| sentence.text.as_str())
            .collect::<Vec<_>>()
            .join(" "))
    }

    pub async fn submit_async_task(
        &self,
        file_url: &str,
        options: DashScopeAsyncRequestOptions,
    ) -> Result<String> {
        let options = options.validate()?;
        let submit_url = async_transcription_url(&self.base_url)?;
        let response = crate::net::credential_http()
            .post(submit_url)
            .header("Authorization", format!("Bearer {}", self.api_key.trim()))
            .header("Content-Type", "application/json")
            .header("X-DashScope-Async", "enable")
            .header("X-DashScope-OssResourceResolve", "enable")
            .json(&async_transcription_body_with_options(
                &self.model,
                file_url,
                options,
            ))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("submit DashScope async ASR task")?;
        let submitted = response_json(response, "DashScope async ASR submission").await?;
        submitted
            .pointer("/output/task_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .context("DashScope async ASR response missing task_id")
    }

    pub async fn poll_async_task(
        &self,
        task_id: &str,
        poll_timeout: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> Result<DashScopeAsyncTranscript> {
        if task_id.trim().is_empty() {
            anyhow::bail!("DashScope async ASR task_id is empty");
        }
        let task_url = api_url(&self.base_url, &format!("/api/v1/tasks/{task_id}"))?;
        let deadline = Instant::now() + poll_timeout;
        let completed = loop {
            if cancelled.load(Ordering::Acquire) {
                anyhow::bail!("DashScope async ASR task polling cancelled");
            }
            // 轮询窗口最长可达 600s、每秒一次：对瞬态网络失败做有界重试，
            // 避免 10 分钟内单次连接抖动/5xx 直接废弃整段转写。
            let task = get_json_with_retry(
                crate::net::credential_http(),
                task_url.clone(),
                Some(self.api_key.trim()),
                deadline,
                "poll DashScope async ASR task",
            )
            .await?;
            match task
                .pointer("/output/task_status")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "SUCCEEDED" => break task,
                "FAILED" | "CANCELED" | "UNKNOWN" => {
                    let message = task
                        .get("message")
                        .or_else(|| task.pointer("/output/message"))
                        .and_then(Value::as_str)
                        .unwrap_or("task failed");
                    anyhow::bail!("DashScope async ASR task failed: {message}");
                }
                _ if Instant::now() >= deadline => {
                    anyhow::bail!("DashScope async ASR task timed out");
                }
                _ => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        };
        if cancelled.load(Ordering::Acquire) {
            anyhow::bail!("DashScope async ASR task polling cancelled");
        }
        let result = download_async_result(&extract_async_result_url(&completed)?).await?;
        extract_async_transcript_for_model(&self.model, &result)
    }

    pub async fn cancel_async_task(&self, task_id: &str) -> Result<()> {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            anyhow::bail!("DashScope async ASR task_id is empty");
        }
        let cancel_url = api_url(&self.base_url, &format!("/api/v1/tasks/{task_id}/cancel"))?;
        let response = crate::net::credential_http()
            .post(cancel_url)
            .header("Authorization", format!("Bearer {}", self.api_key.trim()))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("cancel DashScope async ASR task")?;
        ensure_success(response, "DashScope async ASR cancellation").await
    }

    pub fn cancel(&self) {
        self.buffer.lock().clear();
    }
}

fn api_url(base_url: &str, path: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url.trim()).context("parse DashScope base URL")?;
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn dashscope_transfer_url(raw: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(raw.trim()).context("parse DashScope transfer URL")?;
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("DashScope transfer URL must not contain credentials");
    }
    let host = url
        .host_str()
        .context("DashScope transfer URL missing host")?
        .to_ascii_lowercase();

    #[cfg(test)]
    if host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    {
        return Ok(url);
    }

    if host != "aliyuncs.com" && !host.ends_with(".aliyuncs.com") {
        anyhow::bail!("DashScope transfer URL must use an Alibaba Cloud OSS host");
    }
    match url.scheme() {
        "https" => {}
        "http" => url
            .set_scheme("https")
            .map_err(|_| anyhow::anyhow!("upgrade DashScope transfer URL to HTTPS"))?,
        _ => anyhow::bail!("DashScope transfer URL must use HTTPS"),
    }
    Ok(url)
}

fn async_upload_timeout(bytes: u64) -> Duration {
    let transfer_secs =
        bytes.saturating_add(ASYNC_UPLOAD_BYTES_PER_SEC - 1) / ASYNC_UPLOAD_BYTES_PER_SEC;
    Duration::from_secs(transfer_secs.saturating_add(30).max(60))
}

fn upload_form(
    policy: &DashScopeUploadPolicy,
    object_key: &str,
    file: reqwest::multipart::Part,
) -> reqwest::multipart::Form {
    reqwest::multipart::Form::new()
        .text("OSSAccessKeyId", policy.oss_access_key_id.clone())
        .text("policy", policy.policy.clone())
        .text("Signature", policy.signature.clone())
        .text("key", object_key.to_string())
        .text("x-oss-object-acl", policy.object_acl.clone())
        .text("x-oss-forbid-overwrite", policy.forbid_overwrite.clone())
        .text("success_action_status", "200")
        .part("file", file)
}

async fn download_async_result(raw_url: &str) -> Result<Value> {
    let result_url = dashscope_transfer_url(raw_url)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    get_json_with_retry(
        crate::net::anonymous_no_redirect_http(),
        result_url,
        None,
        deadline,
        "download DashScope async ASR result",
    )
    .await
}

/// GET JSON 请求的瞬态失败重试上限（指数退避 500ms / 1s / 2s / 4s）。
const ASYNC_HTTP_RETRY_ATTEMPTS: u32 = 3;

fn retry_backoff(attempts: u32) -> Duration {
    Duration::from_millis((500u64 * 2u64.pow(attempts.min(3))).min(4000))
}

/// 带瞬态重试的 GET JSON。
///
/// 连接失败 / 超时 / 请求阶段错误 / 5xx / 429 视为瞬态：指数退避重试，最多
/// `ASYNC_HTTP_RETRY_ATTEMPTS` 次且不晚于 `deadline`（GET 幂等，重试安全）。
/// 4xx 与确定性错误立即返回；`api_key` 为 Some 时附带 Bearer 头。
async fn get_json_with_retry(
    client: reqwest::Client,
    url: reqwest::Url,
    api_key: Option<&str>,
    deadline: Instant,
    operation: &'static str,
) -> Result<Value> {
    let mut attempts: u32 = 0;
    loop {
        let mut request = client.get(url.clone()).timeout(Duration::from_secs(30));
        if let Some(key) = api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => {
                return response
                    .json()
                    .await
                    .with_context(|| format!("parse {operation} response"));
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let transient =
                    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                if !transient || attempts >= ASYNC_HTTP_RETRY_ATTEMPTS || Instant::now() >= deadline
                {
                    anyhow::bail!("{operation} error {status}: {body}");
                }
                attempts += 1;
                tokio::time::sleep(retry_backoff(attempts)).await;
            }
            Err(err) => {
                let transient = err.is_timeout() || err.is_connect() || err.is_request();
                if !transient || attempts >= ASYNC_HTTP_RETRY_ATTEMPTS || Instant::now() >= deadline
                {
                    return Err(err).with_context(|| format!("{operation} request failed"));
                }
                attempts += 1;
                tokio::time::sleep(retry_backoff(attempts)).await;
            }
        }
    }
}

async fn ensure_success(response: reqwest::Response, operation: &str) -> Result<()> {
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    anyhow::bail!("{operation} error {status}: {body}")
}

async fn response_json(response: reqwest::Response, operation: &str) -> Result<Value> {
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("{operation} error {status}: {body}");
    }
    response
        .json()
        .await
        .with_context(|| format!("parse {operation} response"))
}

impl crate::recorder::AudioConsumer for DashScopeMultimodalASR {
    fn consume_pcm_chunk(&self, pcm: &[u8]) {
        self.buffer.lock().extend_from_slice(pcm);
    }
}

/// 归一化到 multimodal-generation 的完整 endpoint。
///
/// preset 默认下发的就是完整地址，命中首个分支直接用；用户若只填了业务空间
/// 专属域名根（`https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com`）则补上标准
/// 路径。其余情况保守地把标准后缀拼到用户给的路径后面。
pub fn generation_url(base_url: &str) -> Result<String> {
    const CANONICAL_PATH: &str = "/api/v1/services/aigc/multimodal-generation/generation";
    let trimmed = base_url.trim();
    let parsed = reqwest::Url::parse(trimmed).context("parse DashScope base URL")?;
    let path = parsed.path().trim_end_matches('/');
    if path.ends_with("/multimodal-generation/generation") {
        let mut url = parsed.clone();
        url.set_path(path);
        return Ok(url.to_string());
    }
    let mut url = parsed.clone();
    if path.is_empty() {
        url.set_path(CANONICAL_PATH);
    } else {
        url.set_path(&format!("{path}{CANONICAL_PATH}"));
    }
    Ok(url.to_string())
}

pub fn async_transcription_url(base_url: &str) -> Result<String> {
    Ok(api_url(base_url, "/api/v1/services/audio/asr/transcription")?.to_string())
}

pub fn dashscope_multimodal_body(model: &str, wav: &[u8]) -> Value {
    let audio_data = format!(
        "data:audio/wav;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(wav)
    );
    dashscope_multimodal_body_from_uri(model, &audio_data)
}

pub fn dashscope_multimodal_body_from_uri(model: &str, audio_uri: &str) -> Value {
    if is_qwen_sync_model(model) {
        return serde_json::json!({
            "model": model,
            "input": {
                "messages": [{
                    "role": "user",
                    "content": [{ "audio": audio_uri }],
                }],
            },
        });
    }
    // qwen-audio-3.0-asr-flash 还支持 vocabulary 与 language_hints；当前批量客户端
    // 尚未将这两项设置映射到请求体，暂时保持自动语言检测且不传热词。
    serde_json::json!({
        "model": model,
        "input": {
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "input_audio",
                    "input_audio": { "data": audio_uri },
                }],
            }],
        },
        "parameters": {
            "format": "wav",
            "sample_rate": "16000",
        },
    })
}

pub fn async_transcription_body_with_options(
    model: &str,
    file_url: &str,
    options: DashScopeAsyncRequestOptions,
) -> Value {
    let mut parameters = serde_json::Map::new();
    parameters.insert(
        "diarization_enabled".to_string(),
        Value::Bool(options.diarization_enabled),
    );
    if let Some(speaker_count) = options.speaker_count {
        parameters.insert("speaker_count".to_string(), Value::from(speaker_count));
    }
    serde_json::json!({
        "model": model,
        "input": { "file_urls": [file_url] },
        "parameters": parameters,
    })
}

pub fn extract_async_result_url(json: &Value) -> Result<String> {
    let url = json
        .pointer("/output/results/0/transcription_url")
        .and_then(Value::as_str);
    url.map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToOwned::to_owned)
        .context("DashScope async ASR response missing transcription_url")
}

pub fn extract_async_transcript_for_model(
    model: &str,
    json: &Value,
) -> Result<DashScopeAsyncTranscript> {
    match model.trim() {
        "fun-asr" => extract_fun_asr_transcript(json),
        "paraformer-v2" => extract_paraformer_v2_transcript(json),
        other if protocol_for_model(other) == Some(DashScopeBatchProtocol::AsyncTranscription) => {
            extract_async_transcript(json)
        }
        other => anyhow::bail!("unsupported structured DashScope ASR model: {other}"),
    }
}

fn extract_fun_asr_transcript(json: &Value) -> Result<DashScopeAsyncTranscript> {
    extract_async_transcript(json).context("parse fun-asr transcription result")
}

fn extract_paraformer_v2_transcript(json: &Value) -> Result<DashScopeAsyncTranscript> {
    extract_async_transcript(json).context("parse paraformer-v2 transcription result")
}

pub fn extract_async_transcript(json: &Value) -> Result<DashScopeAsyncTranscript> {
    let transcripts = json
        .get("transcripts")
        .context("DashScope async ASR result missing transcripts")?
        .as_array()
        .context("DashScope async ASR transcripts must be an array")?;
    let mut parsed = Vec::new();
    for transcript in transcripts {
        if let Some(sentences) = transcript.get("sentences") {
            let sentences = sentences
                .as_array()
                .context("DashScope async ASR sentences must be an array")?;
            for sentence in sentences {
                let text = required_trimmed_string(sentence, "text")?;
                let begin_time_ms = required_u64(sentence, "begin_time")?;
                let end_time_ms = required_u64(sentence, "end_time")?;
                if end_time_ms < begin_time_ms {
                    anyhow::bail!("DashScope ASR sentence has invalid time range");
                }
                parsed.push(DashScopeAsyncSentence {
                    begin_time_ms,
                    end_time_ms,
                    text,
                    sentence_id: optional_scalar_string(sentence.get("sentence_id"))?,
                    speaker_id: optional_scalar_string(sentence.get("speaker_id"))?,
                });
            }
            continue;
        }
        if let Some(text) = transcript.get("text") {
            let text = text
                .as_str()
                .context("DashScope async ASR transcript text must be a string")?
                .trim();
            if text.is_empty() {
                continue;
            }
            let duration = transcript
                .get("content_duration_in_milliseconds")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            parsed.push(DashScopeAsyncSentence {
                begin_time_ms: 0,
                end_time_ms: duration,
                text: text.to_string(),
                sentence_id: None,
                speaker_id: None,
            });
            continue;
        }
        anyhow::bail!("DashScope async ASR transcript missing text or sentences");
    }
    if parsed.is_empty() {
        anyhow::bail!("DashScope async ASR result contains no transcript text");
    }
    parsed.sort_by_key(|sentence| sentence.begin_time_ms);
    Ok(DashScopeAsyncTranscript { sentences: parsed })
}

fn required_trimmed_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .with_context(|| format!("DashScope async ASR sentence missing {field}"))
}

fn required_u64(value: &Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("DashScope async ASR sentence missing {field}"))
}

fn optional_scalar_string(value: Option<&Value>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    if let Some(value) = value.as_str() {
        let value = value.trim();
        return Ok((!value.is_empty()).then(|| value.to_string()));
    }
    if let Some(value) = value.as_u64() {
        return Ok(Some(value.to_string()));
    }
    if let Some(value) = value.as_i64() {
        return Ok(Some(value.to_string()));
    }
    anyhow::bail!("DashScope async ASR scalar identifier has invalid type")
}

/// fun-asr-flash 的响应信封与标准多模态接口不同，且不同模型版本字段路径略有
/// 差异（`output.text` / `output.output.sentence.text` / 标准 `choices`）。
/// 这里按已知路径逐一兜底提取，取到第一个非空文本即返回，避免因单一路径假设
/// 而在某个版本上静默丢字。
pub fn extract_dashscope_text(json: &Value) -> String {
    let output = json.get("output");

    // 1) output.text —— fun-asr-flash 文档主路径
    if let Some(text) = output.and_then(|o| o.get("text")).and_then(Value::as_str) {
        if !text.trim().is_empty() {
            return text.trim().to_string();
        }
    }

    // 2) output.output.sentence.text —— 文档给出的另一种嵌套形态
    if let Some(text) = output
        .and_then(|o| o.get("output"))
        .and_then(|o| o.get("sentence"))
        .and_then(|s| s.get("text"))
        .and_then(Value::as_str)
    {
        if !text.trim().is_empty() {
            return text.trim().to_string();
        }
    }

    // 3) output.sentence.text
    if let Some(text) = output
        .and_then(|o| o.get("sentence"))
        .and_then(|s| s.get("text"))
        .and_then(Value::as_str)
    {
        if !text.trim().is_empty() {
            return text.trim().to_string();
        }
    }

    // 4) 标准多模态 output.choices[0].message.content（字符串或 [{text}] 数组）
    if let Some(content) = output
        .and_then(|o| o.get("choices"))
        .and_then(|c| c.as_array())
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
    {
        if let Some(text) = content.as_str() {
            return text.trim().to_string();
        }
        if let Some(items) = content.as_array() {
            return items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
                .trim()
                .to_string();
        }
    }

    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::AudioConsumer;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn generation_url_from_full_endpoint_is_unchanged() {
        assert_eq!(generation_url(DEFAULT_ENDPOINT).unwrap(), DEFAULT_ENDPOINT);
        assert_eq!(
            generation_url("https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation/").unwrap(),
            DEFAULT_ENDPOINT
        );
    }

    #[test]
    fn generation_url_from_workspace_host_gets_canonical_path() {
        assert_eq!(
            generation_url("https://ws-xxx.cn-beijing.maas.aliyuncs.com").unwrap(),
            "https://ws-xxx.cn-beijing.maas.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation"
        );
    }

    #[test]
    fn body_uses_multimodal_generation_shape() {
        for model in [DEFAULT_MODEL, QWEN_AUDIO_MODEL] {
            let body = dashscope_multimodal_body(model, b"wav");
            assert_eq!(body["model"], model);
            let audio = &body["input"]["messages"][0]["content"][0];
            assert_eq!(audio["type"], "input_audio");
            assert!(audio["input_audio"]["data"]
                .as_str()
                .unwrap()
                .starts_with("data:audio/wav;base64,"));
            assert_eq!(body["parameters"]["format"], "wav");
            assert_eq!(body["parameters"]["sample_rate"], "16000");
            assert!(body["parameters"].get("vocabulary_id").is_none());
        }
    }

    #[test]
    fn qwen_flash_body_uses_documented_audio_shape() {
        let body = dashscope_multimodal_body("qwen3-asr-flash", b"wav");
        assert_eq!(body["model"], "qwen3-asr-flash");
        let audio = &body["input"]["messages"][0]["content"][0];
        assert!(audio["audio"]
            .as_str()
            .unwrap()
            .starts_with("data:audio/wav;base64,"));
        assert!(audio.get("input_audio").is_none());
    }

    #[test]
    fn classifies_supported_batch_model_protocols() {
        assert_eq!(
            protocol_for_model("fun-asr-flash-2026-06-15"),
            Some(DashScopeBatchProtocol::Multimodal)
        );
        assert_eq!(
            protocol_for_model("qwen3-asr-flash-2026-02-10"),
            Some(DashScopeBatchProtocol::Multimodal)
        );
        // beta 合并：#876 引入的 qwen-audio-3.0-asr-flash 走同步 multimodal。
        assert_eq!(
            protocol_for_model("qwen-audio-3.0-asr-flash"),
            Some(DashScopeBatchProtocol::Multimodal)
        );
        for model in ["fun-asr", "fun-asr-mtl-2025-08-25", "paraformer-v2"] {
            assert_eq!(
                protocol_for_model(model),
                Some(DashScopeBatchProtocol::AsyncTranscription),
                "unexpected protocol for {model}"
            );
        }
        // qwen3-asr-flash-filetrans 仅接受公网 URL，与本地录音的临时 OSS 链路
        // 不兼容：显式拒绝，不得路由到异步协议。
        assert_eq!(
            protocol_for_model("qwen3-asr-flash-filetrans-2025-11-17"),
            None
        );
        assert_eq!(protocol_for_model("unknown-asr"), None);
    }

    #[test]
    fn async_models_get_a_task_polling_timeout() {
        let asr = DashScopeMultimodalASR::new(
            "sk-test".to_string(),
            ASYNC_DEFAULT_ENDPOINT.to_string(),
            "fun-asr".to_string(),
        );
        assert!(asr.transcribe_timeout(1.0) >= Duration::from_secs(660));
        assert!(asr.transcribe_timeout(1_800.0) >= Duration::from_secs(1_500));
        assert!(asr.transcribe_timeout(1_800.0) > asr.transcribe_timeout(1.0));
        assert!(async_upload_timeout(58_000_000) >= Duration::from_secs(900));
    }

    #[test]
    fn async_body_uses_file_urls_input_shape() {
        let funasr = async_transcription_body_with_options(
            "fun-asr",
            "oss://bucket/test.wav",
            DashScopeAsyncRequestOptions {
                diarization_enabled: false,
                speaker_count: None,
            },
        );
        assert_eq!(funasr["input"]["file_urls"][0], "oss://bucket/test.wav");
        assert!(funasr["input"].get("file_url").is_none());
        assert_eq!(funasr["parameters"]["diarization_enabled"], false);
        assert!(funasr["parameters"].get("speaker_count").is_none());
    }

    #[test]
    fn async_body_enables_diarization_and_valid_speaker_hint() {
        let body = async_transcription_body_with_options(
            "paraformer-v2",
            "oss://bucket/test.wav",
            DashScopeAsyncRequestOptions {
                diarization_enabled: true,
                speaker_count: Some(4),
            },
        );
        assert_eq!(body["parameters"]["diarization_enabled"], true);
        assert_eq!(body["parameters"]["speaker_count"], 4);
        assert!(DashScopeAsyncRequestOptions {
            diarization_enabled: true,
            speaker_count: Some(1),
        }
        .validate()
        .is_err());
    }

    #[test]
    fn extracts_async_result_url_from_results_array() {
        let json = serde_json::json!({
            "output": {"results": [{
                "subtask_status": "SUCCEEDED",
                "transcription_url": "https://result.example/funasr.json"
            }]}
        });
        assert_eq!(
            extract_async_result_url(&json).unwrap(),
            "https://result.example/funasr.json"
        );
    }

    #[test]
    fn extracts_structured_text_from_async_result_documents() {
        let funasr = serde_json::json!({
            "transcripts": [{"sentences": [
                {"begin_time": 0, "end_time": 500, "text": "第一句"},
                {"begin_time": 500, "end_time": 1000, "text": "第二句"}
            ]}]
        });
        let parsed = extract_async_transcript(&funasr).unwrap();
        assert_eq!(
            parsed
                .sentences
                .iter()
                .map(|sentence| sentence.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            "第一句 第二句"
        );

        let qwen = serde_json::json!({
            "transcripts": [{"text": "Qwen 转写结果"}]
        });
        assert_eq!(
            extract_async_transcript(&qwen).unwrap().sentences[0].text,
            "Qwen 转写结果"
        );
    }

    #[test]
    fn model_specific_parsers_keep_timestamps_and_speaker_ids() {
        let result = serde_json::json!({
            "transcripts": [{"sentences": [{
                "begin_time": 100,
                "end_time": 900,
                "text": "第一句",
                "sentence_id": 1,
                "speaker_id": 0
            }, {
                "begin_time": 900,
                "end_time": 1700,
                "text": "第二句",
                "sentence_id": "2",
                "speaker_id": "1"
            }]}]
        });
        for model in ["fun-asr", "paraformer-v2"] {
            let parsed = extract_async_transcript_for_model(model, &result).unwrap();
            assert_eq!(parsed.sentences.len(), 2);
            assert_eq!(parsed.sentences[0].begin_time_ms, 100);
            assert_eq!(parsed.sentences[0].speaker_id.as_deref(), Some("0"));
            assert_eq!(parsed.sentences[1].sentence_id.as_deref(), Some("2"));
        }
    }

    #[test]
    fn model_specific_parsers_reject_missing_sentence_timestamps() {
        let result = serde_json::json!({
            "transcripts": [{"sentences": [{"text": "没有时间戳"}]}]
        });
        for model in ["fun-asr", "paraformer-v2"] {
            assert!(extract_async_transcript_for_model(model, &result).is_err());
        }
    }

    #[test]
    fn rejects_malformed_async_result_documents() {
        assert!(extract_async_transcript(&serde_json::json!({})).is_err());
        assert!(extract_async_transcript(&serde_json::json!({
            "transcripts": [{"unexpected": "shape"}]
        }))
        .is_err());
    }

    #[test]
    fn validates_dashscope_transfer_urls() {
        let upgraded =
            dashscope_transfer_url("http://dashscope-file.oss-cn-beijing.aliyuncs.com/result.json")
                .unwrap();
        assert_eq!(upgraded.scheme(), "https");
        assert!(dashscope_transfer_url("http://169.254.169.254/latest/meta-data").is_err());
        assert!(dashscope_transfer_url("https://aliyuncs.com.evil.example/result.json").is_err());
    }

    #[tokio::test]
    async fn get_json_retries_transient_5xx_then_succeeds() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_hits = Arc::clone(&hits);
        let server = tokio::spawn(async move {
            for expected_status in [503_u16, 200] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).await.unwrap();
                server_hits.fetch_add(1, Ordering::SeqCst);
                let (status_text, body) = if expected_status == 503 {
                    ("Service Unavailable", "retry me")
                } else {
                    ("OK", "{\"ok\":true}")
                };
                let response = format!(
                    "HTTP/1.1 {expected_status} {status_text}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let value = get_json_with_retry(
            crate::net::credential_http(),
            format!("http://{addr}/poll").parse().unwrap(),
            None,
            Instant::now() + Duration::from_secs(10),
            "test poll",
        )
        .await
        .unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        server.await.unwrap();
    }

    #[test]
    fn extract_text_prefers_output_text() {
        let json = serde_json::json!({ "output": { "text": "  你好世界  " } });
        assert_eq!(extract_dashscope_text(&json), "你好世界");
    }

    #[test]
    fn extract_text_falls_back_to_nested_sentence() {
        let json = serde_json::json!({
            "output": { "output": { "sentence": { "text": "嵌套句" } } }
        });
        assert_eq!(extract_dashscope_text(&json), "嵌套句");
    }

    #[test]
    fn extract_text_falls_back_to_choices_content_array() {
        let json = serde_json::json!({
            "output": {
                "choices": [{
                    "message": { "content": [{ "text": "第一段" }, { "text": "第二段" }] }
                }]
            }
        });
        assert_eq!(extract_dashscope_text(&json), "第一段第二段");
    }

    #[test]
    fn extract_text_empty_when_no_known_path() {
        let json = serde_json::json!({ "request_id": "abc", "output": {} });
        assert_eq!(extract_dashscope_text(&json), "");
    }

    #[tokio::test]
    async fn posts_multimodal_generation_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "timed out waiting for DashScope ASR test request"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => panic!("accept DashScope ASR test request failed: {err}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_http_request(&mut stream);
            let request_text = String::from_utf8_lossy(&request);
            let lower = request_text.to_ascii_lowercase();
            assert!(request_text.starts_with(
                "POST /api/v1/services/aigc/multimodal-generation/generation HTTP/1.1"
            ));
            assert!(lower.contains("authorization: bearer sk-test"));
            assert!(lower.contains("content-type: application/json"));
            assert!(request_text.contains(r#""model":"fun-asr-flash-2026-06-15""#));
            assert!(request_text.contains(r#""type":"input_audio""#));
            assert!(request_text.contains("data:audio/wav;base64,"));
            assert!(!request_text.contains("vocabulary_id"));
            write_json_response(
                &mut stream,
                r#"{"output":{"text":"你好百炼"},"request_id":"r1"}"#,
            );
        });

        let asr = DashScopeMultimodalASR::new(
            "sk-test".to_string(),
            format!(
                "http://{}/api/v1/services/aigc/multimodal-generation/generation",
                addr
            ),
            DEFAULT_MODEL.to_string(),
        );
        asr.consume_pcm_chunk(&vec![0u8; 32_000]);
        assert_eq!(asr.buffer_duration_ms(), 1_000);
        let transcript = asr.transcribe().await.unwrap();

        assert_eq!(transcript.text, "你好百炼");
        assert_eq!(transcript.duration_ms, 1_000);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn uploads_and_polls_async_transcription() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for step in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_http_request(&mut stream);
                let request_text = String::from_utf8_lossy(&request);
                let lower = request_text.to_ascii_lowercase();
                if matches!(step, 0 | 2 | 3) {
                    assert!(lower.contains("authorization: bearer sk-test"));
                } else {
                    assert!(!lower.contains("authorization:"));
                }
                match step {
                    0 => {
                        assert!(request_text.starts_with(
                            "GET /api/v1/uploads?action=getPolicy&model=fun-asr HTTP/1.1"
                        ));
                        write_json_response(
                            &mut stream,
                            &format!(
                                r#"{{"data":{{"policy":"policy","signature":"signature","upload_dir":"dashscope-instant/test","upload_host":"http://{addr}","oss_access_key_id":"key-id","x_oss_object_acl":"private","x_oss_forbid_overwrite":"true"}}}}"#
                            ),
                        );
                    }
                    1 => {
                        assert!(request_text.starts_with("POST / HTTP/1.1"));
                        assert!(lower.contains("content-type: multipart/form-data"));
                        assert!(request_text.contains("OSSAccessKeyId"));
                        assert!(request_text.contains("success_action_status"));
                        assert!(request_text.contains("audio.wav"));
                        write_json_response(&mut stream, "{}");
                    }
                    2 => {
                        assert!(request_text
                            .starts_with("POST /api/v1/services/audio/asr/transcription HTTP/1.1"));
                        assert!(lower.contains("x-dashscope-async: enable"));
                        assert!(lower.contains("x-dashscope-ossresourceresolve: enable"));
                        assert!(request_text
                            .contains(r#""file_urls":["oss://dashscope-instant/test/audio.wav"]"#));
                        write_json_response(&mut stream, r#"{"output":{"task_id":"task-1"}}"#);
                    }
                    3 => {
                        assert!(request_text.starts_with("GET /api/v1/tasks/task-1 HTTP/1.1"));
                        write_json_response(
                            &mut stream,
                            &format!(
                                r#"{{"output":{{"task_status":"SUCCEEDED","results":[{{"subtask_status":"SUCCEEDED","transcription_url":"http://{addr}/result.json"}}]}}}}"#
                            ),
                        );
                    }
                    4 => {
                        assert!(request_text.starts_with("GET /result.json HTTP/1.1"));
                        write_json_response(
                            &mut stream,
                            r#"{"transcripts":[{"sentences":[{"begin_time":0,"end_time":500,"sentence_id":1,"text":"异步"},{"begin_time":500,"end_time":1000,"sentence_id":2,"text":"转写"}]}]}"#,
                        );
                    }
                    _ => unreachable!(),
                }
            }
        });

        let asr = DashScopeMultimodalASR::new(
            "sk-test".to_string(),
            format!("http://{addr}/api/v1/services/audio/asr/transcription"),
            "fun-asr".to_string(),
        );
        asr.consume_pcm_chunk(&vec![0u8; 32_000]);
        let transcript = asr.transcribe().await.unwrap();
        assert_eq!(transcript.text, "异步 转写");
        assert_eq!(transcript.duration_ms, 1_000);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn streams_segmented_meeting_wav_into_one_multipart_file() {
        let dir = std::env::temp_dir().join(format!("dashscope-meeting-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("part-0001.wav"),
            crate::asr::wav::encode_wav_16k_mono(&[1, 2]),
        )
        .unwrap();
        std::fs::write(
            dir.join("part-0002.wav"),
            crate::asr::wav::encode_wav_16k_mono(&[3, 4]),
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for step in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_http_request(&mut stream);
                let request_text = String::from_utf8_lossy(&request);
                match step {
                    0 => {
                        assert!(request_text.starts_with(
                            "GET /api/v1/uploads?action=getPolicy&model=fun-asr HTTP/1.1"
                        ));
                        write_json_response(
                            &mut stream,
                            &format!(
                                r#"{{"data":{{"policy":"policy","signature":"signature","upload_dir":"dashscope-instant/meeting","upload_host":"http://{addr}","oss_access_key_id":"key-id","x_oss_object_acl":"private","x_oss_forbid_overwrite":"true"}}}}"#
                            ),
                        );
                    }
                    1 => {
                        assert!(request_text.starts_with("POST / HTTP/1.1"));
                        assert!(request_text
                            .to_ascii_lowercase()
                            .contains("content-type: multipart/form-data"));
                        let riff_positions = request
                            .windows(4)
                            .enumerate()
                            .filter_map(|(index, window)| (window == b"RIFF").then_some(index))
                            .collect::<Vec<_>>();
                        assert_eq!(riff_positions.len(), 1);
                        let expected = crate::asr::wav::encode_wav_16k_mono(&[1, 2, 3, 4]);
                        let start = riff_positions[0];
                        assert_eq!(&request[start..start + expected.len()], expected.as_slice());
                        write_json_response(&mut stream, "{}");
                    }
                    _ => unreachable!(),
                }
            }
        });

        let asr = DashScopeMultimodalASR::new(
            "sk-test".to_string(),
            format!("http://{addr}/api/v1/services/audio/asr/transcription"),
            "fun-asr".to_string(),
        );
        let source = MeetingAudioSource::from_path(&dir).unwrap();
        let file_url = asr
            .upload_meeting_audio(source, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();

        assert_eq!(file_url, "oss://dashscope-instant/meeting/audio.wav");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn cancels_pending_async_task_with_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_http_request(&mut stream);
            let request_text = String::from_utf8_lossy(&request);
            assert!(request_text.starts_with("POST /api/v1/tasks/task-1/cancel HTTP/1.1"));
            assert!(request_text
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-test"));
            write_json_response(&mut stream, r#"{"request_id":"request-1"}"#);
        });

        let asr = DashScopeMultimodalASR::new(
            "sk-test".to_string(),
            format!("http://{addr}/api/v1/services/audio/asr/transcription"),
            "fun-asr".to_string(),
        );
        asr.cancel_async_task("task-1").await.unwrap();
        server.join().unwrap();
    }

    #[tokio::test]
    async fn async_policy_request_does_not_follow_redirects_with_credentials() {
        let redirect_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect_addr = redirect_listener.local_addr().unwrap();
        let target_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        target_listener.set_nonblocking(true).unwrap();
        let target_addr = target_listener.local_addr().unwrap();
        let followed = Arc::new(AtomicBool::new(false));
        let target_followed = Arc::clone(&followed);

        let redirect_server = thread::spawn(move || {
            let (mut stream, _) = redirect_listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(String::from_utf8_lossy(&request)
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-test"));
            let response = format!(
                "HTTP/1.1 302 Found\r\nlocation: http://{target_addr}/api/v1/uploads\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        let target_server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match target_listener.accept() {
                    Ok((mut stream, _)) => {
                        target_followed.store(true, Ordering::SeqCst);
                        let _request = read_http_request(&mut stream);
                        let body = r#"{"message":"redirect followed"}"#;
                        let response = format!(
                            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        stream.write_all(response.as_bytes()).unwrap();
                        stream.flush().unwrap();
                        break;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            break;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => panic!("accept redirect target request failed: {err}"),
                }
            }
        });

        let asr = DashScopeMultimodalASR::new(
            "sk-test".to_string(),
            format!("http://{redirect_addr}/api/v1/services/audio/asr/transcription"),
            "fun-asr".to_string(),
        );
        asr.consume_pcm_chunk(&vec![0u8; 32_000]);
        let error = asr.transcribe().await.unwrap_err().to_string();

        assert!(error.contains("302 Found"), "unexpected error: {error}");
        redirect_server.join().unwrap();
        target_server.join().unwrap();
        assert!(!followed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn async_result_download_does_not_follow_redirects() {
        let redirect_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect_addr = redirect_listener.local_addr().unwrap();
        let target_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        target_listener.set_nonblocking(true).unwrap();
        let target_addr = target_listener.local_addr().unwrap();
        let followed = Arc::new(AtomicBool::new(false));
        let target_followed = Arc::clone(&followed);
        let redirect_server = thread::spawn(move || {
            let (mut stream, _) = redirect_listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(!String::from_utf8_lossy(&request)
                .to_ascii_lowercase()
                .contains("authorization:"));
            let response = format!(
                "HTTP/1.1 302 Found\r\nlocation: http://{target_addr}/result.json\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        let target_server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match target_listener.accept() {
                    Ok((_stream, _)) => {
                        target_followed.store(true, Ordering::SeqCst);
                        break;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => panic!("accept result redirect target failed: {err}"),
                }
            }
        });

        let error = download_async_result(&format!("http://{redirect_addr}/result.json"))
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("302 Found"), "unexpected error: {error}");
        redirect_server.join().unwrap();
        target_server.join().unwrap();
        assert!(!followed.load(Ordering::SeqCst));
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        let mut expected_len = None;
        loop {
            let read = stream.read(&mut buf).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buf[..read]);
            if expected_len.is_none() {
                expected_len = parse_expected_request_len(&request);
            }
            if expected_len.is_some_and(|len| request.len() >= len) {
                break;
            }
        }
        request
    }

    fn parse_expected_request_len(request: &[u8]) -> Option<usize> {
        let header_end = request.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_len = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        Some(header_end + content_len)
    }

    fn write_json_response(stream: &mut std::net::TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.flush().unwrap();
    }
}
