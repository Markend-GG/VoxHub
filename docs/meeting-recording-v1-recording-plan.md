# 会议录音总结 V1-2 录音与原文生成执行计划

状态：ready for implementation
日期：2026-07-04
前置计划：`docs/meeting-recording-v1-storage-plan.md`

## 目标

实现 V1-2「会议录音与原文生成」的最小闭环：用户可以开始会议录音、暂停、继续、停止，并在停止后保存一条包含会议原文 `TranscriptSegment`（转写片段）的本地 `MeetingRecord`（会议记录）。

V1-2 停止后优先保存实时 `ASR`（Automatic Speech Recognition，语音转文字）文本，不调用 `LLM`（Large Language Model，大语言模型）生成总结，不实现会议页面。前端只补齐 `IPC`（Inter-Process Communication，进程间调用）wrapper（封装函数）和事件类型，供后续 V1-3 页面接入。

## 范围

- 支持开始会议录音。
- 支持暂停 / 继续。
- 支持停止会议并保存会议记录。
- V1 只录麦克风，不采集系统声音。
- 复用当前全局 `ASR provider`（语音转文字服务商）和语言偏好，不做会议专用 ASR 配置。
- 使用 `ASR final segment`（最终识别片段）生成 `TranscriptSegment`。
- V1 默认 `speakerLabel` 为“未区分”，只预留说话人字段。
- 录音期间实时追加原文段落。
- `ASR` 中断时保留已识别原文，会议状态标为 `transcribing_interrupted`，允许继续录音。
- 录音期间始终保留临时音频。
- 停止后按 V1-1 的会议音频保留数量策略处理：
  - 默认保留最近 20 场会议原始音频。
  - 最大 100。
  - 设置为 0 时停止后清理音频，只保留文本记录。
- 停止后只保存原文，不做标题/摘要/待办生成。标题使用可预测降级标题，例如 `会议记录 2026-07-04 15:30`。

## 不做项

- 不做 UI 页面。
- 不做 `LLM` 总结、标题提炼、摘要、关键结论、待办、风险/未决问题。
- 不做 `VAD`（Voice Activity Detection，语音活动检测）。
- 不做人声分离 / `speaker diarization`（说话人分离）。
- 不做系统声音采集 / `system audio capture`（系统声音采集）。
- 不做字幕级低延迟流式 / `subtitle-grade streaming`（字幕级低延迟逐字刷新）。
- 不做实时标点恢复 / `punctuation restoration`（标点恢复）。
- 不做会议专用 `ASR` / `LLM` 配置。
- 不做音频手动重新转写 UI。V1-2 只保证音频与原文数据为后续重转写入口保留条件。
- 不改现有短口述主链路行为；如必须抽公共 helper（辅助函数），只抽录音启动、ASR 构造、转写收尾这类最小复用点，并用短口述回归测试验证。

## 需要读取的现有代码入口

实施前必须重新读取这些文件，不能只按本计划记忆开发：

- `openless-all/app/src-tauri/src/types.rs`
  - 已有 `MeetingRecord`、`MeetingStatus`、`MeetingAudioState`、`TranscriptSegment`、`TranscriptSegmentSource`、`meeting_audio_retention_count`。
- `openless-all/app/src-tauri/src/persistence/meeting.rs`
  - 已有 `MeetingStore`（会议存储）和 `prune_audio_retention`（音频保留清理）。
- `openless-all/app/src-tauri/src/persistence/paths.rs`
  - 已有 `meeting_recording_path_for_id`（会议音频路径）。
- `openless-all/app/src-tauri/src/commands/meetings.rs`
  - 已有会议 CRUD（创建、读取、更新、删除）IPC。
- `openless-all/app/src-tauri/src/recorder.rs`
  - 复用 `Recorder::start`，它只采集麦克风，输出 16 kHz mono int16 PCM（16k 单声道 16 位 PCM），并可同时写 WAV 音频归档。
- `openless-all/app/src-tauri/src/asr/mod.rs`
  - 复用 `RawTranscript` 和 `AudioConsumer`（音频消费者）抽象。
- `openless-all/app/src-tauri/src/coordinator.rs`
  - 读取 `ActiveAsr`（当前 ASR 会话枚举）、`Inner`（全局协调器内部状态）、ASR timeout（超时）函数、`DeferredAsrBridge`（延迟连接音频桥）。
