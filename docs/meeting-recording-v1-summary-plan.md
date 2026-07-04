# 会议录音总结 V1-4 总结生成执行计划

状态：ready for implementation planning
日期：2026-07-04
前置计划：

- `docs/meeting-recording-v1-storage-plan.md`
- `docs/meeting-recording-v1-recording-plan.md`
- `docs/meeting-recording-v1-ui-plan.md`

## 1. 背景与目标

V1-1 已落地 `MeetingRecord`（会议记录）、`TranscriptSegment`（会议原文片段）、`MeetingSummary`（会议总结）、结构化 `MeetingTodo`（会议待办）和本地 JSON 存储。

V1-2 已完成会议录音、暂停、继续、停止和原文生成。停止后目前只保存原文和音频保留状态，不调用 `LLM`（Large Language Model，大语言模型）。

V1-3 已接入会议列表、详情、录音控制和原文展示 UI，并为 `summarizing` / `summary_failed` 状态预留了基础状态文案，但还没有 summary 展示区、重试按钮或 summary 生成 `IPC`（Inter-Process Communication，进程间调用）。

V1-4 目标是完成“停止会议后基于已保存原文生成会议总结”的最小闭环：复用当前全局 `LLM provider`（大语言模型服务商），生成标题、overview（概览）、keyDecisions（关键决定）、todos（待办事项）和 risksAndOpenQuestions（风险与开放问题），失败时保留原文并允许重试。

## 2. V1-4 范围判断

结论：V1-4 应进入「会议总结生成」阶段。

依据：

- `docs/meeting-recording-v1-spec.md` 明确写入 V1 目标：停止后基于原文自动生成会议标题、摘要、关键结论、结构化待办、风险/未决问题。
- `docs/meeting-recording-v1-implementation-plan.md` 将 V1 第 4 步定义为“会后总结、待办与长会处理”。
- `docs/meeting-recording-v1-ui-plan.md` 把停止后 `LLM` 总结、标题自动生成、结构化 todo、风险/未决问题、rolling context（滚动上下文）和失败重试放入 V1-4。

执行时需要按最新 V1-3 范围继续收窄：V1-4 只做总结生成和展示闭环，不补做早期 spec 中的编辑、删除、Markdown 导出、音频重转写等更大记录管理能力。

## 3. 已确认决策

1. stop（停止会议）后自动触发 summary generation（总结生成）。
   - 后端在会议停止并保存 record 后触发 summary generation。
   - UI 需要通过 `meeting:summary` event（事件）或 refresh（刷新）看到 `summarizing`、`completed`、`summary_failed` 状态变化。

2. empty transcript（空原文）不调用 `LLM`，会议状态落 `summary_failed`。
   - 保留原文、音频状态和已有 summary。
   - 错误信息显示“会议原文为空，无法生成总结”。

3. `LLM` 可以生成会议标题，但只在标题仍是默认标题时覆盖。
   - 如果用户已手动改过标题，不覆盖。
   - V1-4 不新增 `titleEdited`（标题已编辑）字段；实现时用保守 helper（辅助函数）识别 V1-2 默认标题。无法确认是默认标题时，按用户标题处理，不覆盖。

4. V1-4 做最小 rolling context（滚动上下文）处理长原文。
   - 按字符阈值 chunk（分块），每块生成累计 notes（笔记），最后合成一次最终 JSON。
   - 不做中间笔记 UI。

5. V1-4 不做 cancel（取消总结）。
   - 失败或超时后进入 `summary_failed`。
   - 失败后只支持 retry（重试）。

## 4. 明确不做项

V1-4 不做：

- system audio capture（系统声音采集）。
- `VAD`（Voice Activity Detection，语音活动检测）。
- speaker diarization（说话人识别 / 说话人分离）。
- 音频重转写。
- 会议专用 `ASR`（Automatic Speech Recognition，语音转文字）配置。
- 会议专用 `LLM provider` 配置。
- 复杂富文本编辑器。
- 多会议批量总结。
- 云同步。
- 权限、账号、计费系统。
- Markdown / Word / PDF 导出。
- 删除会议记录。
- 第三方任务系统同步。
- 修改短口述主链路行为。

## 5. 当前代码结构观察

后端：

