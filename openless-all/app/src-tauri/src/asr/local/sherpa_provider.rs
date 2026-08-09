//! sherpa-onnx 本地 ASR provider（Windows offline batch + online streaming）。
//!
//! 形状与 `foundry_provider.rs` 对齐：
//! - 作为 `Recorder::AudioConsumer` 持续吃 PCM
//! - 录音结束后 `transcribe(timeout)` 返回 `RawTranscript`
//! - `cancel()` 让任何 in-flight transcription 提前结束，并清理已缓存 PCM
//!
//! Offline 模型通过有界队列把 16kHz mono s16le PCM 持续写入临时文件，停止录音后
//! 由 runtime 固定分片读取。Online 模型在独立 worker 中实时消费 PCM，partial token
//! 通过回调上抛，停止录音后返回 final `RawTranscript`。

use std::fs::{File, OpenOptions};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use crate::asr::RawTranscript;

use super::sherpa;
use super::sherpa_runtime::{SherpaOnlineSession, SherpaOnnxRuntime};

pub struct SherpaOnnxAsr {
    runtime: Arc<SherpaOnnxRuntime>,
    model_alias: String,
    language_hint: Option<String>,
    mode: SherpaProviderMode,
    cancel_generation: AtomicU64,
}

enum SherpaProviderMode {
    Offline {
        spool: Mutex<Option<OfflinePcmSpool>>,
    },
    Online {
        worker: Mutex<Option<OnlineWorker>>,
    },
}

const OFFLINE_PCM_QUEUE_CAPACITY: usize = 256;
const ONLINE_PCM_QUEUE_CAPACITY: usize = 256;

struct OfflinePcmSpool {
    tx: Option<SyncSender<OfflineSpoolMessage>>,
    result_rx: Option<Receiver<Result<OfflinePcmFile>>>,
    join_handle: Option<JoinHandle<()>>,
    audio_bytes: AtomicU64,
    error: Arc<Mutex<Option<String>>>,
    cancelled: Arc<AtomicBool>,
}

struct OfflinePcmFile {
    file: File,
    bytes: u64,
}

enum OfflineSpoolMessage {
    Pcm(Vec<u8>),
    Finish,
}

struct OnlineWorker {
    tx: SyncSender<OnlineWorkerMessage>,
    result_rx: Mutex<Option<Receiver<Result<String>>>>,
    join_handle: Mutex<Option<JoinHandle<()>>>,
    audio_bytes: AtomicU64,
    error: Mutex<Option<String>>,
    cancelled: Arc<AtomicBool>,
}

enum OnlineWorkerMessage {
    Pcm(Vec<u8>),
    Finish,
    Cancel,
}

pub type SherpaTokenHandler = Arc<dyn Fn(String) + Send + Sync + 'static>;

impl SherpaOnnxAsr {
    pub fn new(
        runtime: Arc<SherpaOnnxRuntime>,
        model_alias: String,
        language_hint: Option<String>,
    ) -> Result<Self> {
        Ok(Self {
            runtime,
            model_alias,
            language_hint: normalize_language_hint(language_hint),
            mode: SherpaProviderMode::Offline {
                spool: Mutex::new(Some(OfflinePcmSpool::spawn()?)),
            },
            cancel_generation: AtomicU64::new(0),
        })
    }

    pub async fn new_for_model(
        runtime: Arc<SherpaOnnxRuntime>,
        model_alias: String,
        language_hint: Option<String>,
        token_handler: Option<SherpaTokenHandler>,
    ) -> Result<Self> {
        if sherpa::alias_is_online(&model_alias) {
            let session = runtime.create_online_session(&model_alias).await?;
            Ok(Self {
                runtime,
                model_alias,
                language_hint: normalize_language_hint(language_hint),
                mode: SherpaProviderMode::Online {
                    worker: Mutex::new(Some(OnlineWorker::spawn(session, token_handler))),
                },
                cancel_generation: AtomicU64::new(0),
            })
        } else {
            Self::new(runtime, model_alias, language_hint)
        }
    }