- `openless-all/app/src-tauri/src/coordinator/asr_wiring.rs`
  - 复用或抽取 `ensure_microphone_permission`（麦克风权限检查）、`ensure_asr_credentials`（ASR 凭据检查）、`build_qa_asr_start`（按全局 ASR provider 构造 ASR）的逻辑。
- `openless-all/app/src-tauri/src/coordinator/dictation.rs`
  - 参考短口述启动 recorder、停止 recorder、flush ASR、调用 `transcribe` / `await_final_result` 的完整路径，但不要复用它的 polish（润色）、insert（插入）和 history（历史）流程。
- `openless-all/app/src-tauri/src/coordinator/qa_session.rs`
  - 参考 QA 独立 recorder + ASR 资源管理方式。
- `openless-all/app/src-tauri/src/coordinator/resources.rs`
  - 参考按 `session_id` 守卫资源的模式。
- `openless-all/app/src/lib/types.ts`
  - 前端已有会议类型镜像。
- `openless-all/app/src/lib/ipc/meetings.ts`
  - 前端已有会议 CRUD wrapper。

## 推荐架构

### 总体建议

新增独立会议录音链路，不把会议 session（会话）塞进短口述 `SessionState`。推荐新增后端模块：

- `openless-all/app/src-tauri/src/meeting/mod.rs`
- `openless-all/app/src-tauri/src/meeting/session.rs`
- `openless-all/app/src-tauri/src/meeting/asr.rs`
- `openless-all/app/src-tauri/src/meeting/manager.rs`

如果实现时希望更保守，可以先用单文件 `openless-all/app/src-tauri/src/meeting_recording.rs`，但内部仍按 session、ASR、manager 三个区域拆分。不要把会议逻辑继续堆进 `coordinator/dictation.rs`。

### 后端职责划分

- `meeting/session.rs`
  - 纯状态机，无 Tauri 依赖。
  - 定义 `MeetingSessionPhase`（会议会话阶段）、状态流转函数、暂停时长计算、segment id 分配。
- `meeting/asr.rs`
  - 会议版 ASR 启动与收尾。
  - 复用全局 `ASR provider` 和语言偏好。
  - 抽公共 helper 时必须保持短口述行为不变。
- `meeting/manager.rs`
  - 管理 active meeting（活跃会议）、recorder、ASR、临时音频、`MeetingStore` 更新和 Tauri event（事件）发送。
- `commands/meetings.rs`
  - 在现有 CRUD 命令旁新增录音控制命令。

### 全局状态

新增一个 Tauri managed state（托管状态）：

```rust
pub struct MeetingRecordingManager {
    active: Mutex<Option<ActiveMeetingSession>>,
}
```

`ActiveMeetingSession` 建议包含：

```rust
pub struct ActiveMeetingSession {
    pub meeting_id: String,
    pub phase: MeetingSessionPhase,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub paused_at: Option<DateTime<Utc>>,
    pub accumulated_paused_ms: u64,
    pub transcript_segments: Vec<TranscriptSegment>,
    pub next_segment_index: u64,
    pub audio_archive_active: bool,
    pub asr_interrupted: bool,
    pub active_provider: String,
}
```

资源句柄不要放进可序列化 snapshot（快照）里。`Recorder` 和 `ActiveAsr` 应作为 manager 内部资源单独保存，或包在只后端可见的 runtime struct（运行时结构）里。

### 与短口述主链路的隔离

V1-2 必须避免会议和短口述同时抢麦克风。推荐实现一个最小互斥策略：

- 开始会议前检查短口述是否处于 recording / starting / processing（录音、启动、处理中）状态。
- 如果短口述活跃，`start_meeting_recording` 返回 `"dictation is active"`。
- 会议录音中，短口述启动也应被拒绝。实现时可在短口述 `begin_session` 开头增加只读检查：如果会议 manager 有 active meeting，返回明确错误。
- 这属于最小复用点，必须在计划执行时列入短口述回归验证。

不要复用短口述 `end_session`，因为它会触发 polish（润色）、insert（插入）、history（历史记录）等会议 V1-2 不需要的行为。

## 数据流

### 开始录音

1. 前端调用 `start_meeting_recording`。
2. 后端检查：
   - 没有 active meeting。
   - 短口述未占用麦克风。
   - 麦克风权限可用。
   - 当前全局 `ASR provider` 凭据或本地模型状态可用。