- `openless-all/app/src-tauri/src/types.rs`
  - 已有 `MeetingStatus`：`draft`、`recording`、`paused`、`transcribing_interrupted`、`summarizing`、`summary_failed`、`completed`。
  - 已有 `MeetingSummary`：`overview`、`keyDecisions`、`todos`、`risksAndOpenQuestions`。
  - 已有 `MeetingTodo`：`content`、`owner`、`dueDate`、`sourceSegmentIds`、`sourceQuote`。
- `openless-all/app/src-tauri/src/commands/meetings.rs`
  - 已有 list/get/create/update/delete 和 start/pause/resume/stop/getActive recording commands。
  - 缺少 `generate_meeting_summary` / `retry_meeting_summary`。
- `openless-all/app/src-tauri/src/coordinator/meeting.rs`
  - `stop_meeting_recording` 当前 flush（冲刷）ASR 后保存 record，最终状态为 `completed` 或 `transcribing_interrupted`，然后清空 active meeting runtime（运行时）。
  - 已有 `meeting:state`、`meeting:transcript-segment`、`meeting:error` event。
  - `meeting:state` payload（负载）是 `MeetingRecordingSnapshot`（会议录音快照），只适合 active recording（活跃录音），不适合 completed meeting（已完成会议）的总结进度通知。
- `openless-all/app/src-tauri/src/persistence/meeting.rs`
  - `MeetingStore` 支持 list/get/create/update/delete 和音频 retention（保留策略）清理。
- `openless-all/app/src-tauri/src/coordinator/polish_flow.rs`
  - `polish_text` 通过 `CredentialsVault::get_active_llm()` 复用全局 `LLM provider`，但 API 面向短口述 polish（润色）语义。
- `openless-all/app/src-tauri/src/polish.rs`
  - `ActiveLLMProvider` 已支持 OpenAI-compatible（OpenAI 兼容协议）和 Codex OAuth provider（Codex OAuth 服务商）。
  - 低层已有 chat completion（聊天补全）和 Codex Responses API（响应接口）路径，但部分 helper（辅助函数）是私有方法。

前端：

- `openless-all/app/src/lib/types.ts`
  - 已镜像 `MeetingSummary`、`MeetingTodo`、`MeetingStatus`。
- `openless-all/app/src/lib/ipc/meetings.ts`
  - 缺少 `generateMeetingSummary` / `retryMeetingSummary` wrapper（封装函数）。
- `openless-all/app/src/pages/Meetings.tsx`
  - 已展示列表、详情、录音控制和 transcript（原文）。
  - 已有 `summarizing` / `summary_failed` status label（状态标签），但未展示 summary 内容、loading（加载中）或 retry（重试）。
- `openless-all/app/src/i18n/*.ts`
  - 需要补齐 summary 展示、生成中、失败、重试、空态等五份文案。

接口缺口：

- 没有 summary generation command（总结生成命令）。
- 没有 summary generation event（总结生成事件）。
- 现有 `meeting:state` 语义不适合已停止会议总结更新。
- 现有 LLM 调用接口偏短口述 polish，需要新增通用 completion（补全）helper 或 meeting summary 专用 wrapper，避免污染短口述流程。

## 6. 推荐产品行为

停止会议后：

1. 如果最终会议状态是 `completed` 且 transcript 非空，自动进入 `summarizing`。
2. 如果 transcript 为空，不调用 `LLM`，直接写入 `summary_failed`。
3. UI 保留原文区，并在 summary 区显示“总结生成中”或失败提示。
4. 总结成功后状态回到 `completed`，展示 overview、key decisions、todos、risks/open questions，并按默认标题规则更新标题。
5. 总结失败后状态为 `summary_failed`，保留 transcript、音频状态和已有 summary，展示失败提示和 retry 按钮。
6. `transcribing_interrupted` 会议不自动触发总结；V1-4 UI 只提示原文可能不完整，不新增基于不完整原文的手动生成入口。

用户手动重试：

- 仅当 meeting 不在 `recording` / `paused` / active recording 状态时可用。
- `summary_failed` 必须可重试。
- `completed` 且 summary 为空时可生成。
- `completed` 且已有 summary 时不暴露 regenerate（重新生成）入口；V1-4 失败后只支持 retry（重试）。

空原文：

- 不调用 `LLM`。
- 写入 `summary_failed`，保留原文。
- `meeting:error` 或 summary event 返回 code：`emptyTranscript`。

过长原文：

- 后端自动 chunk（分块）处理，UI 不暴露复杂配置。
- 2 小时软限制仍不强制停止。