    #[allow(dead_code)]
    pub fn model_alias(&self) -> &str {
        &self.model_alias
    }

    #[allow(dead_code)]
    pub fn language_hint(&self) -> Option<&str> {
        self.language_hint.as_deref()
    }

    /// 当前缓冲音频时长（毫秒）。Offline 读 PCM buffer；Online 读 worker 已接收
    /// 的 PCM 字节数。不消费缓冲。
    pub fn buffer_duration_ms(&self) -> u64 {
        match &self.mode {
            SherpaProviderMode::Offline { spool } => spool
                .lock()
                .as_ref()
                .map(|spool| pcm_duration_ms_from_bytes(spool.audio_bytes.load(Ordering::SeqCst)))
                .unwrap_or(0),
            SherpaProviderMode::Online { worker } => worker
                .lock()
                .as_ref()
                .map(|worker| pcm_duration_ms_from_bytes(worker.audio_bytes.load(Ordering::SeqCst)))
                .unwrap_or(0),
        }
    }

    pub async fn transcribe(&self, audio_timeout: Duration) -> Result<RawTranscript> {
        match &self.mode {
            SherpaProviderMode::Offline { spool } => {
                self.transcribe_offline(spool, audio_timeout).await
            }
            SherpaProviderMode::Online { worker } => {
                self.transcribe_online(worker, audio_timeout).await
            }
        }
    }

    async fn transcribe_offline(
        &self,
        spool_slot: &Mutex<Option<OfflinePcmSpool>>,
        audio_timeout: Duration,
    ) -> Result<RawTranscript> {
        let cancel_generation = self.cancel_generation.load(Ordering::SeqCst);
        let Some(spool) = spool_slot.lock().take() else {
            return Ok(RawTranscript {
                text: String::new(),
                duration_ms: 0,
            });
        };
        let pcm_file = spool.finish().await?;
        if pcm_file.bytes == 0 {
            return Ok(RawTranscript {
                text: String::new(),
                duration_ms: 0,
            });
        }

        let duration_ms = pcm_duration_ms_from_bytes(pcm_file.bytes);
        let result = self
            .runtime
            .transcribe_pcm_file(
                &self.model_alias,
                pcm_file.file,
                pcm_file.bytes,
                self.language_hint(),
                audio_timeout,
            )
            .await;

        if self.cancel_generation.load(Ordering::SeqCst) != cancel_generation {
            anyhow::bail!("sherpa-onnx transcription cancelled");
        }

        let text = result?;
        Ok(RawTranscript {
            text: trim_transcript_text(&text),
            duration_ms,
        })
    }

    async fn transcribe_online(
        &self,
        worker_slot: &Mutex<Option<OnlineWorker>>,
        audio_timeout: Duration,
    ) -> Result<RawTranscript> {
        let cancel_generation = self.cancel_generation.load(Ordering::SeqCst);
        let Some(worker) = worker_slot.lock().take() else {
            return Ok(RawTranscript {
                text: String::new(),
                duration_ms: 0,
            });
        };
        let duration_ms = pcm_duration_ms_from_bytes(worker.audio_bytes.load(Ordering::SeqCst));
        let started = Instant::now();
        let result = worker.finish(audio_timeout).await;
        let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        if self.cancel_generation.load(Ordering::SeqCst) != cancel_generation {
            self.runtime.record_streaming_result(
                &self.model_alias,
                duration_ms,
                elapsed_ms,
                Some("sherpa-onnx streaming transcription cancelled".into()),
            );
            anyhow::bail!("sherpa-onnx streaming transcription cancelled");
        }
        match &result {
            Ok(_) => self.runtime.record_streaming_result(
                &self.model_alias,
                duration_ms,
                elapsed_ms,
                None,
            ),
            Err(error) => self.runtime.record_streaming_result(
                &self.model_alias,
                duration_ms,
                elapsed_ms,
                Some(format!("{error:#}")),
            ),
        }
        let text = result?;
        Ok(RawTranscript {
            text: trim_transcript_text(&text),
            duration_ms,
        })
    }