3. 生成 `meeting_id`。
4. 创建初始 `MeetingRecord`：
   - `status = recording`
   - `title = "会议记录 YYYY-MM-DD HH:mm"`
   - `transcriptSegments = []`
   - `summary = MeetingSummary::default()`
   - `audio.state = temporary`
   - `audio.retained = false`
   - `audio.path = null`，后端通过 meeting id 查找 `meeting-recordings/<meeting_id>/` 分段音频目录，避免前端依赖本地绝对路径。
5. 调用 `MeetingStore::create` 保存初始记录。
6. 使用 `meeting-recordings/<meeting_id>/part-0001.wav` 作为 `Recorder::start` 的 `audio_archive_path`。V1-2 需要新增 part path helper（分段路径辅助函数），不要直接复用单文件路径覆盖前一段音频。
7. 构造会议 ASR consumer（消费者），把 recorder PCM 同时送入 ASR 和 WAV archiver（归档器）。
8. 返回 `MeetingRecordingSnapshot`（会议录音快照），并 emit `meeting:state`。

### 录音中追加原文

1. `ASR provider` 产生 `final segment`（最终识别片段）。
2. manager 生成 `TranscriptSegment`：
   - `id = "seg-<递增序号>"`
   - `speakerLabel = "未区分"`
   - `startMs = 当前片段起始时间`
   - `endMs = 当前片段结束时间`，无法准确获得时允许使用收到 final 时的 elapsed（已过时长）
   - `text = ASR final text`
   - `source = realtime_asr`
3. 将 segment 追加到 active session 内存状态。
4. 同步更新 `MeetingStore` 中对应 `MeetingRecord.transcriptSegments`。
5. emit `meeting:transcript-segment`，供 V1-3 页面实时追加。

### 暂停

1. 前端调用 `pause_meeting_recording(meeting_id)`。
2. 后端验证 active meeting id 匹配且当前为 `recording` 或 `transcribing_interrupted`。
3. 停止当前 recorder。
4. 对当前 ASR session 做 flush（冲刷）：
   - streaming provider（流式服务商）调用 `send_last_frame` + `await_final_result`。
   - batch provider（批处理服务商）调用 `transcribe`。
5. 将 pause 前尚未落库的最终文本追加为 `TranscriptSegment`。
6. 状态切到 `paused`，记录 `paused_at`。
7. 更新 `MeetingStore` 并 emit `meeting:state`。

### 继续

1. 前端调用 `resume_meeting_recording(meeting_id)`。
2. 后端验证 active meeting id 匹配且当前为 `paused`。
3. 累加暂停时长到 `accumulated_paused_ms`。
4. 重新构造新的 ASR session，并重新启动 recorder。
5. 使用同一个会议音频路径继续归档时要特别验证 WAV append 行为。当前 `Recorder::start` 会创建新 WAV 文件，因此 V1-2 推荐改用“分段临时音频路径”方案，避免覆盖 pause 前音频。

### 停止

1. 前端调用 `stop_meeting_recording(meeting_id)`。
2. 后端验证 active meeting id 匹配。
3. 停止 recorder。
4. flush 当前 ASR session，把最终文本追加为 `TranscriptSegment`。
5. 计算 `durationMs = endedAt - startedAt - accumulatedPausedMs`。
6. 根据是否发生过 ASR 中断决定状态：
   - 无中断：`completed`
   - 中断且未重新成功恢复：`transcribing_interrupted`
7. 处理会议音频：
   - archive 成功且 retention count（保留数量）大于 0：`audio.state = retained`，`audio.retained = true`。
   - archive 成功但 retention count 为 0：删除音频，`audio.state = pruned`，`audio.retained = false`，`audio.path = null`。
   - archive 创建失败：`audio.state = unavailable`，`audio.retained = false`，`audio.path = null`。
8. 更新 `MeetingStore`。
9. 调用 `MeetingStore::prune_audio_retention(retention_count)` 清理超出数量的旧会议音频。
10. 清空 active session，并 emit `meeting:state`。

## 状态流转

### 后端运行时状态

新增 `MeetingSessionPhase`（会议会话阶段）：

```rust
pub enum MeetingSessionPhase {
    Idle,
    Starting,
    Recording,
    Paused,
    Stopping,
    Completed,
    TranscribingInterrupted,
}
```

建议状态流：

```text
Idle
  -> Starting
  -> Recording
  -> Paused
  -> Recording
  -> Stopping
  -> Completed
```

异常状态流：

```text
Recording
  -> TranscribingInterrupted
  -> Recording
  -> Paused
  -> Recording
  -> Stopping
  -> Completed 或 TranscribingInterrupted
```

### `MeetingRecord.status` 映射

