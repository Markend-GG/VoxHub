# 会议录音 V2-1 Realtime ASR Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为会议录音接入 meeting-specific ASR settings（会议专用语音转文字设置）、provider capabilities（服务商能力声明）、`fun-asr-realtime` draft/final（临时/最终）事件和 timestamp metadata（时间戳元数据），让会议中实时可见文字，并为 V2-2 云端 / 本地会后说话人处理保存足够数据。

**Architecture:** V2-1 复用现有 meeting coordinator（会议协调器）、CredentialsVault（凭据存储）、ASR provider（语音转文字服务商）和 event（事件）机制。新增能力只放在会议 ASR 选择、ASR 事件契约、`TranscriptSegment.metadata` 和现有 `bailian` Fun-ASR Realtime provider 增强上，短口述主链路默认行为不变。

**Tech Stack:** Rust、Tauri IPC、React、TypeScript、Alibaba Bailian Fun-ASR Realtime WebSocket、Windows MSVC toolchain（工具链）。

---

状态：implementation present / pending full acceptance（代码主体已存在，待完整验收）
日期：2026-07-08  
进展同步：2026-08-12（Phase 1 自动验证完成）
上级计划：`docs/meeting-recording-v2-plan.md`

## 0. 2026-08-12 进展同步

本计划最初写于 2026-07-08。经只读核对当前代码，V2-1 的主要数据结构和事件链路已经存在，包括：

- `MeetingAsrSettings`（会议 ASR 设置）及偏好持久化。
- `TranscriptSegment.metadata`（原文片段元数据）。
- `MeetingRecordingSnapshot.activeProviderSessionId`（活跃服务商会话标识）。
- ASR draft sink（临时识别回调）与 `meeting:transcript-draft` 事件。
- 会议页 draft 订阅、provider session 防旧事件污染，以及 pause / resume 的 session metadata（会话元数据）传递路径。

因此，本文后续“当前没有 / 需要新增”的表述应统一理解为 **2026-07-08 实施前基线**，不能再作为 2026-08-12 的缺失项清单。Phase 1 已补齐本场实际 realtime ASR 配置持久化、旧记录兼容、resume 模型锁定和设置页 effective provider capability 查询，并通过 TypeScript、前端 build、MSVC `cargo check` 及 meeting / bailian / preferences / credentials 自动测试。当前状态仍标记为 `partial`（部分完成），原因是还没有完成真实百炼会议、暂停 / 继续、多次 session、网络中断、短口述回归和 Tauri UI 人工验收。最新状态以 `docs/meeting-recording-v2-acceptance-checklist.md` 为准；本文任务 checkbox（复选框）保留原实施顺序，不代表当前代码全部未实现。

## 0.1 2026-07-08 开发就绪审查结论（历史基线）

结论：V2-1 可以进入开发，但执行前必须按本计划中的“实现锚点”落地，不要只按概念描述开发。当前代码与计划之间最容易漏掉的点有 6 个：

- `UserPreferences`（用户偏好类型）的主定义在 `openless-all/app/src-tauri/src/types.rs`，`PreferencesStore`（偏好存储）只负责读写；新增 `meeting_asr` 必须同步 `UserPreferences`、`UserPreferencesWire`、`Default` 和 TypeScript `UserPreferences`。
- 现有 `CredentialsVault::get(...)` 读取的是 active ASR provider（当前全局语音转文字服务商），会议 provider-specific（按会议服务商）读取必须新增只读 helper，不能通过临时切换全局 active ASR 实现。
- 当前 `MeetingSession`（会议会话）已有 `active_provider`，但 `resume_meeting_recording` 仍会重新读取全局 active ASR；V2-1 必须改为从 `MeetingSession` runtime state（运行时状态）读取锁定的 provider 和 silence preset（静音档位）。
- `MeetingRecordingSnapshot`（会议录音快照）当前没有 `providerSessionId`，但前端 stale event guard（旧事件防污染）需要它；V2-1 应新增 `activeProviderSessionId`。
- 现有 `meeting_next_part_index` 是 1-based（从 1 开始）的录音分片序号，`audioPartIndex` 必须沿用这个语义，不要改成 0-based。
- 前端 browser mock（浏览器模拟）也要补 `listAsrProviderCapabilities`、`mockSettings.meetingAsr` 和 meeting mock metadata，否则 `npm run build` 或浏览器预览会被类型/空值问题挡住。

不需要再向用户确认新的产品问题；剩余都是实现细节约束。

## 1. 背景与目标

V1 会议录音已经能保存会议原文，但 realtime（实时）体验仍依赖 provider final segment（服务商最终片段）。V2-1 要把会议场景从“尽量实时追加 final”升级为“draft + final 双层显示”：

- draft（临时识别）：录音中底部“正在识别”行，来自 provider interim result（临时结果）。
- final（最终片段）：provider 确认后的片段，生成 `TranscriptSegment` 并持久化。
- timestamp（时间戳）：优先保存 word/token-level timestamp（词级/Token 级时间戳），用于保存可回退的实时原文和诊断跨 session（会话）时间轴；V2-2 所选会后模型 `fun-asr` 或 `paraformer-v2` 使用自己的结构化时间戳生成整理后原文，本地 speaker diarization（说话人分离）优先与该会后原文对齐，不跨模型把标签硬贴回实时文本。

## 2. V2-1 范围

- 设置 -> 服务新增“会议 ASR”区域。
- 会议 ASR 默认继承全局 ASR。
- 会议 ASR 可选择所有现有 ASR provider。
- 复用现有 CredentialsVault（凭据存储），不新增会议专用密钥保存。
- 不暴露 Workspace ID、region（区域）或会议专用 endpoint（端点）字段。
- 新增 ASR provider capabilities（服务商能力声明）。
- 对支持 `supportsVadSilencePreset` 的 provider 显示 silence preset（静音档位）：短 / 标准 / 长。
- 增强现有 `bailian` provider，作为 `fun-asr-realtime` V2-1 主实现。
- 新增 `meeting:transcript-draft` event（会议临时原文事件）。
- 扩展 `TranscriptSegment` 增加 `metadata`（元数据）。
- pause 关闭当前 ASR session，resume 新开 ASR session。
- 网络 / server error（服务端错误）后继续录音、保存音频、清空 draft、状态进入 `transcribing_interrupted`。
- active meeting（活跃会议）期间锁定 effective provider（实际服务商）和 silence preset（静音档位）；设置变更只影响下一场会议。
- 对 batch provider（批处理语音转文字服务商）保持 V1 降级行为：录音中不显示假 draft，pause / stop 后按返回结果落 final segment。