    pub fn cancel(&self) {
        self.cancel_generation.fetch_add(1, Ordering::SeqCst);
        self.runtime.request_cancel_prepare();
        match &self.mode {
            SherpaProviderMode::Offline { spool } => {
                if let Some(spool) = spool.lock().take() {
                    spool.cancel();
                }
            }
            SherpaProviderMode::Online { worker } => {
                if let Some(worker) = worker.lock().take() {
                    worker.cancel();
                }
            }
        }
    }
}

impl crate::recorder::AudioConsumer for SherpaOnnxAsr {
    fn consume_pcm_chunk(&self, pcm: &[u8]) {
        match &self.mode {
            SherpaProviderMode::Offline { spool } => {
                if let Some(spool) = spool.lock().as_ref() {
                    spool.send_pcm(pcm);
                }
            }
            SherpaProviderMode::Online { worker } => {
                if let Some(worker) = worker.lock().as_ref() {
                    worker.send_pcm(pcm);
                }
            }
        }
    }
}

fn pcm_duration_ms_from_bytes(bytes: u64) -> u64 {
    crate::asr::pcm::pcm_duration_ms_from_bytes(bytes)
}

fn trim_transcript_text(text: &str) -> String {
    text.trim().to_string()
}

fn normalize_language_hint(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
}

impl OfflinePcmSpool {
    fn spawn() -> Result<Self> {
        let file = create_temporary_pcm_file()?;
        let (tx, rx) = mpsc::sync_channel::<OfflineSpoolMessage>(OFFLINE_PCM_QUEUE_CAPACITY);
        let (result_tx, result_rx) = mpsc::channel::<Result<OfflinePcmFile>>();
        let error = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let join_handle = std::thread::Builder::new()
            .name("openless-sherpa-offline-spool".into())
            .spawn(move || {
                use std::io::Write;

                let mut file = file;
                let mut bytes = 0u64;
                let result = loop {
                    if worker_cancelled.load(Ordering::SeqCst) {
                        break Err(anyhow::anyhow!("sherpa-onnx offline spool cancelled"));
                    }
                    match rx.recv() {
                        Ok(OfflineSpoolMessage::Pcm(pcm)) => {
                            if let Err(write_error) = file.write_all(&pcm) {
                                break Err(anyhow::anyhow!(
                                    "write sherpa-onnx offline PCM spool failed: {write_error}"
                                ));
                            }
                            bytes = bytes.saturating_add(pcm.len() as u64);
                        }
                        Ok(OfflineSpoolMessage::Finish) => {
                            if let Err(flush_error) = file.flush() {
                                break Err(anyhow::anyhow!(
                                    "flush sherpa-onnx offline PCM spool failed: {flush_error}"
                                ));
                            }
                            break Ok(OfflinePcmFile { file, bytes });
                        }
                        Err(_) => {
                            break Err(anyhow::anyhow!("sherpa-onnx offline spool channel closed"));
                        }
                    }
                };
                let _ = result_tx.send(result);
            })
            .context("spawn sherpa-onnx offline spool worker")?;

        Ok(Self {
            tx: Some(tx),
            result_rx: Some(result_rx),
            join_handle: Some(join_handle),
            audio_bytes: AtomicU64::new(0),
            error,
            cancelled,
        })
    }

    fn send_pcm(&self, pcm: &[u8]) {
        if pcm.is_empty() || self.cancelled.load(Ordering::SeqCst) || self.error.lock().is_some() {
            return;
        }
        let Some(tx) = self.tx.as_ref() else {
            return;
        };
        match tx.try_send(OfflineSpoolMessage::Pcm(pcm.to_vec())) {
            Ok(()) => {
                self.audio_bytes
                    .fetch_add(pcm.len() as u64, Ordering::SeqCst);
            }
            Err(TrySendError::Full(_)) => self.record_error(
                "sherpa-onnx offline PCM spool is full; transcription was stopped to avoid unbounded memory growth",
            ),
            Err(TrySendError::Disconnected(_)) => {
                self.record_error("sherpa-onnx offline PCM spool worker stopped unexpectedly")
            }
        }
    }