- `Starting`：可先落库为 `recording`，因为 V1-1 没有 `starting` 状态。
- `Recording`：`recording`
- `Paused`：`paused`
- `TranscribingInterrupted`：`transcribing_interrupted`
- `Stopping`：仍保持最近一次用户可见状态，最终写 `completed` 或 `transcribing_interrupted`。
- `Completed`：`completed`

### ASR 中断语义

`ASR` 中断不等于录音停止：

- recorder 继续运行时，临时音频继续写入。
- 已经确认的 `TranscriptSegment` 不清空。
- 会议记录状态更新为 `transcribing_interrupted`。
- 如果后续 resume 或重建 ASR 成功，可以继续追加新的 `TranscriptSegment`。
- 停止时如果本场会议曾发生 ASR 中断，且没有完整重转写覆盖，最终记录保持 `transcribing_interrupted`，以便后续手动重新转写功能接管。

## IPC 设计

### Rust commands

在 `openless-all/app/src-tauri/src/commands/meetings.rs` 新增：

```rust
#[tauri::command]
pub async fn start_meeting_recording(
    manager: tauri::State<'_, MeetingRecordingManager>,
) -> Result<MeetingRecordingSnapshot, String>

#[tauri::command]
pub async fn pause_meeting_recording(
    id: String,
    manager: tauri::State<'_, MeetingRecordingManager>,
) -> Result<MeetingRecordingSnapshot, String>

#[tauri::command]
pub async fn resume_meeting_recording(
    id: String,
    manager: tauri::State<'_, MeetingRecordingManager>,
) -> Result<MeetingRecordingSnapshot, String>

#[tauri::command]
pub async fn stop_meeting_recording(
    id: String,
    manager: tauri::State<'_, MeetingRecordingManager>,
) -> Result<MeetingRecord, String>

#[tauri::command]
pub fn get_active_meeting_recording(
    manager: tauri::State<'_, MeetingRecordingManager>,
) -> Result<Option<MeetingRecordingSnapshot>, String>
```

所有传入 id 复用 V1-1 的 UUID literal（UUID 字面量）校验规则，非法时返回 `"invalid meeting id"`。

### Snapshot 类型

新增 Rust + TypeScript mirror（镜像类型）：

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRecordingSnapshot {
    pub meeting: MeetingRecord,
    pub phase: MeetingRecordingPhase,
    pub elapsed_ms: u64,
    pub active_asr_provider: String,
    pub asr_interrupted: bool,
}
```

`MeetingRecordingPhase` 可与后端 runtime phase 分开，保持 IPC 稳定：

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingRecordingPhase {
    Starting,
    Recording,
    Paused,
    Stopping,
    TranscribingInterrupted,
}
```

### Tauri events

V1-2 无 UI，但为了满足“录音期间实时追加原文段落”，后端必须发事件，V1-3 页面再订阅：

- `meeting:state`
  - payload：`MeetingRecordingSnapshot`
  - 用于录音状态、时长、暂停/继续状态同步。
- `meeting:transcript-segment`
  - payload：
    ```rust
    pub struct MeetingTranscriptSegmentEvent {
        pub meeting_id: String,
        pub segment: TranscriptSegment,
    }
    ```
  - 用于实时追加原文段落。
- `meeting:error`
  - payload：
    ```rust
    pub struct MeetingErrorEvent {
        pub meeting_id: Option<String>,
        pub code: String,
        pub message: String,
    }
    ```
  - 用于后续 UI 展示错误。V1-2 可以只记录和 emit，不做页面消费。

### Frontend wrappers

修改 `openless-all/app/src/lib/ipc/meetings.ts`，新增：

```ts
export function startMeetingRecording(): Promise<MeetingRecordingSnapshot>
export function pauseMeetingRecording(id: string): Promise<MeetingRecordingSnapshot>
export function resumeMeetingRecording(id: string): Promise<MeetingRecordingSnapshot>
export function stopMeetingRecording(id: string): Promise<MeetingRecord>
export function getActiveMeetingRecording(): Promise<MeetingRecordingSnapshot | null>
```

同时在 `openless-all/app/src/lib/types.ts` 新增对应 TypeScript 类型。mock（模拟）实现只返回静态数据，不需要模拟真实录音。

## ASR final segment 设计

### 关键边界

现有 `RawTranscript` 是 ASR 结束后的整段文本，不是统一的 `final segment` 事件接口。V1-2 要实现“录音期间实时追加原文段落”，需要补一个 provider-neutral（跨服务商）的 segment sink（片段接收器）抽象。