## 3. 明确不做项

- 不做 speaker diarization（说话人分离）。
- 不做 system audio capture（系统声音采集）。
- 不做上传音频。
- 不新增手动重新生成原文入口；不删除、不重构 V1 已有“重新转写”能力。
- 不做停止后自动重新转写。
- 不做会议音频文件流式上传；该能力由 V2-2 共用 audio source（音频源）实现。
- 不做 subtitle-grade streaming（字幕级低延迟流式逐字刷新）。
- 不做第二次 punctuation restoration（标点恢复）。
- 不做第二次 ITN（Inverse Text Normalization，逆文本规范化）。
- 不做 LLM cleanup（大语言模型清理）。
- 不做会议专用 LLM provider（大语言模型服务商）配置。
- 不把新会议设置暴露到短口述 UI。

## 4. 2026-07-08 实施前代码结构观察（历史基线）

后端：

- `openless-all/app/src-tauri/src/asr/bailian.rs`
  - 当前已有 `PROVIDER_ID = "bailian"`、`DEFAULT_MODEL = "fun-asr-realtime"`。
  - 当前已经处理 `result-generated`、`sentence_end`、`begin_time`、`end_time`。
  - 当前已有 `AsrFinalSegmentSink`，但没有 draft sink（临时片段回调）和 token timestamp metadata（Token 时间戳元数据）。
- `openless-all/app/src-tauri/src/asr/mod.rs`
  - 当前 `AsrFinalSegment` 只有 `text`、`start_ms`、`end_ms`。
  - V2-1 需要补 provider/session/sequence/metadata。
- `openless-all/app/src-tauri/src/coordinator/meeting.rs`
  - 当前 `meeting_final_segment_sink` 把 final segment 转为 `TranscriptSegment`。
  - 当前已 emit `meeting:transcript-segment`、`meeting:error`。
  - V2-1 需要新增 draft sink，并在 ASR 中断时 emit draft clear。
  - 当前 pause / resume 会产生多个录音 part（分片）和多个 ASR session；V2-1 必须保存 timebase（时间轴）映射，避免后续 V2-2 无法把 speaker turns（说话人时间段）对齐回原文。
  - 当前 `MeetingSession.active_provider` 已能表达“本场会议锁定 provider”，但 `resume_meeting_recording` 仍重新读取全局 active ASR；V2-1 必须修正为从 session runtime state 读取。
  - 当前 `meeting_next_part_index` 初始化为 `1`，`start_meeting_recorder` 先读取当前值作为 part index，再自增；V2-1 的 `audioPartIndex` 沿用 1-based 语义。
- `openless-all/app/src-tauri/src/persistence/credentials.rs`
  - 当前 `CredentialsVault::get(CredentialAccount::AsrApiKey)` 依赖 active ASR provider。
  - 当前 `lookup_account(root, account)` 和 `write_account(root, account, value)` 都以 `root.active.asr` 路由 ASR entry。
  - V2-1 会议专用 ASR 如果不能影响全局短口述，需要最小新增“按 provider id 读取 ASR 凭据”的能力。
- `openless-all/app/src-tauri/src/persistence/preferences.rs`
  - 当前只负责读写 `UserPreferences`，不是新增偏好字段的主类型位置。
- `openless-all/app/src-tauri/src/types.rs`
  - 当前 `TranscriptSegment` 没有 `metadata`。
  - 当前没有 `MeetingTranscriptDraftEvent`。
  - 当前 `UserPreferences`、`UserPreferencesWire`、`Default for UserPreferences` 都在本文件；新增 `meeting_asr` 必须同时改三处，避免旧 preferences JSON 读取失败或字段往返丢失。
  - 当前 `MeetingRecordingSnapshot` 没有 `activeProviderSessionId`，前端无法校验 draft event 是否属于当前 ASR session。

前端：

- `openless-all/app/src/pages/Meetings.tsx`
  - 当前展示 `TranscriptSegment` 列表。
  - V2-1 需要订阅 `meeting:transcript-draft` 并渲染底部“正在识别”行。
- `openless-all/app/src/pages/settings/ProvidersSection.tsx`
  - 当前 ASR provider 列表已有 `bailian`，并把模型设为 `fun-asr-realtime`。
  - 当前本地 provider 部分在服务页下拉中隐藏，V2-1 会议 ASR 应允许选择所有 provider。
- `openless-all/app/src/lib/types.ts`
  - 需要补 TS 类型。
- `openless-all/app/src/lib/ipc/mock-data.ts`
  - 需要补 `mockSettings.meetingAsr` 和 mock meeting segment metadata，保证 browser mock 与 Tauri 环境同形。
- `openless-all/app/src/lib/ipc/settings.ts` 或现有 provider IPC wrapper
  - 需要新增 `listAsrProviderCapabilities()` wrapper 和 mock 返回值。
- `openless-all/app/src/i18n/*.ts`
  - 需要补“会议 ASR”、silence preset、draft 行、错误提示文案。

## 5. 数据契约设计

V2-1 的持久化原则：

- `TranscriptSegment.startMs/endMs` 使用 meeting-relative active audio time（会议内有效音频时间轴），不包含暂停时长。
- provider 原始时间写入 `metadata.providerStartMs/providerEndMs`，不直接当作跨 session 全局时间。
- 每次 pause / resume 后 provider timestamp（服务商时间戳）可能从 0 重新开始，必须用 `providerSessionId` 和 `sessionStartMs` 区分。
- 会议音频可能按 part（分片）保存，必须记录 `audioPartIndex`，便于 V2-2 本地 speaker diarization（说话人分离）定位音频文件。
- 不保存完整 provider raw payload（服务商原始负载），只保存标准化 metadata（元数据）。
- `supportsWordTimestamp` 在 V2-1 中表示“可返回 word / char / token timestamp（词 / 字 / Token 级时间戳）之一”，不要假设中文一定是英文意义上的 word。`fun-asr-realtime` 的 `words` 字段应按实际 token 文本写入 `TranscriptTokenTimestamp.kind`：中文单字优先用 `char`，英文词优先用 `word`，无法判断时用 `token`。
- `MeetingRecordingSnapshot.activeProviderSessionId` 只表达当前 active ASR session（活跃语音识别会话）的 id；会议停止后可以为 `None/null`。