    async fn finish(mut self) -> Result<OfflinePcmFile> {
        let tx = self
            .tx
            .take()
            .context("sherpa-onnx offline spool sender missing")?;
        let result_rx = self
            .result_rx
            .take()
            .context("sherpa-onnx offline spool result receiver missing")?;
        let join_handle = self.join_handle.take();
        let result = tokio::task::spawn_blocking(move || {
            tx.send(OfflineSpoolMessage::Finish)
                .map_err(|_| anyhow::anyhow!("sherpa-onnx offline spool worker stopped"))?;
            drop(tx);
            let result = result_rx
                .recv()
                .map_err(|_| anyhow::anyhow!("sherpa-onnx offline spool returned no result"))?;
            if let Some(join_handle) = join_handle {
                join_handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("sherpa-onnx offline spool worker panicked"))?;
            }
            result
        })
        .await
        .map_err(|error| anyhow::anyhow!("join sherpa-onnx offline spool failed: {error}"))??;

        if let Some(error) = self.error.lock().clone() {
            anyhow::bail!(error);
        }
        Ok(result)
    }

    fn cancel(mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.tx.take();
        self.result_rx.take();
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }

    fn record_error(&self, message: &str) {
        let mut error = self.error.lock();
        if error.is_none() {
            *error = Some(message.to_string());
            log::error!("[sherpa-asr] {message}");
        }
    }
}