推荐新增：

```rust
pub struct AsrFinalSegment {
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
}

pub type AsrFinalSegmentSink = Arc<dyn Fn(AsrFinalSegment) + Send + Sync>;
```

对 provider 的接入策略：

- Volcengine / Bailian 这类 streaming provider（流式服务商）：
  - 在收到 final 结果时调用 sink。
  - `await_final_result` 仍返回整段 `RawTranscript`，保持短口述行为不变。
- sherpa-onnx online（如果当前模型路径已有 final / endpoint 信息）：
  - 可以在 endpoint commit 时调用 sink。
  - 不要把 `local-asr-token` 当作会议正式原文来源；它是 token / partial（临时识别片段）层。
- Whisper / MiMo / Foundry / Qwen / Apple Speech 这类 batch provider（批处理服务商）：
  - 录音期间通常没有 final segment。
  - 在 pause / stop 时把 `RawTranscript.text` 作为一个 `TranscriptSegment` 追加。

这意味着 V1-2 的“实时追加”在不同 ASR provider 上是 best effort（尽力而为）：

- 支持 final callback（最终片段回调）的 provider：录音中实时追加。
- 只支持 batch（批处理）的 provider：暂停或停止时追加，仍保证最终原文保存。

验收时必须用至少一个支持 streaming final segment 的 provider 验证实时追加；同时用一个 batch provider 验证停止后保存原文。

## 音频归档方案

### 重要问题

当前 `Recorder::start(..., audio_archive_path)` 会创建一个 WAV 文件。暂停后继续如果复用同一个 path，可能覆盖前一段音频。V1-2 不应在不了解 `WavArchiver` 追加能力的情况下直接复用同一文件。

### 推荐方案

V1-2 使用分段音频文件：

```text
meeting-recordings/<meeting_id>/
  part-0001.wav
  part-0002.wav
  ...
```

停止后：

- 如果本次会议从未暂停，允许保留单文件 `meeting-recordings/<meeting_id>.wav`，但为了代码一致性，仍推荐统一分段目录。
- 如果有多个 part，先在 V1-2 保留分段文件，不强制合并 WAV。
- `MeetingAudioMeta.path` 指向目录或主索引路径时，需要在 V1-1 当前 `path: Option<String>` 语义上明确：V1-2 建议保持 `path = null`，后端统一通过 meeting id 查找音频，避免前端依赖本地绝对路径。
- `MeetingStore::prune_audio_retention` 当前按 `meeting_recording_path_for_id(id)` 删除单文件，V1-2 需要同步扩展为支持删除会议音频目录。测试必须覆盖单文件兼容和目录删除。

如果实现者选择直接合并 WAV，必须新增 WAV concat（拼接）测试，确认每段 header（文件头）不会被拼入 PCM 数据中。没有测试前不建议合并。

## 错误处理

### 开始失败

- 麦克风权限失败：不创建正式 active session，返回 `"microphone permission denied"` 或现有中文权限错误。
- ASR 凭据/模型缺失：不启动 recorder，返回现有 ASR 错误文案。
- recorder 启动失败：取消 ASR，会议记录如果已创建则更新为 `transcribing_interrupted` 或删除 draft。推荐最小策略：只有 recorder 成功后才创建 active session；若已写入初始记录，则更新为 `transcribing_interrupted` 并保留空原文。
- active meeting 已存在：返回 `"meeting recording already active"`。
- 短口述正在工作：返回 `"dictation is active"`。

### 录音中 ASR 中断

- 不停止 recorder。
- 记录 `asr_interrupted = true`。
- 更新 `MeetingRecord.status = transcribing_interrupted`。
- 保留已识别 `TranscriptSegment`。
- emit `meeting:error`，code 建议为 `asrInterrupted`。
- 后端可以尝试重新构造 ASR session；如果实现复杂，V1-2 允许只继续录音并等待停止后依赖临时音频重转写能力，但必须保留音频。

### 暂停失败

- recorder 停止成功但 ASR flush 失败：
  - 保留已有 segment。
  - 状态设为 `transcribing_interrupted`。
  - 会议仍进入 `paused` 或保持 `transcribing_interrupted`。推荐 IPC snapshot phase 显示 `transcribing_interrupted`，但允许 `resume`。

### 继续失败

- ASR 或 recorder 重新启动失败：
  - 保持会议在 `paused` 或 `transcribing_interrupted`，不丢已识别原文。
  - 返回明确错误。