## 7. Summary 数据流设计

推荐新增后端纯流程：

```text
MeetingRecord
  -> validate_summary_input
  -> build_transcript_text
  -> generate_summary_with_active_llm
  -> parse_llm_json
  -> update MeetingRecord.title (only if default) + MeetingRecord.summary + MeetingRecord.status
  -> persist MeetingStore
  -> emit meeting:summary
```

数据源：

- 只读取已保存 `MeetingRecord.transcriptSegments`。
- 不读取音频，不重新转写。
- 不读取短口述 history（历史记录）。

输出：

- `MeetingRecord.title`，仅当当前标题仍是默认标题时覆盖
- `MeetingRecord.summary.overview`
- `MeetingRecord.summary.keyDecisions`
- `MeetingRecord.summary.todos`
- `MeetingRecord.summary.risksAndOpenQuestions`
- `MeetingRecord.status`

JSON contract（JSON 契约）：

```json
{
  "title": "string",
  "overview": "string",
  "keyDecisions": ["string"],
  "todos": [
    {
      "content": "string",
      "owner": null,
      "dueDate": null,
      "sourceSegmentIds": ["seg-000001"],
      "sourceQuote": "string"
    }
  ],
  "risksAndOpenQuestions": ["string"]
}
```

解析规则：

- 只接受 JSON object（对象），允许剥离 ```json code fence（代码块外壳）。
- `title` 和 `overview` trim（去除首尾空白）后不能为空；为空时使用降级值并记录 warning（警告）。
- `title` 只在 `is_default_meeting_title(record.title, record.startedAt)` 为 true 时写回；无法确认默认标题时不覆盖。
- 数组字段缺失时按空数组处理。
- `sourceSegmentIds` 只能引用输入 transcript segment id；无效 id 丢弃。
- `owner` / `dueDate` 无法判断时为 null，不允许模型编造。
- `sourceQuote` 必须来自相关片段的短摘录；解析后不做复杂 fuzzy match（模糊匹配），只做长度限制和 trim。

## 8. LLM provider 复用设计

V1-4 复用当前全局 `LLM provider` 和现有凭据槽：

- 读取 `CredentialsVault::get_active_llm()`。
- 使用现有 `ArkApiKey`、`ArkEndpoint`、`ArkModelId`、Codex OAuth、Gemini 等全局配置。
- 读取 `UserPreferences.llm_thinking_enabled`、`working_languages`、`chinese_script_preference`、`output_language_preference`。
- 不新增会议专用 provider、model、temperature、prompt template 配置。

推荐最小后端封装：

- 新增 `openless-all/app/src-tauri/src/coordinator/meeting_summary.rs`。
- 在该模块中实现 meeting summary 专用 orchestration（编排），但调用一个通用 `LLM` completion helper。
- 避免直接把 summary prompt 塞进 `polish_text`，因为 `polish_text` 会套用短口述 `PolishMode`（润色模式）、hotwords（热词）和 style prompt（风格提示词）语义。

需要的最小 LLM helper：

- 在 `openless-all/app/src-tauri/src/coordinator/polish_flow.rs` 或 `openless-all/app/src-tauri/src/polish.rs` 暴露一个小范围函数，例如：

```rust
pub(super) async fn complete_text_with_active_llm(
    system_prompt: &str,
    user_prompt: &str,
    working_languages: &[String],
    chinese_script_preference: ChineseScriptPreference,
    output_language_preference: OutputLanguagePreference,
    llm_thinking_enabled: bool,
) -> anyhow::Result<String>
```

实现要求：

- Gemini 继续走 `GeminiProvider` 原生路径。
- OpenAI-compatible / Codex 继续走 `build_active_llm_provider`。
- 不改变 `polish_text` 的签名和短口述调用路径。
- 如果必须改 `ActiveLLMProvider`，只新增 `complete_text` 方法，不改 `polish` / `translate_to` 行为。

## 9. Prompt（提示词）设计

System prompt（系统提示词）目标：

- 生成会议纪要，不做润色插入。
- 只输出 JSON，不输出 Markdown。
- 不编造未出现在 transcript 的结论、负责人、日期。
- 待办必须可追溯到原文片段。
- 输出语言遵守用户当前 output language preference（输出语言偏好）；如果为 auto，使用 transcript 主语言。

User prompt（用户提示词）包含：

- meeting metadata（会议元信息）：startedAt、endedAt、durationMs。
- transcript segments（原文片段）：id、speakerLabel、timestamp、text。
- JSON schema（JSON 结构）。
- 约束：无负责人/截止日期填 null；风险和开放问题合并到 `risksAndOpenQuestions`。

短会 prompt 示例：

```text
请根据下面的会议原文生成会议纪要。只输出一个 JSON object，不要输出 Markdown。