### 5.1 Rust 类型

修改 `openless-all/app/src-tauri/src/types.rs`：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptTokenKind {
    Word,
    Char,
    Token,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptTokenTimestamp {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub provider_start_ms: Option<u64>,
    pub provider_end_ms: Option<u64>,
    pub kind: TranscriptTokenKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct TranscriptSegmentMetadata {
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
```

修改 `TranscriptSegment`：

```rust
pub struct TranscriptSegment {
    pub id: String,
    pub speaker_label: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub text: String,
    pub source: TranscriptSegmentSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<TranscriptSegmentMetadata>,
}
```

新增 event：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MeetingTranscriptDraftEvent {
    pub meeting_id: String,
    pub provider_id: String,
    pub provider_session_id: Option<String>,
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub sequence: Option<u64>,
    pub clear: bool,
}
```

修改 `MeetingRecordingSnapshot`，为前端 stale event guard（旧事件防污染）提供当前 ASR session id：

```rust
pub struct MeetingRecordingSnapshot {
    pub meeting: MeetingRecord,
    pub phase: MeetingRecordingPhase,
    pub elapsed_ms: u64,
    pub active_asr_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_provider_session_id: Option<String>,
    pub asr_interrupted: bool,
}
```

### 5.2 TypeScript 类型

修改 `openless-all/app/src/lib/types.ts`：

```ts
export type TranscriptTokenKind = 'word' | 'char' | 'token';

export interface TranscriptTokenTimestamp {
  text: string;
  startMs: number;
  endMs: number;
  providerStartMs: number | null;
  providerEndMs: number | null;
  kind: TranscriptTokenKind;
}

export interface TranscriptSegmentMetadata {
  providerId: string | null;
  providerSessionId: string | null;
  providerSegmentId: string | null;
  sentenceId: string | null;
  sequence: number | null;
  audioPartIndex: number | null;
  sessionStartMs: number | null;
  providerStartMs: number | null;
  providerEndMs: number | null;
  tokenTimestamps: TranscriptTokenTimestamp[];
}

export interface MeetingTranscriptDraftEvent {
  meetingId: string;
  providerId: string;
  providerSessionId: string | null;
  text: string;
  startMs: number | null;
  endMs: number | null;
  sequence: number | null;
  clear: boolean;
}
```

`TranscriptSegment` 增加：

```ts
metadata?: TranscriptSegmentMetadata | null;
```

`MeetingRecordingSnapshot` 增加：

```ts
activeProviderSessionId?: string | null;
```

## 6. Provider capabilities 设计

新增 provider capability（服务商能力）类型，优先放在后端 `openless-all/app/src-tauri/src/asr/mod.rs` 或邻近小模块：

```rust
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
```

V2-1 初始能力表：

| provider | supportsRealtime | supportsDraftResult | supportsFinalSegmentEvent | supportsBatchResult | supportsVadSilencePreset | supportsSentenceTimestamp | supportsWordTimestamp | V2-1 行为 |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| `bailian` / `fun-asr-realtime` | true | true | true | false | true | true | true | V2-1 主实现，显示 draft + final |
| `volcengine` | true | false | true | false | false | true | false | 保持 final segment 追加，不显示 draft |
| `siliconflow` / Whisper-compatible | false | false | false | true | false | false | false | batch fallback，pause / stop 后落段 |
| `zhipu` / Whisper-compatible | false | false | false | true | false | false | false | batch fallback |
| `groq` / Whisper-compatible | false | false | false | true | false | false | false | batch fallback |
| `whisper` | false | false | false | true | false | false | false | batch fallback |
| `openrouter` | false | false | false | true | false | false | false | batch fallback |
| `xiaomi-mimo-asr` | false | false | false | true | false | false | false | batch fallback |
| `foundry-local-whisper` | false | false | false | true | false | false | false | local batch fallback |
| `sherpa-onnx-local` | false | false | false | true | false | false | false | V2-1 保守声明；后续本地 streaming 单独升级 |
| `local-qwen3` | false | false | false | true | false | false | false | local batch fallback |
| `apple-speech` | false | false | false | true | false | false | false | 保守 fallback，不在 V2-1 做会议实时适配 |

默认规则：

- 未列出的 provider 默认只声明 `supportsBatchResult = true`，其他 realtime 能力为 false。
- UI 不能根据 provider name（服务商名称）猜能力，必须读 capability snapshot（能力快照）。
- 支持 batch 的 provider 仍可用于会议 ASR，但不显示“正在识别”draft 行，不承诺录音中实时追加。
- browser mock（浏览器模拟）必须返回同形 `AsrProviderCapabilities[]`，避免设置页在非 Tauri 环境下因为 IPC 缺失而无法渲染。

新增 IPC（Inter-Process Communication，进程间调用）建议：

```rust
#[tauri::command]
pub fn list_asr_provider_capabilities() -> Vec<AsrProviderCapabilities>
```

前端调用封装放在 `openless-all/app/src/lib/ipc/settings.ts` 或现有 provider IPC 文件中。

## 7. Meeting ASR config 设计

修改 preferences（偏好设置）类型，新增字段的实际落点：

- Rust 主类型：`openless-all/app/src-tauri/src/types.rs`
  - `UserPreferences`
  - `UserPreferencesWire`
  - `impl Default for UserPreferences`
  - 相关 serde round trip（序列化往返）测试
- 存储读写：`openless-all/app/src-tauri/src/persistence/preferences.rs`
  - 通常只需要补测试，不应把新字段主定义放在这里。
- TypeScript 类型：`openless-all/app/src/lib/types.ts`
  - `UserPreferences` 增加 `meetingAsr`。
- Mock：`openless-all/app/src/lib/ipc/mock-data.ts`
  - `mockSettings.meetingAsr` 必须给默认值。

新增类型：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeetingAsrMode {
    InheritGlobal,
    ProviderSpecific,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeetingVadSilencePreset {
    Short,
    Standard,
    Long,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct MeetingAsrSettings {
    pub mode: MeetingAsrMode,
    pub provider_id: Option<String>,
    pub vad_silence_preset: MeetingVadSilencePreset,
}
```

默认值：

```rust
MeetingAsrSettings {
    mode: MeetingAsrMode::InheritGlobal,
    provider_id: None,
    vad_silence_preset: MeetingVadSilencePreset::Standard,
}
```

effective provider（实际服务商）：

```text
if meeting_asr.mode == inherit_global:
  use CredentialsVault active ASR
else:
  use meeting_asr.provider_id
```

active meeting provider lock（活跃会议服务商锁定）：

```text
start meeting:
  resolve effective provider + capabilities + silence preset
  store them in MeetingSession runtime state

while meeting is active:
  settings changes do not change this MeetingSession
  pause/resume reuses the locked provider and preset

next meeting:
  read latest settings again
```

V2-1 不做 hot switch（热切换 provider）。如果用户在会议录音中修改会议 ASR 设置，设置可以保存，但当前会议不切换 provider，避免同一会议中多个 provider 的 timestamp（时间戳）、sequence（序号）和错误状态混杂。

最小后端补点：

- 新增按 provider id 读取 ASR 凭据的方法，避免为了会议 ASR 临时修改全局 active ASR。
- 不新增新密钥槽，仍读取现有 provider entry。
- 不修改 `CredentialsVault::set_active_asr_provider`、`CredentialsVault::get_active_asr` 和短口述调用点。

建议接口形态：

```rust
impl CredentialsVault {
    pub fn get_asr_for_provider(provider_id: &str, account: CredentialAccount) -> Result<Option<String>>;
}
```

实现约束：

- `get_asr_for_provider` 内部读取 `root.providers.asr.get(provider_id)`，不能临时改 `root.active.asr`。
- 只允许读取 ASR 相关 `CredentialAccount`：`AsrApiKey`、`AsrEndpoint`、`AsrModel`、`AsrVocabularyId`、`VolcengineAppKey`、`VolcengineAccessKey`、`VolcengineResourceId`。
- 对 `VolcengineAppKey` 保持现有兼容：优先 `appKey`，为空时 fallback 到 `apiKey`。
- 对本地 ASR provider（本地语音转文字服务商）返回空凭据是合法状态，后续由本地模型 readiness（就绪状态）检查处理。
- 增加测试覆盖：读取非 active provider 的 API key / endpoint / model 不改变 `root.active.asr`，且 active provider 读取结果不被污染。

ASR credentials 检查也要 provider-specific：

```rust
pub(super) fn ensure_asr_credentials_for_provider(provider_id: &str) -> Result<(), String>;
```

短口述继续调用现有 `ensure_asr_credentials()`；会议启动调用 `ensure_asr_credentials_for_provider(effective_provider)`。

### 7.1 ASR 启动 options 与 meeting runtime state

不要继续把 `build_qa_asr_start_with_final_segment_sink(...)` 的参数无限加长。V2-1 建议新增 meeting 专用 options（选项）结构，短口述主链路继续使用现有函数：

```rust
pub(super) struct MeetingAsrStartOptions {
    pub provider_id: String,
    pub final_segment_sink: Option<AsrFinalSegmentSink>,
    pub draft_segment_sink: Option<AsrDraftSegmentSink>,
    pub provider_session_id: String,
    pub session_start_ms: u64,
    pub audio_part_index: u32,
    pub silence_preset: MeetingVadSilencePreset,
}
```

`MeetingSession` 增加 runtime state（运行时状态），不要只散落在多个 `Mutex` 中：

```rust
pub(super) struct MeetingAsrRuntimeState {
    pub provider_id: String,
    pub capabilities: AsrProviderCapabilities,
    pub silence_preset: MeetingVadSilencePreset,
    pub provider_session_id: Option<String>,
    pub session_start_ms: u64,
    pub audio_part_index: u32,
    pub next_sequence: u64,
}
```

边界：

- start meeting 时 resolve effective provider（实际服务商）、capabilities（能力声明）和 silence preset，并写入 `MeetingSession`。
- 每次打开新 ASR session 前生成新的 `provider_session_id`，并把 `session_start_ms = session.elapsed_ms_at(now)`。
- `audio_part_index` 使用即将写入的录音 part 序号，沿用当前代码 1-based 语义。
- resume 时必须读取 `MeetingSession` 内锁定的 provider / preset，不重新读 `CredentialsVault::get_active_asr()` 或 preferences。
- snapshot 时把当前 `provider_session_id` 写入 `MeetingRecordingSnapshot.activeProviderSessionId`。
- ASR session 结束、pause、stop 或 interrupted 后发送 draft clear；停止会议后 snapshot 中 `activeProviderSessionId` 可以为空。

## 8. Fun-ASR Realtime 接入设计

官方接口要点（以阿里云百炼 `fun-asr-realtime` WebSocket API 文档为准，执行前再核对一次当前文档：https://help.aliyun.com/zh/model-studio/fun-asr-realtime-websocket-api）：

- WebSocket（网页套接字）连接。
- 客户端发送 `run-task` 开始任务。
- 客户端发送 binary audio（二进制音频）。
- 客户端发送 `finish-task` 结束任务。
- 服务端通过 `result-generated` 返回 interim / final（临时 / 最终）结果。
- 结果中包含 `sentence`、`words`、`begin_time`、`end_time`、`sentence_end` 等字段。
- `max_sentence_silence` 属于 `parameters`（参数）里的 server VAD（服务端语音活动检测）静音阈值，官方范围是毫秒级配置；V2-1 只暴露简单档位，不暴露原始数字输入。
- 如果官方字段名或 shape（结构）和当前代码假设不同，以实际文档与测试 payload 为准，先保存 sentence timestamp（句级时间戳），不要臆造 word timestamp（词级时间戳）。

V2-1 不新增 `fun-asr-realtime` 第二 provider。现有 `bailian` provider 已经使用 `DEFAULT_MODEL = "fun-asr-realtime"`，因此只增强 `openless-all/app/src-tauri/src/asr/bailian.rs`：

- 为 interim result emit draft。
- 为 final result emit final segment metadata。
- 将 `max_sentence_silence` 参数映射到 silence preset：
  - short：`600`
  - standard：`800`
  - long：`1200`
- 解析 `words` 为 `token_timestamps`。
- 使用 `sentence_id` 作为优先 dedup key（去重键）。
- 每次 WebSocket session 生成 `provider_session_id`。
- run-task payload 中只加入 V2-1 需要的最小参数：`sample_rate`、`format`、可选 `vocabulary_id`、`max_sentence_silence`。不要在 V2-1 新增 punctuation / ITN 二次开关 UI。
- realtime 阶段不改 provider 文本；final 文本只做 trim（去首尾空白）、不可见控制字符移除、换行规范化和空 final 丢弃。

silence preset 生效边界：

- 只在创建新 WebSocket session 时写入 `run-task` payload（负载）。
- 会议中修改 silence preset 不影响当前 active meeting。
- pause 后 resume 仍使用会议开始时锁定的 preset，不读取新设置。

timebase（时间轴）转换：

```text
sessionStartMs = 当前 ASR session 在 meeting active audio timeline 上的起点
providerStartMs = provider 返回的 begin_time / start time
segment.startMs = sessionStartMs + providerStartMs
segment.endMs = sessionStartMs + providerEndMs
metadata.providerStartMs = providerStartMs
metadata.providerEndMs = providerEndMs
metadata.sessionStartMs = sessionStartMs
metadata.audioPartIndex = 当前录音 part index
```

如果 provider 没有返回 timestamp：

- `metadata.providerStartMs/providerEndMs = null`。
- `segment.startMs` 使用当前 session 的 append fallback（追加兜底）时间，保证 UI 有稳定排序。
- `supportsSentenceTimestamp/supportsWordTimestamp` 必须为 false，后续 V2-2 不能假设可精确对齐。

## 9. Draft / final 事件流

后端：

- `meeting:transcript-draft`
  - interim result 到达时 emit。
  - final result 到达时 emit `clear: true`，再 emit `meeting:transcript-segment`。
  - ASR 中断、pause、stop 时 emit `clear: true`。
- `meeting:transcript-segment`
  - 只用于 final segment。
  - 必须包含 `metadata`。

前端：

- `Meetings.tsx` 增加 `draftByMeetingId` state。
- 收到 draft event：
  - `clear: true` 时清空该 meeting draft。
  - 否则覆盖同一 meeting 的 draft。
- 原文列表底部显示一行“正在识别”。
- 切换会议时只显示当前 selected meeting 的 draft。
- 停止后清空 draft。
- draft event 必须按 `meetingId + providerSessionId + sequence` 做 stale event guard（旧事件防污染）；无法匹配当前 active meeting 的 draft 不渲染。
- 如果 `activeSnapshot.activeProviderSessionId` 存在，前端必须校验 `event.providerSessionId === activeSnapshot.activeProviderSessionId`；如果 snapshot 为空或该字段为空，只允许处理 `clear: true` 或仅按 `meetingId` 清理，不渲染新的 draft。
- 后端仍是第一道防线：draft sink 捕获 `meetingId + providerSessionId`，如果当前 session 已经不是同一个 meeting/provider session，不 emit 用户可见 draft。
- draft 不进入 search（搜索）、summary（总结）、export（导出）或 persisted record（持久化记录）。

## 10. Task 分解

### Task 1: 数据类型与兼容读取

**Files:**

- Modify: `openless-all/app/src-tauri/src/types.rs`
- Modify: `openless-all/app/src/lib/types.ts`
- Modify: `openless-all/app/src/lib/ipc/mock-data.ts`
- Test: `openless-all/app/src-tauri/src/persistence/meeting.rs` 或现有 meeting store 测试模块

- [ ] **Step 1: 写 Rust 类型兼容测试**

测试旧 JSON 没有 `metadata` 仍能反序列化为 `metadata = None`。
同时测试新 JSON 包含 `audioPartIndex`、`sessionStartMs` 和 `tokenTimestamps` 时能完整 round trip（序列化再反序列化）。

Run:

```powershell
cd D:\codex项目\VOXHUB\openless-all\app\src-tauri
cargo test meeting_segment_metadata
```

Expected: 先失败，因为类型还不存在。

- [ ] **Step 2: 增加 Rust 类型**

按第 5.1 节修改 `types.rs`，给 `TranscriptSegment` 增加可选 `metadata`。
同时给 `MeetingRecordingSnapshot` 增加可选 `active_provider_session_id`，并新增 `MeetingTranscriptDraftEvent`。

- [ ] **Step 3: 增加 TypeScript 类型**

按第 5.2 节修改 `openless-all/app/src/lib/types.ts`。
同时更新 `mock-data.ts`：mock meeting 的老 segment 可以不带 metadata，但如新增 V2 示例 segment，metadata 必须完整同形。

- [ ] **Step 4: 验证**

Run:

```powershell
cd D:\codex项目\VOXHUB\openless-all\app
.\node_modules\.bin\tsc.CMD --noEmit
```

Run:

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo test meeting_segment_metadata"
```

Expected: PASS。

### Task 2: Provider capabilities

**Files:**

- Modify: `openless-all/app/src-tauri/src/asr/mod.rs`
- Modify: `openless-all/app/src-tauri/src/commands/providers.rs`
- Modify: `openless-all/app/src-tauri/src/lib.rs`
- Modify: `openless-all/app/src/lib/ipc/settings.ts` 或现有 provider IPC 文件
- Modify: `openless-all/app/src/lib/types.ts`
- Modify: `openless-all/app/src/lib/ipc/mock-data.ts`

- [ ] **Step 1: 写 capability 测试**

覆盖 `bailian` capabilities 必须声明：

```text
supportsRealtime = true
supportsDraftResult = true
supportsVadSilencePreset = true
supportsSentenceTimestamp = true
supportsWordTimestamp = true
```

同时覆盖 batch provider（批处理服务商）默认不声明 realtime 能力：

```text
supportsRealtime = false
supportsDraftResult = false
supportsFinalSegmentEvent = false
supportsBatchResult = true
```

- [ ] **Step 2: 增加 `AsrProviderCapabilities`**

按第 6 节添加 Rust 类型和静态能力函数。

- [ ] **Step 3: 暴露 IPC**

新增 `list_asr_provider_capabilities` command，并注册到 `lib.rs`。

- [ ] **Step 4: 增加 TS wrapper**

返回 `AsrProviderCapabilities[]`，供设置页使用。
mock wrapper 必须返回同一张能力表，至少包含 `bailian`、`volcengine`、Whisper-compatible provider、本地 ASR provider。

- [ ] **Step 5: 验证**

Run:

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo test capability"
```

Expected: PASS。

### Task 3: Meeting ASR settings

**Files:**

- Modify: `openless-all/app/src-tauri/src/types.rs`
- Modify: `openless-all/app/src-tauri/src/persistence/preferences.rs`
- Modify: `openless-all/app/src/lib/types.ts`
- Modify: `openless-all/app/src/lib/ipc/mock-data.ts`
- Modify: `openless-all/app/src/pages/settings/ProvidersSection.tsx` 或新增 `openless-all/app/src/pages/settings/MeetingAsrSection.tsx`
- Modify: `openless-all/app/src/i18n/zh-CN.ts`
- Modify: `openless-all/app/src/i18n/zh-TW.ts`
- Modify: `openless-all/app/src/i18n/en.ts`
- Modify: `openless-all/app/src/i18n/ja.ts`
- Modify: `openless-all/app/src/i18n/ko.ts`

- [ ] **Step 1: 写 preferences 默认值测试**

旧 preferences JSON 没有 `meetingAsr` 时，默认应为 inherit global + standard preset。

- [ ] **Step 2: 增加 preferences 字段**

在 `types.rs` 的 `UserPreferences`、`UserPreferencesWire`、`impl Default for UserPreferences` 同步添加 `meeting_asr`，并确保 serde default（序列化默认值）兼容旧配置。`preferences.rs` 只补读写测试，不把主类型定义迁到这里。
在 `openless-all/app/src/lib/types.ts` 的 `UserPreferences` 增加 `meetingAsr`，并在 `mock-data.ts` 的 `mockSettings` 加默认值。

- [ ] **Step 3: 设置页新增“会议 ASR”**

UI 行为：

- mode：继承全局 ASR / 单独选择。
- provider select：显示所有 ASR provider。
- silence preset：仅当 selected provider capability `supportsVadSilencePreset` 为 true 时显示。
- 不显示 Workspace ID。
- 不显示 region。
- 不新增 endpoint 字段。
- 不显示会议页 ASR 状态。
- 如果当前有 active meeting，设置页允许保存新会议 ASR 设置，但显示“当前会议继续使用开始时的 ASR 设置，新设置下场会议生效”。

- [ ] **Step 4: i18n 补齐五种语言**

新增 key：

```text
settings.providers.meetingAsrTitle
settings.providers.meetingAsrDesc
settings.providers.meetingAsrInheritGlobal
settings.providers.meetingAsrProviderSpecific
settings.providers.meetingAsrProviderLabel
settings.providers.meetingAsrSilencePresetLabel
settings.providers.meetingAsrSilenceShort
settings.providers.meetingAsrSilenceStandard
settings.providers.meetingAsrSilenceLong
```

- [ ] **Step 5: 验证**

Run:

```powershell
cd D:\codex项目\VOXHUB\openless-all\app
.\node_modules\.bin\tsc.CMD --noEmit
npm run build
```

Expected: PASS。

### Task 4: 会议启动时使用 effective meeting ASR

**Files:**

- Modify: `openless-all/app/src-tauri/src/persistence/credentials.rs`
- Modify: `openless-all/app/src-tauri/src/coordinator/meeting.rs`
- Modify: `openless-all/app/src-tauri/src/coordinator/asr_wiring.rs`
- Modify: `openless-all/app/src-tauri/src/types.rs`
- Test: `openless-all/app/src-tauri/src/coordinator/meeting.rs`
- Test: `openless-all/app/src-tauri/src/persistence/credentials.rs`

- [ ] **Step 1: 写 effective provider 测试**

覆盖：

- inherit global 使用 `CredentialsVault::get_active_asr()`。
- provider specific 使用 meeting setting provider id。
- provider specific 不改变全局 active ASR。
- active meeting 期间锁定 provider；录音中修改设置后，resume 仍使用原 provider。
- silence preset 与 provider 一起锁定；录音中修改 preset 后，resume 仍使用原 preset。

- [ ] **Step 2: 新增按 provider id 读取 ASR 凭据**

在 `CredentialsVault` 增加只读方法，不改现有 active provider。
测试必须覆盖：

```text
root.active.asr = volcengine
读取 bailian 的 AsrApiKey / AsrEndpoint / AsrModel
读取后 root.active.asr 仍是 volcengine
```

如果读取的是本地 provider，凭据为空不应直接报错；由 provider readiness（服务商就绪检查）负责判断。

- [ ] **Step 3: 修改 meeting ASR 构造**

`start_meeting_recording` 和 `resume_meeting_recording` 使用 effective meeting ASR provider。
`start_meeting_recording` 必须把 effective provider、capabilities、silence preset 写入 `MeetingSession` runtime state；`resume_meeting_recording` 从 runtime state 读取，不重新解析 preferences。
当前代码中 `resume_meeting_recording` 重新调用 `CredentialsVault::get_active_asr()`，这是 V2-1 必须修复的行为，不是可选优化。

- [ ] **Step 3.1: 增加 meeting ASR options**

新增 `MeetingAsrStartOptions` 或同等结构，把 provider、final sink、draft sink、provider session id、session start、audio part、silence preset 作为一个对象传给 meeting ASR 构造。短口述 `build_qa_asr_start(...)` 和 `build_qa_asr_start_with_final_segment_sink(...)` 的现有行为不变。

- [ ] **Step 4: 保持短口述不变**

短口述仍继续读全局 active ASR，不读取 meeting setting。

- [ ] **Step 5: 记录 timebase 起点**

每次启动 ASR session 时记录：

```rust
provider_session_id: String
audio_part_index: u32 // 沿用当前 1-based meeting_next_part_index
session_start_ms: u64
```

`session_start_ms` 使用 meeting active audio timeline（会议有效音频时间轴），不包含暂停时长。
`MeetingRecordingSnapshot.activeProviderSessionId` 从当前 runtime state 输出，供前端 draft stale guard 使用。

- [ ] **Step 6: 验证**

Run:

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo test meeting"
```

Expected: PASS。

### Task 5: ASR draft sink 与 metadata sink

**Files:**

- Modify: `openless-all/app/src-tauri/src/asr/mod.rs`
- Modify: `openless-all/app/src-tauri/src/asr/bailian.rs`
- Modify: `openless-all/app/src-tauri/src/coordinator/meeting.rs`
- Test: `openless-all/app/src-tauri/src/asr/bailian.rs`
- Test: `openless-all/app/src-tauri/src/coordinator/meeting.rs`

- [ ] **Step 1: 扩展 ASR segment 结构**

`AsrFinalSegment` 增加：

```rust
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
```

新增：

```rust
pub struct AsrDraftSegment {
    pub provider_id: String,
    pub provider_session_id: Option<String>,
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub sequence: Option<u64>,
    pub audio_part_index: Option<u32>,
    pub session_start_ms: Option<u64>,
}

pub type AsrDraftSegmentSink = Arc<dyn Fn(AsrDraftSegment) + Send + Sync>;
```

- [ ] **Step 2: Bailian interim result 调用 draft sink**

`sentence_end == false` 时发 draft，不写入 final_segments。

- [ ] **Step 3: Bailian final result 调用 final sink**

`sentence_end == true` 时发 final，并带 metadata。
final sink 里必须带上 `provider_session_id`、`sequence`、`audio_part_index`、`session_start_ms`，这些来自 `MeetingAsrStartOptions`，不是从 provider payload 猜出来。

- [ ] **Step 4: 解析 words**

将 provider `words` 转成 `TranscriptTokenTimestamp`，保留 meeting-relative `start_ms/end_ms` 和 provider raw time。
如果 `words` item 只有字级文本，`kind = Char`；如果 item 明确是词，`kind = Word`；否则 `kind = Token`。

- [ ] **Step 5: 按 timebase 转换 segment 时间**

在 meeting sink 中把 provider raw time 转成 meeting-relative time：

```text
startMs = sessionStartMs + providerStartMs
endMs = sessionStartMs + providerEndMs
```

如果 provider raw time 缺失，使用 append fallback time，并把 provider raw time 保持为 null。

- [ ] **Step 6: 去重策略**

final segment 去重顺序：

```text
1. meetingId + providerId + providerSessionId + providerSegmentId
2. meetingId + providerId + providerSessionId + sentenceId
3. meetingId + providerId + providerSessionId + sequence
4. fallback: normalizedText + startMs + endMs
```

现有 `transcript_segment_exists(record, text, start_ms, end_ms)` 可以作为 fallback，不要直接删除；先补 metadata key 去重，再保留文本+时间兜底。

- [ ] **Step 7: 测试**

覆盖：

- interim 只发 draft，不发 final。
- final 清理同 sentence 的 draft 并发 final。
- `words` 被保存到 metadata。
- 重复 `sentence_id` 不重复追加 final。
- 同一个 `sentence_id` 在不同 `providerSessionId` 下不会被错误去重。
- pause/resume 后第二个 provider session 的 providerStartMs 从 0 开始时，segment.startMs 仍落在会议全局 active audio timeline 的正确位置。
- final segment metadata 包含 `audioPartIndex` 和 `sessionStartMs`。

Run:

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo test bailian"
```

Expected: PASS。

### Task 6: Meeting draft event 与 UI 渲染

**Files:**

- Modify: `openless-all/app/src-tauri/src/coordinator/meeting.rs`
- Modify: `openless-all/app/src/pages/Meetings.tsx`
- Modify: `openless-all/app/src/lib/types.ts`
- Modify: `openless-all/app/src/i18n/zh-CN.ts`
- Modify: `openless-all/app/src/i18n/zh-TW.ts`
- Modify: `openless-all/app/src/i18n/en.ts`
- Modify: `openless-all/app/src/i18n/ja.ts`
- Modify: `openless-all/app/src/i18n/ko.ts`

- [ ] **Step 1: 后端 emit draft**

新增 `emit_meeting_transcript_draft`：

```text
meeting:transcript-draft
```

pause / stop / ASR interrupted 都发送 `clear: true`。

- [ ] **Step 2: 前端订阅 draft**

在 `Meetings.tsx` 的 event subscription（事件订阅）中增加 `meeting:transcript-draft`。

- [ ] **Step 3: 前端渲染底部“正在识别”行**

仅当前 selected meeting 有 draft 时显示。

- [ ] **Step 4: final 到达时清空 draft**

收到同 meeting 的 final segment event 后清空 draft。

- [ ] **Step 5: batch provider 降级 UI**

当 selected meeting ASR provider capability `supportsDraftResult = false`：

- 不显示“正在识别”行。
- 不显示假进度。
- 保留 V1 文案：batch provider 可能在 pause / stop 后追加原文。

- [ ] **Step 6: stale event guard**

前端处理 draft event 时检查：

```text
event.meetingId === activeMeeting.id
event.providerSessionId matches activeSnapshot.activeProviderSessionId when activeProviderSessionId is present
```

无法匹配时忽略，避免上一场会议或上一段 ASR session 的 draft 污染当前会议。
如果没有 active snapshot，前端只能接受 `clear: true` 事件来清理 draft，不渲染新的 draft 文本。

- [ ] **Step 7: 验证**

Run:

```powershell
cd D:\codex项目\VOXHUB\openless-all\app
.\node_modules\.bin\tsc.CMD --noEmit
npm run build
```

Expected: PASS。

### Task 7: 网络错误与中断边界

**Files:**

- Modify: `openless-all/app/src-tauri/src/asr/bailian.rs`
- Modify: `openless-all/app/src-tauri/src/coordinator/meeting.rs`
- Test: `openless-all/app/src-tauri/src/coordinator/meeting.rs`

- [ ] **Step 1: 写中断测试**

模拟 ASR error 后：

- meeting recording 继续。
- local audio path 继续保留。
- meeting status 是 `transcribing_interrupted`。
- 已有 final segments 不丢。
- draft clear event 被发送。
- 后续 stop 保存已有 final segments。
- 不触发自动重新转写。
- 不触发自动 reconnect。

- [ ] **Step 2: 实现无自动 reconnect**

WebSocket / server error 后停止 ASR consumer，不重连。

- [ ] **Step 3: stop 后保存 record**

停止会议仍保存已有 final segments，summary 逻辑沿用现有 V1。

- [ ] **Step 4: batch provider 错误边界**

batch provider 在 pause / stop 后转写失败时：

- 已保存 final segments 保留。
- meeting 状态进入 `transcribing_interrupted`。
- audio retention（音频保留）仍按 V1 策略处理。

- [ ] **Step 5: 验证**

Run:

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo test meeting"
```

Expected: PASS。

### Task 8: Summary / search / export 边界

**Files:**

- Modify: `openless-all/app/src/pages/Meetings.tsx`
- Modify: `openless-all/app/src-tauri/src/coordinator/meeting_summary.rs`
- Modify: `openless-all/app/src-tauri/src/commands/meetings.rs`
- Test: meeting summary / export related tests

- [ ] **Step 1: summary 只使用 final segments**

确认 summary generation（总结生成）读取 `record.transcriptSegments`，不读取 draft event state。

- [ ] **Step 2: search 不包含 draft**

会议列表 search（搜索）只匹配 persisted `MeetingRecord`，不匹配当前 draft。

- [ ] **Step 3: Markdown export 不包含 draft**

导出的总结和转写原文只使用 persisted final segments。

- [ ] **Step 4: metadata 不污染用户可见 Markdown**

Markdown 仍显示：

```text
[发言人][时间] 发言内容
```

不输出 providerId、providerSessionId、sequence 等调试 metadata。

- [ ] **Step 5: 验证**

Run:

```powershell
cd D:\codex项目\VOXHUB\openless-all\app
.\node_modules\.bin\tsc.CMD --noEmit
npm run build
```

Run:

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo test meeting"
```

Expected: PASS。

### Task 9: 最终验证

**Files:**

- All V2-1 modified files.

- [ ] **Step 1: 前端验证**

```powershell
cd D:\codex项目\VOXHUB\openless-all\app
.\node_modules\.bin\tsc.CMD --noEmit
npm run build
```

- [ ] **Step 2: Rust 验证**

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo test meeting"
```

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo test credentials"
```

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo test preferences"
```

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo test bailian"
```

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo check"
```

- [ ] **Step 3: diff 验证**

```powershell
git diff --check
git status --short
git diff --stat
```

## 11. 人工验证计划

1. 设置 -> 服务 -> 会议 ASR 默认显示“继承全局 ASR”。
2. 切换成单独选择后，可以看到所有 ASR provider。
3. 选择 `bailian` / `fun-asr-realtime` 时出现 silence preset（静音档位）。
4. 会议页不出现 ASR 状态面板。
5. 开始会议后，实时识别内容显示在底部“正在识别”行。
6. final segment 到达后，“正在识别”行清空，并追加正式原文。
7. pause 后 draft 清空；resume 后新 ASR session 继续同一会议。
8. pause / resume 后，第二段 final segment 的时间戳继续沿用会议全局有效音频时间轴，不从 UI 上回到 00:00。
9. 网络中断后录音继续，状态显示 `transcribing_interrupted`，已保存原文不消失。
10. stop 后会议记录保存，已有 final segments 可用于 summary。
11. 短口述仍使用全局 ASR 设置，不受会议 ASR 设置影响。
12. 修改会议 ASR 设置不会影响正在进行的会议，只影响下一场会议。
13. 选择 batch provider 时，不显示“正在识别”行，pause / stop 后按 V1 降级策略落段。

## 12. 风险与回退

- 如果 `CredentialsVault` 按 provider id 读取改动风险高，先把 V2-1 限制为 inherit global；但这会不满足“会议专用设置”目标，必须标记为 blocker（阻塞）。
- 如果 `bailian` 的 `words` 字段结构与官方文档示例差异较大，先保存 sentence timestamp（句级时间戳），word timestamp（词级时间戳）保持空数组，并记录 provider payload 解析缺口；不要保存完整原始 payload。
- 如果阿里云官方 `fun-asr-realtime` API 字段名或 `max_sentence_silence` 取值边界变化，先调整 provider parser（服务商解析器）和测试 payload，不要改 UI 范围或新增高级配置。
- 如果 draft event 导致 UI 频繁刷新卡顿，前端按 meeting id 做 100-200ms throttle（节流），但 final segment 不节流。
- 如果 `cargo fmt --check` 仍被既有无关文件阻塞，只格式化 V2-1 touched Rust files（本阶段改过的 Rust 文件），不要全量格式化无关目录。
- 如果 provider-specific credentials（按服务商读取凭据）无法安全实现，不要悄悄切全局 active ASR；必须把“会议专用 provider 不影响短口述”标为 blocker（阻塞）。
- 如果 timebase（时间轴）换算无法在当前 meeting session state 中可靠实现，不能进入 V2-2；V2-2 speaker diarization alignment 会依赖这份数据。
- 如果 `activeProviderSessionId` 无法可靠下发到前端，前端不要渲染 draft，只保留 final segment 行为；否则会有跨 session draft 污染风险。

## 13. V2-1 验收清单

- [ ] 会议 ASR 有独立设置，默认继承全局 ASR。
- [ ] 会议 ASR 可选择所有现有 provider。
- [ ] 会议设置复用现有 CredentialsVault，不新增密钥系统。
- [ ] provider-specific credentials（按服务商读取凭据）不改变全局 active ASR。
- [ ] 不暴露 Workspace ID / region / 会议专用 endpoint。
- [ ] Provider capabilities 决定 UI 能否显示 silence preset 和 realtime 行为。
- [ ] `bailian` / `fun-asr-realtime` 支持 draft + final。
- [ ] Draft 只显示，不持久化、不总结、不导出。
- [ ] Final segment 持久化为 `TranscriptSegment`。
- [ ] `TranscriptSegment.metadata` 保存 provider/session/sequence/timestamp。
- [ ] `TranscriptSegment.metadata` 保存 `audioPartIndex` 和 `sessionStartMs`，支持后续按音频分片对齐。
- [ ] `TranscriptSegment.startMs/endMs` 使用 meeting-relative active audio time，不直接使用 provider session 内时间。
- [ ] `MeetingRecordingSnapshot.activeProviderSessionId` 支持前端 draft stale guard。
- [ ] pause / resume 跨 session 边界清楚。
- [ ] active meeting 期间 provider 和 silence preset 锁定，设置变更只影响下一场会议。
- [ ] batch provider 不显示假 draft，pause / stop 后按 V1 降级策略落段。
- [ ] 网络中断后继续录音，状态进入 `transcribing_interrupted`。
- [ ] summary、search、Markdown export 都只读取 final persisted segments，不读取 draft。
- [ ] browser mock 与 Tauri IPC 类型同形，设置页和会议页在浏览器预览可编译。
- [ ] V2-1 不执行 speaker diarization。
- [ ] V2-1 不做上传音频。
- [ ] V2-1 不做 system audio capture。
- [ ] V2-1 不新增或重构 V1 已有重新转写入口。
- [ ] 短口述主链路行为不变。