### 停止失败

- recorder 停止本身应尽力完成，不应阻止保存会议记录。
- ASR flush 失败：
  - 保存已有 segment。
  - 最终 `status = transcribing_interrupted`。
- 音频归档失败：
  - 保存原文。
  - `audio.state = unavailable`。
- retention 清理失败：
  - 会议停止保存成功。
  - 返回或 emit 清理错误，日志包含路径，但不要删除会议文本记录。

## 测试计划

### Rust 单元测试

新增纯状态机测试，避免真实麦克风依赖：

- `meeting_session_starts_from_idle`
- `meeting_session_rejects_double_start`
- `meeting_session_pause_resume_accumulates_paused_duration`
- `meeting_session_stop_computes_duration_excluding_pause`
- `meeting_session_asr_interruption_preserves_segments`
- `meeting_session_resume_allowed_after_transcribing_interrupted`

新增 segment builder（片段构造）测试：

- 默认 `speakerLabel = "未区分"`。
- `source = realtime_asr`。
- 空文本不生成 segment。
- segment id 按 `seg-000001` 递增。
- 缺少 provider 时间戳时，用会议 elapsed（已过时长）填充 `endMs`。

新增音频保留测试：

- 删除 retained（保留）会议音频时兼容单文件和目录。
- retention count 为 0 时停止会议后清理音频但保留文本记录。
- 超出 retention count 时清理最旧会议音频，不删除 `MeetingRecord`。

新增 fake ASR / fake recorder（测试替身）测试：

- fake streaming ASR 在录音中触发 final segment，manager 追加并 emit。
- fake batch ASR 在 stop 时返回 `RawTranscript`，manager 保存一个 segment。
- fake ASR 中断后已有 segment 不丢，状态为 `transcribing_interrupted`。

### Rust 集成/回归测试

优先跑 targeted（定向）测试：

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& rustup run stable-x86_64-pc-windows-msvc cargo test meeting"
```

如果抽取了短口述公共 ASR helper，必须额外跑现有 coordinator 相关测试：

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& rustup run stable-x86_64-pc-windows-msvc cargo test coordinator"
```

### TypeScript 检查

```powershell
cd openless-all/app
.\node_modules\.bin\tsc.CMD --noEmit
```

### 手工验证

V1-2 无 UI，手工验证可通过临时前端调用、Tauri console（控制台）或后端测试命令完成：

- Windows 默认麦克风录一段中文短句，停止后 `MeetingRecord.transcriptSegments` 有内容。
- 暂停后继续，两个录音段落保存到同一会议。
- 暂停期间说话不应进入原文。
- 使用 streaming provider 验证录音中能收到 `meeting:transcript-segment`。
- 使用 batch provider 验证停止后至少能保存一段原文。
- 模拟 ASR 中断，确认已识别原文不丢，会议状态为 `transcribing_interrupted`。
- retention count 设为 0，停止后会议文本保留，音频被清理。
- retention count 设为 1，录两场后较旧会议音频被清理，文本记录仍存在。
- 会议录音中尝试短口述，应被拒绝或明确提示。
- 短口述录音中尝试开始会议，应被拒绝。

## 分步骤开发任务

### Task 1：补充会议录音类型与前端 mirror

文件：

- 修改 `openless-all/app/src-tauri/src/types.rs`
- 修改 `openless-all/app/src/lib/types.ts`

步骤：

1. 新增 `MeetingRecordingPhase`、`MeetingRecordingSnapshot`、`MeetingTranscriptSegmentEvent`、`MeetingErrorEvent`。
2. 在 TypeScript 中新增对应类型。
3. 添加 serde camelCase / snake_case 规则测试，确保 Rust IPC 输出和 TypeScript 字段一致。
4. 运行 `cargo test meeting_recording` 和 `tsc --noEmit`。

### Task 2：实现纯状态机

文件：

- 新建 `openless-all/app/src-tauri/src/meeting/mod.rs`
- 新建 `openless-all/app/src-tauri/src/meeting/session.rs`
- 修改 `openless-all/app/src-tauri/src/lib.rs` 或 `main` module export（模块导出位置按当前代码结构确定）

步骤：

1. 定义 `MeetingSessionPhase` 和纯状态结构。
2. 实现 start / pause / resume / interrupt / stop 的状态流转函数。
3. 实现 `TranscriptSegment` 构造 helper。
4. 写状态机和 segment builder 单测。
5. 运行 `cargo test meeting_session`。