会议原文：
[seg-000001][未区分][00:00] 我们今天确认 V1-4 只做总结生成，不做导出。
[seg-000002][未区分][00:18] 待办是下周前把失败重试补上，负责人暂时没定。

输出 JSON schema：
{
  "title": "string",
  "overview": "string",
  "keyDecisions": ["string"],
  "todos": [
    {
      "content": "string",
      "owner": null,
      "dueDate": null,
      "sourceSegmentIds": ["seg-000001"],
      "sourceQuote": "string"
    }
  ],
  "risksAndOpenQuestions": ["string"]
}
```

长会 rolling context 方案：

1. 将 transcript 按字符数 chunk，例如每块 12k 到 20k chars，边界尽量落在 segment 之间。
2. 每块 prompt 输入上一轮 accumulated notes（累计笔记）和当前 chunk。
3. 每块输出 interim notes（中间笔记），包含 decisions/todos/risks 的候选项和 source segment ids。
4. 最后一轮把 accumulated notes 合成为最终 JSON。
5. V1-4 不展示 interim notes。

## 10. 状态流转设计

推荐 `MeetingRecord.status` 流转：

```text
completed
  -> summarizing
  -> completed

completed
  -> summarizing
  -> summary_failed

summary_failed
  -> summarizing
  -> completed

summary_failed
  -> summarizing
  -> summary_failed
```

禁止流转：

- `recording` / `paused` / active `transcribing_interrupted` 不允许触发 summary。
- 同一个 meeting 已经 `summarizing` 时拒绝重复触发，返回 `"meeting summary already running"`。
- meeting 不存在时返回 `"meeting not found"`。

失败语义：

- `LLM` 失败、超时、invalid JSON（无效 JSON）、empty transcript 都进入 `summary_failed`。
- 失败不清空 `transcriptSegments`。
- 失败不删除音频。
- 失败时保留已有 summary；如果没有已有 summary，保持 `MeetingSummary::default()`。
- V1-4 不提供 cancel（取消总结）；运行中的 summary job 只能自然成功、失败或超时。

并发控制：

- 在 `Coordinator::Inner` 中新增 `meeting_summary_jobs: Mutex<HashSet<String>>` 或同等轻量结构，避免同一 meeting 重复生成。
- V1-4 只强制拒绝同一 meeting 的重复 summary job（总结任务）。不新增面向用户的全局并发设置。

## 11. IPC 和 event 接入设计

推荐新增 Rust commands：

```rust
#[tauri::command]
pub async fn generate_meeting_summary(
    id: String,
    coord: CoordinatorState<'_>,
) -> Result<MeetingRecord, String>

#[tauri::command]
pub async fn retry_meeting_summary(
    id: String,
    coord: CoordinatorState<'_>,
) -> Result<MeetingRecord, String>
```

说明：

- `generate_meeting_summary` 用于 completed 且无 summary 的会议。
- `retry_meeting_summary` 用于 `summary_failed`。
- 两者后端可共用同一个 internal function（内部函数），但保留两个 `IPC` 名称让 UI 语义清晰。
- 不建议复用 `update_meeting_record` 做总结生成，因为 summary generation 是后端 `LLM` 副作用和状态机，不能由前端拼完整 record。

推荐新增 TypeScript wrappers：

```ts
export function generateMeetingSummary(id: string): Promise<MeetingRecord>
export function retryMeetingSummary(id: string): Promise<MeetingRecord>
```

推荐新增 event：

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSummaryEvent {
    pub meeting_id: String,
    pub status: MeetingStatus,
    pub meeting: Option<MeetingRecord>,
    pub error: Option<MeetingErrorEvent>,
}
```

event name：

- `meeting:summary`

payload 行为：

- 进入 `summarizing`：发送 `{ meetingId, status: "summarizing", meeting, error: null }`。
- 成功：发送 `{ meetingId, status: "completed", meeting, error: null }`。
- 失败：发送 `{ meetingId, status: "summary_failed", meeting, error }`。