impl Drop for OfflinePcmSpool {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.tx.take();
        self.result_rx.take();
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

fn create_temporary_pcm_file() -> Result<File> {
    let dir = std::env::temp_dir().join("OpenLess").join("sherpa-spool");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create sherpa-onnx spool directory {}", dir.display()))?;
    let path = dir.join(format!(
        "{}-{}.pcm",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x0000_0100;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        options.custom_flags(FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("create sherpa-onnx PCM spool {}", path.display()))?;
    #[cfg(not(target_os = "windows"))]
    std::fs::remove_file(&path)
        .with_context(|| format!("unlink sherpa-onnx PCM spool {}", path.display()))?;
    Ok(file)
}

impl OnlineWorker {
    fn spawn(mut session: SherpaOnlineSession, token_handler: Option<SherpaTokenHandler>) -> Self {
        let alias = session.alias().to_string();
        let (tx, rx) = mpsc::sync_channel::<OnlineWorkerMessage>(ONLINE_PCM_QUEUE_CAPACITY);
        let (result_tx, result_rx) = mpsc::channel::<Result<String>>();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let join_handle = std::thread::Builder::new()
            .name(format!("openless-sherpa-online-{alias}"))
            .spawn(move || {
                let emit = |piece: &str| {
                    if piece.is_empty() || worker_cancelled.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Some(handler) = token_handler.as_ref() {
                        handler(piece.to_string());
                    }
                };
                let result = loop {
                    match rx.recv() {
                        Ok(OnlineWorkerMessage::Pcm(pcm)) => {
                            if worker_cancelled.load(Ordering::SeqCst) {
                                break Err(anyhow::anyhow!("sherpa-onnx streaming cancelled"));
                            }
                            if let Err(error) = session.accept_pcm_chunk(&pcm, &emit) {
                                break Err(error);
                            }
                            if worker_cancelled.load(Ordering::SeqCst) {
                                break Err(anyhow::anyhow!("sherpa-onnx streaming cancelled"));
                            }
                        }
                        Ok(OnlineWorkerMessage::Finish) => {
                            if worker_cancelled.load(Ordering::SeqCst) {
                                break Err(anyhow::anyhow!("sherpa-onnx streaming cancelled"));
                            }
                            break session.finish(&emit);
                        }
                        Ok(OnlineWorkerMessage::Cancel) | Err(_) => {
                            worker_cancelled.store(true, Ordering::SeqCst);
                            break Err(anyhow::anyhow!("sherpa-onnx streaming cancelled"));
                        }
                    }
                };
                let _ = result_tx.send(result);
            })
            .expect("spawn sherpa online worker");

        Self {
            tx,
            result_rx: Mutex::new(Some(result_rx)),
            join_handle: Mutex::new(Some(join_handle)),
            audio_bytes: AtomicU64::new(0),
            error: Mutex::new(None),
            cancelled,
        }
    }

    fn send_pcm(&self, pcm: &[u8]) {
        if pcm.is_empty() || self.cancelled.load(Ordering::SeqCst) || self.error.lock().is_some() {
            return;
        }
        match self
            .tx
            .try_send(OnlineWorkerMessage::Pcm(pcm.to_vec()))
        {
            Ok(()) => {
                self.audio_bytes
                    .fetch_add(pcm.len() as u64, Ordering::SeqCst);
            }
            Err(TrySendError::Full(_)) => self.record_error(
                "sherpa-onnx online PCM queue is full; transcription was stopped to avoid unbounded memory growth",
            ),
            Err(TrySendError::Disconnected(_)) => {
                self.record_error("sherpa-onnx online worker stopped unexpectedly")
            }
        }
    }

    async fn finish(self, audio_timeout: Duration) -> Result<String> {
        let result_rx = self
            .result_rx
            .lock()
            .take()
            .ok_or_else(|| anyhow::anyhow!("sherpa-onnx streaming result already taken"))?;
        let join_handle = self.join_handle.lock().take();
        let finish_tx = self.tx.clone();
        let result = tokio::time::timeout(audio_timeout, async move {
            tokio::task::spawn_blocking(move || {
                finish_tx
                    .send(OnlineWorkerMessage::Finish)
                    .map_err(|_| anyhow::anyhow!("sherpa-onnx streaming worker stopped"))?;
                let worker_result = result_rx.recv().map_err(|error| {
                    anyhow::anyhow!("sherpa-onnx streaming worker closed: {error}")
                })?;
                if let Some(join_handle) = join_handle {
                    join_handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("sherpa-onnx streaming worker panicked"))?;
                }
                worker_result
            })
            .await
            .map_err(|error| anyhow::anyhow!("sherpa-onnx streaming join failed: {error:#}"))?
        })
        .await;
        let result = match result {
            Ok(result) => result,
            Err(_) => {
                self.cancelled.store(true, Ordering::SeqCst);
                let _ = self.tx.try_send(OnlineWorkerMessage::Cancel);
                anyhow::bail!("sherpa-onnx streaming transcribe timeout");
            }
        };
        if let Some(error) = self.error.lock().clone() {
            anyhow::bail!(error);
        }
        result
    }

    fn cancel(self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let _ = self.tx.try_send(OnlineWorkerMessage::Cancel);
        if let Some(join_handle) = self.join_handle.lock().take() {
            let _ = join_handle.join();
        }
    }

    fn record_error(&self, message: &str) {
        let mut error = self.error.lock();
        if error.is_none() {
            *error = Some(message.to_string());
            log::error!("[sherpa-asr] {message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::AudioConsumer;

    fn make_provider() -> SherpaOnnxAsr {
        SherpaOnnxAsr::new(
            Arc::new(SherpaOnnxRuntime::new()),
            "sense-voice-small-zh".into(),
            Some("  ZH  ".into()),
        )
        .unwrap()
    }

    #[test]
    fn normalize_language_hint_trims_and_lowercases() {
        let provider = make_provider();
        assert_eq!(provider.language_hint(), Some("zh"));
    }

    #[test]
    fn empty_language_hint_normalizes_to_none() {
        let provider = SherpaOnnxAsr::new(
            Arc::new(SherpaOnnxRuntime::new()),
            "paraformer-zh".into(),
            Some("   ".into()),
        )
        .unwrap();
        assert!(provider.language_hint().is_none());
    }

    #[tokio::test]
    async fn offline_spool_preserves_pcm_without_growing_provider_memory() {
        use std::io::{Read, Seek, SeekFrom};

        let spool = OfflinePcmSpool::spawn().unwrap();
        spool.send_pcm(&[1, 2, 3, 4]);
        spool.send_pcm(&[5, 6]);
        let mut pcm_file = spool.finish().await.unwrap();
        let mut pcm = Vec::new();
        pcm_file.file.seek(SeekFrom::Start(0)).unwrap();
        pcm_file.file.read_to_end(&mut pcm).unwrap();

        assert_eq!(pcm_file.bytes, 6);
        assert_eq!(pcm, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn offline_buffer_duration_reports_16k_pcm_duration_without_consuming() {
        let provider = make_provider();
        provider.consume_pcm_chunk(&vec![0u8; 32_000]);

        assert_eq!(provider.buffer_duration_ms(), 1000);
        match &provider.mode {
            SherpaProviderMode::Offline { spool } => assert_eq!(
                spool
                    .lock()
                    .as_ref()
                    .map(|spool| spool.audio_bytes.load(Ordering::SeqCst)),
                Some(32_000)
            ),
            SherpaProviderMode::Online { .. } => panic!("expected offline provider"),
        }
    }

    #[tokio::test]
    async fn empty_spool_transcribe_returns_empty_transcript() {
        let provider = make_provider();
        let result = provider.transcribe(Duration::from_secs(5)).await.unwrap();
        assert!(result.text.is_empty());
        assert_eq!(result.duration_ms, 0);
    }

    #[tokio::test]
    async fn transcribe_consumes_spool_on_runtime_error() {
        let provider = SherpaOnnxAsr::new(
            Arc::new(SherpaOnnxRuntime::new()),
            "unknown-sherpa-model".into(),
            None,
        )
        .unwrap();
        provider.consume_pcm_chunk(&vec![0u8; 32_000]);
        let result = provider.transcribe(Duration::from_secs(5)).await;
        assert!(result.is_err());
        match &provider.mode {
            SherpaProviderMode::Offline { spool } => assert!(spool.lock().is_none()),
            SherpaProviderMode::Online { .. } => panic!("expected offline provider"),
        }
    }

    #[test]
    fn online_queue_overload_is_bounded_and_reported() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let (_result_tx, result_rx) = mpsc::channel();
        let worker = OnlineWorker {
            tx,
            result_rx: Mutex::new(Some(result_rx)),
            join_handle: Mutex::new(None),
            audio_bytes: AtomicU64::new(0),
            error: Mutex::new(None),
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        worker.send_pcm(&[1, 2]);
        worker.send_pcm(&[3, 4]);

        assert_eq!(worker.audio_bytes.load(Ordering::SeqCst), 2);
        assert!(worker
            .error
            .lock()
            .as_deref()
            .unwrap_or_default()
            .contains("queue is full"));
    }

    #[test]
    fn cancel_closes_spool_and_bumps_generation() {
        let runtime = Arc::new(SherpaOnnxRuntime::new());
        let provider = SherpaOnnxAsr::new(
            Arc::clone(&runtime),
            "sense-voice-small-zh".into(),
            Some("  ZH  ".into()),
        )
        .unwrap();
        provider.consume_pcm_chunk(&[1, 2, 3, 4]);
        let before = provider.cancel_generation.load(Ordering::SeqCst);
        provider.cancel();
        let after = provider.cancel_generation.load(Ordering::SeqCst);
        assert!(after > before);
        assert!(runtime.cancel_prepare_requested_for_tests());
        match &provider.mode {
            SherpaProviderMode::Offline { spool } => assert!(spool.lock().is_none()),
            SherpaProviderMode::Online { .. } => panic!("expected offline provider"),
        }
    }
}