### Task 3：抽取或复用 ASR 构造与收尾

文件：

- 新建 `openless-all/app/src-tauri/src/meeting/asr.rs`
- 可能修改 `openless-all/app/src-tauri/src/coordinator/asr_wiring.rs`
- 可能修改 `openless-all/app/src-tauri/src/coordinator.rs`

步骤：

1. 设计 `MeetingAsrStart`，返回 `ActiveAsr` 和 `Arc<dyn recorder::AudioConsumer>`。
2. 优先复用 `build_qa_asr_start`。如果 `pub(super)` 可见性阻碍，抽到 coordinator 可共享的最小 helper。
3. 实现 `flush_active_asr_to_raw_transcript`，集中处理 Volcengine/Bailian/Whisper/MiMo/Foundry/Sherpa/Qwen/Apple Speech 的 stop 逻辑。
4. 保持短口述和 QA 现有行为不变。
5. 跑 `cargo test coordinator`，确认抽取没有破坏短口述。

### Task 4：接入 final segment sink

文件：

- 修改 `openless-all/app/src-tauri/src/asr/volcengine.rs`
- 修改 `openless-all/app/src-tauri/src/asr/bailian.rs`
- 视当前能力修改 `openless-all/app/src-tauri/src/asr/local/sherpa_provider.rs`
- 修改 `openless-all/app/src-tauri/src/asr/mod.rs`
- 修改 `openless-all/app/src-tauri/src/coordinator/asr_wiring.rs`

步骤：

1. 在 `asr/mod.rs` 定义 `AsrFinalSegment` 和 `AsrFinalSegmentSink`。
2. 给支持 streaming final 的 provider 增加 optional sink（可选片段接收器），默认 None，短口述不传 sink。
3. 在 provider 收到 final 结果时调用 sink。
4. batch provider 不强行模拟实时 final，保持 pause / stop 时生成整段 segment。
5. 单测 Volcengine/Bailian final 结果会调用 sink。

### Task 5：实现 MeetingRecordingManager

文件：

- 新建 `openless-all/app/src-tauri/src/meeting/manager.rs`
- 修改 `openless-all/app/src-tauri/src/lib.rs`

步骤：

1. 新增 `MeetingRecordingManager` 并在 Tauri setup 中 `.manage(...)` 注册。
2. 实现 `start`：
   - 权限检查。
   - 全局 ASR provider 检查。
   - 创建 meeting record。
   - 启动 recorder + ASR。
   - emit `meeting:state`。
3. 实现 `pause`：
   - 停 recorder。
   - flush ASR。
   - 落 segment。
   - 更新状态。
4. 实现 `resume`：
   - 新建 ASR session。
   - 新建音频 part。
   - 继续同一 meeting id。
5. 实现 `stop`：
   - 停 recorder。
   - flush ASR。
   - 更新 final record。
   - 按 retention 策略处理音频。
   - 清空 active session。
6. 使用 fake recorder/fake ASR 做 manager 单测，不依赖真实麦克风。

### Task 6：扩展会议音频保留清理

文件：

- 修改 `openless-all/app/src-tauri/src/persistence/paths.rs`
- 修改 `openless-all/app/src-tauri/src/persistence/meeting.rs`
- 修改 `openless-all/app/src-tauri/src/commands/meetings.rs`

步骤：

1. 增加会议音频目录 helper，例如 `meeting_recording_dir_for_id`。
2. `delete_meeting_record` 删除会议音频时兼容单文件和目录。
3. `prune_audio_retention` 兼容单文件和目录。
4. retention 测试覆盖：
   - 单文件删除。
   - 目录删除。
   - retention=0。

### Task 7：新增录音 IPC 和前端 wrapper

文件：

- 修改 `openless-all/app/src-tauri/src/commands/meetings.rs`
- 修改 `openless-all/app/src-tauri/src/commands/mod.rs`
- 修改 `openless-all/app/src-tauri/src/lib.rs`
- 修改 `openless-all/app/src/lib/ipc/meetings.ts`
- 修改 `openless-all/app/src/lib/ipc/index.ts`
- 修改 `openless-all/app/src/lib/ipc/mock-data.ts`

步骤：

1. 新增 start/pause/resume/stop/getActive commands。
2. 注册 desktop 和 mobile invoke handler。
3. 前端 wrapper 新增对应函数。
4. mock 返回静态 `MeetingRecordingSnapshot`。
5. 运行 Rust targeted tests 和 `tsc --noEmit`。