仍可复用 `meeting:error`：

- 对失败额外 emit `meeting:error`，code 使用：
  - `emptyTranscript`
  - `summaryLlmFailed`
  - `summaryInvalidJson`
  - `summaryAlreadyRunning`

为什么不复用 `meeting:state`：

- 当前 `meeting:state` payload 是 `MeetingRecordingSnapshot`，包含 phase、elapsedMs、activeAsrProvider、asrInterrupted，语义绑定 active recording。
- summary 发生在 completed meeting 上，强行复用会让 UI 混淆录音状态和总结状态。

自动触发方案：

- `stop_meeting_recording` 成功保存 record 后，如果状态为 `completed`，后端自动触发 summary generation。
- transcript 为空时 summary generation 不调用 `LLM`，直接把 record 更新为 `summary_failed` 并 emit `meeting:summary`。
- `stop_meeting_recording` 返回值建议可以是刚进入 `summarizing` 的 record，也可以先返回 `completed` 再由 event 更新；计划建议前者，避免 UI 一瞬间显示完成但无 summary。

## 12. UI 接入点

修改 `openless-all/app/src/pages/Meetings.tsx`：

- 在 detail header（详情头部）下方、transcript 区上方新增 `SummarySection`。
- `record.status === 'summarizing'`：显示 summary loading。
- `record.status === 'summary_failed'`：显示失败提示和 retry button（重试按钮）。
- `record.status === 'completed'` 且 summary 非空：展示 overview、keyDecisions、todos、risksAndOpenQuestions。
- `record.status === 'completed'` 且 summary 为空：显示“暂无总结”。
- 订阅 `meeting:summary` event，按 meeting id upsert record。
- `stopMeetingRecording` 后如果返回 `summarizing` record，列表和详情立即更新。

展示规则：

- overview：普通段落。
- keyDecisions：简单 bullet list（项目列表）。
- todos：表格式或紧凑列表，字段为 content、owner、dueDate、sourceQuote。
- risksAndOpenQuestions：简单 bullet list。
- sourceSegmentIds：V1-4 只做轻量展示，可以显示来源片段 id；不做点击跳转，除非实现代价很低且不影响范围。

修改 `openless-all/app/src/i18n/*.ts`：

- `meetings.summaryTitle`
- `meetings.summaryLoading`
- `meetings.summaryFailed`
- `meetings.summaryRetry`
- `meetings.summaryRetrying`
- `meetings.summaryEmpty`
- `meetings.keyDecisions`
- `meetings.todos`
- `meetings.risksAndOpenQuestions`
- `meetings.todoOwner`
- `meetings.todoDueDate`
- `meetings.todoSource`

不做：

- 编辑 summary。
- 删除 meeting。
- 导出 Markdown。
- 音频播放器。
- 重新转写 UI。

## 13. 文件改动清单

新增：

- `docs/meeting-recording-v1-summary-plan.md`
- `openless-all/app/src-tauri/src/coordinator/meeting_summary.rs`
  - 会议 summary 状态机、默认标题识别、prompt builder、LLM 调用、JSON 解析、event emit。

修改：

- `openless-all/app/src-tauri/src/coordinator.rs`
  - 引入 `meeting_summary` 模块。
  - 在 `Inner` 增加 summary job 并发控制字段。
  - 暴露 `generate_meeting_summary` / `retry_meeting_summary` 方法。
- `openless-all/app/src-tauri/src/coordinator/meeting.rs`
  - stop 后按已确认策略触发自动 summary。
  - 注意不要改变录音、ASR、音频 retention 已有行为。
- `openless-all/app/src-tauri/src/coordinator/polish_flow.rs` 或 `openless-all/app/src-tauri/src/polish.rs`
  - 只新增通用 completion helper；不修改短口述 polish 行为。
- `openless-all/app/src-tauri/src/types.rs`
  - 新增 `MeetingSummaryEvent` 类型。
- `openless-all/app/src-tauri/src/commands/meetings.rs`
  - 新增 `generate_meeting_summary` / `retry_meeting_summary` commands。
- `openless-all/app/src-tauri/src/commands/mod.rs`
  - 如需要，重导出新增 event 类型或 command 所需类型。
- `openless-all/app/src-tauri/src/lib.rs`
  - 注册 desktop 和 mobile invoke handler。
- `openless-all/app/src/lib/types.ts`
  - 新增 `MeetingSummaryEvent` mirror。
- `openless-all/app/src/lib/ipc/meetings.ts`
  - 新增 summary IPC wrappers 和 mock。
- `openless-all/app/src/pages/Meetings.tsx`
  - 新增 summary section、retry action、`meeting:summary` 订阅。
- `openless-all/app/src/i18n/zh-CN.ts`
- `openless-all/app/src/i18n/zh-TW.ts`
- `openless-all/app/src/i18n/en.ts`
- `openless-all/app/src/i18n/ja.ts`
- `openless-all/app/src/i18n/ko.ts`
  - 补齐 summary UI 文案。

不修改：

- `openless-all/app/src-tauri/src/coordinator/dictation.rs`，除非新增通用 LLM helper 需要 import 调整；不得改变短口述流程。
- `openless-all/app/src-tauri/src/asr/`。
- 音频采集、VAD、speaker diarization、系统声音相关代码。

## 14. 分任务实施步骤

### Task 1：补齐 summary event 和 IPC 类型

文件：

- 修改 `openless-all/app/src-tauri/src/types.rs`
- 修改 `openless-all/app/src/lib/types.ts`

步骤：

1. 新增 `MeetingSummaryEvent` Rust 类型，字段为 `meetingId`、`status`、`meeting`、`error`。
2. 新增 TypeScript mirror。
3. 添加 serde（序列化）测试，确认字段为 camelCase（驼峰命名）且 status 为 snake_case（下划线命名）。
4. 运行：
   ```powershell
   cd openless-all/app/src-tauri
   cargo test meeting_summary_event
   ```

### Task 2：实现 prompt builder 和 JSON parser

文件：

- 新增 `openless-all/app/src-tauri/src/coordinator/meeting_summary.rs`

步骤：