### Task 8：短口述互斥回归

文件：

- 可能修改 `openless-all/app/src-tauri/src/coordinator/dictation.rs`
- 可能修改 `openless-all/app/src-tauri/src/coordinator.rs`
- 修改或新增相关 tests

步骤：

1. 会议开始前检查短口述 active 状态。
2. 短口述开始前检查会议 active 状态。
3. 错误文案保持明确但不引入 UI。
4. 跑 coordinator 与 meeting 相关测试。

### Task 9：最终验证

步骤：

1. 运行：
   ```powershell
   git status --short
   ```
2. 运行 Rust 定向测试：
   ```powershell
   cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& rustup run stable-x86_64-pc-windows-msvc cargo test meeting"
   ```
3. 如果改动 coordinator 公共 helper，运行：
   ```powershell
   cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& rustup run stable-x86_64-pc-windows-msvc cargo test coordinator"
   ```
4. 运行 TypeScript：
   ```powershell
   cd openless-all/app
   .\node_modules\.bin\tsc.CMD --noEmit
   ```
5. 查看 diff：
   ```powershell
   git diff --stat
   ```
6. 不自动 commit。只有用户明确要求提交后，才精确 stage V1-2 文件。

## 验收标准

- 可以通过 IPC 开始会议录音。
- 开始会议时只采集麦克风，不采集系统声音。
- 会议开始使用当前全局 `ASR provider` 和语言偏好。
- 可以暂停录音，暂停期间不采集音频，也不产生新原文。
- 可以继续录音，继续后仍写入同一 `MeetingRecord`。
- 可以停止会议，停止后保存会议记录。
- `TranscriptSegment` 默认 `speakerLabel = "未区分"`。
- 支持 `ASR final segment` 的 provider 在录音期间 emit `meeting:transcript-segment`。
- batch provider 至少在 pause / stop 后保存最终原文段落。
- `ASR` 中断时已识别原文不丢，会议状态标为 `transcribing_interrupted`，录音可继续。
- 停止后不调用 `LLM`，不生成总结待办。
- 停止后按 V1-1 retention count 处理会议音频，文本记录始终保留。
- 会议和短口述不会同时抢麦克风。
- 现有短口述主链路行为不变。

## 风险与需要人工验证的点

### 最大技术风险

最大风险是现有 ASR 抽象没有统一的 `final segment` 实时回调。当前很多 provider 只在停止后返回 `RawTranscript`，而用户期望录音期间实时追加段落。V1-2 必须接受 provider 能力差异：

- streaming provider：录音中实时追加。
- batch provider：暂停或停止后追加。

如果强行要求所有 provider 都实时追加，会变成新增 ASR 管线重构，风险和范围都会超过 V1-2。

### 其他风险

- 暂停 / 继续会创建多个 recorder + ASR session，音频归档如果沿用单文件会覆盖，需要分段目录方案。
- 会议和短口述共享麦克风、ASR runtime（运行时）和本地模型缓存，互斥处理不完整会造成资源竞争。
- `ASR` 中断后继续录音时，实时文本可能断段；后续重新转写需要依赖音频保留能力补齐。
- 本地 ASR 首次加载较慢，开始会议可能出现明显延迟；V1-2 不新增会议专用 loading UI，只通过 IPC 错误和 state 表达。
- Windows 麦克风权限、设备占用、音频 callback（回调）静默停止需要真实设备验证，单测不能覆盖。
- 分段音频目录会影响 V1-1 已实现的 retention 和 delete，需要补兼容测试。

### 需要人工验证

- Windows 麦克风真实录音。
- 当前全局在线 ASR provider 的 streaming final segment 是否能实时到达。
- 当前全局本地 ASR provider 停止后是否能保存最终原文。
- 暂停期间讲话不会进入原文。
- 继续后仍写入同一会议。
- ASR 中断模拟后会议不停止、原文不清空。
- retention count 为 0、1、20 时音频清理行为正确。
- 会议录音中短口述不能启动；短口述中会议不能启动。

## 与后续版本的边界

V1-2 只为后续能力保留数据结构和事件，不实现后续能力：

- V1-3 才做会议页面、列表、详情、实时滚动区域。
- V1-4 才做 `LLM` 总结、待办、标题生成、长会 rolling context（滚动上下文）。
- V2 或后续单独计划再做 `VAD`、`speaker diarization`、系统声音采集、实时标点、字幕级低延迟流式、会议专用 ASR 配置。