1. 实现 `build_meeting_summary_prompt(record, prefs)`，输出 system/user prompt。
2. 实现 `format_transcript_segments(record)`，格式为 `[segmentId][speaker][mm:ss] text`。
3. 实现 `parse_meeting_summary_response(raw, valid_segment_ids)`。
4. 实现 `is_default_meeting_title(title, started_at)`，只识别 V1-2 默认标题格式。
5. 支持剥离 ```json code fence。
6. 验证 invalid segment id 会被丢弃。
7. 单测覆盖：
   - 完整 JSON 解析。
   - code fence 解析。
   - 缺失数组字段变空数组。
   - invalid JSON 返回明确错误。
   - invalid sourceSegmentIds 被过滤。
   - 默认标题会被 `LLM` 标题覆盖。
   - 非默认标题不会被覆盖。
8. 运行：
   ```powershell
   cd openless-all/app/src-tauri
   cargo test meeting_summary
   ```

### Task 3：新增通用 LLM completion helper

文件：

- 修改 `openless-all/app/src-tauri/src/coordinator/polish_flow.rs`
- 可能修改 `openless-all/app/src-tauri/src/polish.rs`

步骤：

1. 新增 `complete_text_with_active_llm(...)`。
2. Gemini 分支复用 `read_gemini_credentials` 和 `GeminiProvider`。
3. OpenAI-compatible / Codex 分支复用 `build_active_llm_provider`。
4. 如 `ActiveLLMProvider` 缺少通用入口，只新增 `complete_text(system_prompt, user_prompt, ...)`，不要改变 `polish`、`translate_to`、`answer_chat_streaming`。
5. 单测或现有 tests 覆盖 helper 不改变短口述 prompt composition（提示词组装）。
6. 运行：
   ```powershell
   cd openless-all/app/src-tauri
   cargo test polish
   cargo test coordinator
   ```

### Task 4：实现 summary 状态机和持久化

文件：

- 修改 `openless-all/app/src-tauri/src/coordinator/meeting_summary.rs`
- 修改 `openless-all/app/src-tauri/src/coordinator.rs`

步骤：

1. 在 `Inner` 新增 summary job tracking（任务跟踪）。
2. 实现 `generate_meeting_summary(inner, id, retry)`。
3. 读取 `MeetingStore::get(id)`。
4. 拒绝 active recording meeting。
5. 空 transcript 不调用 `LLM`，写 `summary_failed`。
6. 进入 `summarizing` 时先持久化 record 并 emit `meeting:summary`。
7. LLM 成功后解析 JSON，更新 title 和 summary，状态写 `completed`。
8. LLM 失败或解析失败，状态写 `summary_failed`，保留 transcript 和已有 summary。
9. 无论成功失败，释放 summary job tracking。
10. 单测覆盖状态流转、失败保留原文、重复触发被拒绝。

### Task 5：接入 stop 后自动触发

文件：

- 修改 `openless-all/app/src-tauri/src/coordinator/meeting.rs`
- 修改 `openless-all/app/src-tauri/src/coordinator/meeting_summary.rs`

步骤：

1. 在 `stop_meeting_recording` 保存 completed record 后自动触发 summary。
2. 如果 transcript 非空，自动触发前把 record 状态更新为 `summarizing` 并持久化，保证 UI 刷新一致。
3. 如果 transcript 为空，不调用 `LLM`，直接把 record 状态更新为 `summary_failed` 并 emit `meeting:summary`。
4. 用 `tauri::async_runtime::spawn` 异步执行非空 transcript 的 summary job，避免 stop command 长时间阻塞。
5. `transcribing_interrupted` 不自动触发，也不新增基于不完整原文的手动生成入口。
6. 单测或集成测试覆盖 stop 后非空 transcript 进入 `summarizing`，空 transcript 进入 `summary_failed`。

### Task 6：新增 Rust commands 和前端 IPC wrappers

文件：

- 修改 `openless-all/app/src-tauri/src/commands/meetings.rs`
- 修改 `openless-all/app/src-tauri/src/lib.rs`
- 修改 `openless-all/app/src/lib/ipc/meetings.ts`
- 修改 `openless-all/app/src/lib/ipc/index.ts`（如当前 export 需要补）

步骤：

1. 新增 `generate_meeting_summary` command。
2. 新增 `retry_meeting_summary` command。
3. 复用现有 `validate_meeting_id`。
4. 注册 desktop 和 mobile invoke handler。
5. TypeScript 新增 `generateMeetingSummary` / `retryMeetingSummary`。
6. mock 返回带 summary 的 `MeetingRecord`。
7. 运行：
   ```powershell
   cd openless-all/app
   .\node_modules\.bin\tsc.CMD --noEmit
   ```

### Task 7：前端展示 summary 和重试

文件：

- 修改 `openless-all/app/src/pages/Meetings.tsx`
- 修改 `openless-all/app/src/i18n/zh-CN.ts`
- 修改 `openless-all/app/src/i18n/zh-TW.ts`
- 修改 `openless-all/app/src/i18n/en.ts`
- 修改 `openless-all/app/src/i18n/ja.ts`
- 修改 `openless-all/app/src/i18n/ko.ts`

步骤：

1. 新增 `SummarySection` component。
2. 展示 `summarizing` loading。
3. 展示 `summary_failed` failed state 和 retry button。
4. 展示 completed summary 内容。
5. 订阅 `meeting:summary` event，upsert meeting。
6. retry 调用 `retryMeetingSummary(id)`，actionLoading 可新增 `'summary'`。
7. 补齐五份 i18n key。
8. 运行 `tsc --noEmit` 和 `npm run build`。

### Task 8：长 transcript 最小 rolling context

文件：

- 修改 `openless-all/app/src-tauri/src/coordinator/meeting_summary.rs`

步骤：

1. 新增 chunk splitter（分块器），按 segment 边界控制 chunk 字符数。
2. transcript 未超过阈值时走单次 summary。
3. transcript 超过阈值时走 interim notes + final merge。
4. 单测覆盖 chunk 不切断 segment、final prompt 带入 previous notes。
5. 不新增 UI。

### Task 9：最终验证

命令：

```powershell
git status --short
```

```powershell
cd openless-all/app
.\node_modules\.bin\tsc.CMD --noEmit
npm run build
```

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& rustup run stable-x86_64-pc-windows-msvc cargo test meeting_summary"
```

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& rustup run stable-x86_64-pc-windows-msvc cargo test meeting"
```

如果新增或改动通用 LLM helper：

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& rustup run stable-x86_64-pc-windows-msvc cargo test coordinator"
```

最后检查：

```powershell
git diff --check
git diff --stat
git status --short
```

## 15. 测试计划

Rust 单测：

- `meeting_summary_builds_prompt_with_segment_ids`
- `meeting_summary_parse_json_response`
- `meeting_summary_parse_code_fenced_json`
- `meeting_summary_rejects_invalid_json`
- `meeting_summary_filters_unknown_source_segment_ids`
- `meeting_summary_empty_transcript_fails_without_llm`
- `meeting_summary_failure_preserves_transcript`
- `meeting_summary_retry_from_summary_failed`
- `meeting_summary_rejects_active_recording`
- `meeting_summary_rejects_duplicate_job`
- `meeting_summary_long_transcript_uses_rolling_context`

Rust 回归：

- `cargo test meeting`
- 如果触碰 LLM helper 或 coordinator 公共路径，跑 `cargo test coordinator`。
- 如果触碰 polish provider 方法，跑 `cargo test polish`。

TypeScript：

- `.\node_modules\.bin\tsc.CMD --noEmit`
- `npm run build`

Diff 检查：

- `git diff --check`
- `git diff --stat`

不要求真实 LLM 集成测试进入自动测试；真实 provider 调用放人工验证。

## 16. 人工验证计划

1. 录一段短会议，点击停止。
2. 停止后详情显示 summary loading（总结生成中）。
3. 总结成功后显示标题、overview、key decisions、todos、risks/open questions。
4. todos 至少展示 content、owner、dueDate、sourceQuote。
5. 断网或填错 LLM credentials（凭据），确认进入 `summary_failed`，原文不消失。
6. 点击 retry，恢复网络或凭据后生成成功。
7. 空 transcript 会议不调用 `LLM`，显示 `summary_failed`。
8. 长 transcript 样本可生成最终统一 summary，不出现各段孤立总结。
9. `transcribing_interrupted` 会议不自动总结，并提示原文可能不完整。
10. 短口述录音、润色、插入、历史记录行为不变。
11. 切换到会议页以外再回来，summary 状态可通过 list/get 刷新恢复。
12. 移动端会议详情 summary 区不与录音控件和 transcript 重叠。

## 17. 风险与回退

风险：

- `LLM` 输出 invalid JSON，导致解析失败。回退：进入 `summary_failed`，保留原文，允许 retry。
- 超长 transcript 超出 context window（上下文窗口）。回退：chunk + rolling context；如果仍失败，保留原文并提示失败。
- stop 后自动 summary 与 UI refresh 存在 race condition（竞态）。回退：`meeting:summary` event 加 `listMeetings()` refresh fallback。
- 同一 meeting 重复点击 retry，可能并发覆盖。回退：后端 job tracking 拒绝重复任务。
- `meeting:state` 与 `meeting:summary` 事件语义混淆。回退：V1-4 新增 `meeting:summary`，不改录音事件。
- 复用 LLM provider 时误改短口述 polish。回退：只新增 helper，不改现有 `polish_text` 调用；跑 coordinator/polish 回归。
- 自动改标题可能覆盖用户手动标题。回退：只在标题仍符合 V1-2 默认标题格式时覆盖；无法确认默认标题时不覆盖。
- 长会 rolling notes 可能丢失细节。V1-4 最小方案先保证不中断和最终统一合成，不承诺引用完整性超过 sourceSegmentIds/sourceQuote。

回退策略：

- 如果自动 summary 不稳定，保留手动 `generateMeetingSummary` 入口，stop 后只刷新 completed record。
- 如果 `meeting:summary` event 接入失败，前端在 generate/retry 返回后 upsert record，并在停止后定时或手动 refresh。
- 如果通用 LLM helper 牵动过大，V1-4 可在 `meeting_summary.rs` 中临时复用 `polish_text` 的 provider routing（服务商路由）作为保守路径，但必须明确不使用短口述 style prompt，不改变短口述调用。

## 18. V1-5 / future notes，不要混入 V1-4 实现

V1-5 或后续可单独计划：

- 编辑标题、overview、key decisions、todos、risks/open questions。
- 重新生成 summary 并二次确认覆盖。
- Markdown 导出。
- 删除会议记录。
- 音频存在时重新转写。
- sourceSegmentIds 点击跳转到 transcript。
- 分段笔记可视化。
- 多模板 summary。
- 用户自定义 prompt template（提示词模板）。

future notes 单独计划：

- 会议专用 ASR 配置。
- 会议专用 LLM provider 配置。
- VAD 分段。
- speaker diarization（说话人分离）。
- 实时标点。
- subtitle-grade streaming（字幕级低延迟流式）。
- system audio capture（系统声音采集）。
- 悬浮会议入口。
- Word/PDF 导出。
- 第三方任务系统同步。

以上内容不得进入 V1-4 实现。
