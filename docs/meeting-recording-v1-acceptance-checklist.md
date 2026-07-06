# 会议录音总结 V1 Acceptance Checklist

状态：active  
日期：2026-07-06

## Scope Authority（范围权威）

- 顶层 V1 范围以 `docs/meeting-recording-v1-spec.md` 为准。
- 阶段计划：
  - `docs/meeting-recording-v1-storage-plan.md`
  - `docs/meeting-recording-v1-recording-plan.md`
  - `docs/meeting-recording-v1-ui-plan.md`
  - `docs/meeting-recording-v1-summary-plan.md`
  - `docs/meeting-recording-v1-implementation-plan.md`
- `docs/future-meeting-recording-notes.md` 只记录 future（未来能力），不能把未确认的 V1 项静默移入 future。
- 最新用户确认优先：
  - “结束后统一清理标点和分段”本轮不实现。
  - `summary_failed`（总结失败）时不改标题。

## 状态定义

- `done`：当前工作区已有实现证据；仍可能需要真实桌面环境人工冒烟。
- `partial`：部分路径已实现，但字段、UI、错误处理或验收不完整。
- `missing`：未实现。
- `deferred`：用户明确确认移出当前 V1 收尾范围。
- `blocked`：受环境、依赖或平台能力阻塞，已写明原因。

## Checklist

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| MR-V1-001 | 主导航出现“会议”页，可以开始、暂停、继续、停止会议录音。 | `meeting-recording-v1-spec.md` 验收 1 | done | `openless-all/app/src/pages/Meetings.tsx`、meeting IPC（会议进程间调用）、Rust meeting coordinator（会议协调器） | `tsc --noEmit`；仍需人工冒烟 | - |
| MR-V1-002 | 录音中实时滚动显示 `[未区分][00:12:34] 文本`。 | `meeting-recording-v1-spec.md` 验收 2 | done | `Meetings.tsx` 使用 `HH:MM:SS` timestamp（时间戳）展示 transcript（原文） | `tsc --noEmit`；仍需长会议人工验证 | streaming provider（流式语音转文字服务商）可实时追加；batch provider（批处理语音转文字服务商）按 pause / stop 刷新。 |
| MR-V1-003 | 停止后自动生成标题、摘要、关键结论、结构化待办、风险/未决问题。 | `meeting-recording-v1-spec.md` 验收 3 | done | `meeting_summary.rs`、`meeting.rs`、`Meetings.tsx` | Rust meeting summary tests；仍需真实 LLM provider（大语言模型服务商）人工验证 | stop（停止会议）后进入 `summarizing`（总结中）。 |
| MR-V1-004 | 待办包含内容、负责人、截止时间、来源片段。 | `meeting-recording-v1-spec.md` 验收 4 | done | `MeetingTodo` 数据结构、summary prompt（总结提示词）、`SummaryEditor` 和 `TodoList` | `tsc --noEmit`；仍需 UI 人工验证 | `sourceSegmentIds`（来源片段 ID）保留并展示，不做复杂选择器。 |
| MR-V1-005 | 会议记录本地保存，列表支持按标题/摘要搜索。 | `meeting-recording-v1-spec.md` 验收 5 | done | `MeetingStore`、`meetingSearchText` 覆盖 title、overview、keyDecisions、todos、risks、transcript | `tsc --noEmit`；仍需 UI 人工验证 | 不做复杂全文索引。 |
| MR-V1-006 | 原文只读；标题、总结、待办、风险/未决问题可编辑。 | `meeting-recording-v1-spec.md` 验收 6 | done | `Meetings.tsx` 编辑 draft（草稿）和 `updateMeetingRecord`；transcript 只读 | `tsc --noEmit`；仍需非全屏窗口人工验证 | 编辑区已设滚动和自适应高度。 |
| MR-V1-007 | 支持导出 Markdown。 | `meeting-recording-v1-spec.md` 验收 7 | done | `export_meeting_markdown`、`exportMeetingMarkdown`、会议详情导出按钮 | Rust meeting command tests；仍需 save dialog（保存对话框）人工验证 | 不做 Word/PDF 导出。 |
| MR-V1-008 | 支持删除会议记录并二次确认。 | `meeting-recording-v1-spec.md` 验收 8 | done | `deleteMeetingRecord` UI 二次确认；后端删除 active guard（活跃保护）和音频清理 | Rust meeting delete tests；仍需人工冒烟 | 删除失败不应破坏已有文本记录。 |
| MR-V1-009 | ASR 中断时保留已识别内容，会议可继续录音。 | `meeting-recording-v1-spec.md` 验收 9 | done | `coordinator/meeting.rs` 的 `transcribing_interrupted`（转写中断）状态和 error event（错误事件） | Rust meeting tests；仍需真实 ASR 人工验证 | - |
| MR-V1-010 | 音频存在时支持手动重新转写。 | `meeting-recording-v1-spec.md` 验收 10 | done | `retranscribe_meeting`、`retranscribeMeeting`、会议详情重新转写按钮 | Rust meeting command tests；仍需真实 ASR provider 人工验证 | 只在 `audio.state === retained`（音频已保留）且非 active recording（非活跃录音）时允许。 |
| MR-V1-011 | 总结失败时不丢原文，显示失败状态并允许重试。 | `meeting-recording-v1-spec.md` 验收 11 | done | `meeting_summary.rs`、`retryMeetingSummary`、`Meetings.tsx` retry UI（重试界面） | Rust meeting summary tests；仍需真实失败路径人工验证 | 失败时不改标题，见 MR-V1-016。 |
| MR-V1-012 | 原始音频按最近 N 场保留，默认 20，最大 100，超出后清理最旧音频。 | `meeting-recording-v1-spec.md` 验收 12 | done | `persistence/meeting.rs`、`types.rs` preference（偏好设置）字段、recording stop retention（保留策略） | Rust retention tests；仍需真实文件人工验证 | 只清理音频，不删除会议文本记录。 |
| MR-V1-013 | 设置中可调整会议音频保留数量，最大 100，可设为 0。 | `meeting-recording-v1-spec.md` 原始音频保留 | done | `DataStorageSection.tsx`、五份 i18n（国际化文案） | `tsc --noEmit`；仍需设置页人工验证 | `0` 表示不长期保留会议音频。 |
| MR-V1-014 | 重新生成总结前二次确认，确认后覆盖标题、摘要、关键结论、待办、风险/未决问题。 | `meeting-recording-v1-spec.md` 错误处理 | done | `Meetings.tsx` 页面内 rewrite summary confirmation（重写总结二次确认） | `tsc --noEmit`；仍需人工验证 | 不做 cancel summary（取消总结任务）。 |
| MR-V1-015 | 关闭窗口/退出应用时，如果正在录音，提示并在确认后停止录音保存已有内容。 | `meeting-recording-v1-spec.md` 错误处理 | done | `lib.rs` emit `meeting:close-requested`；`FloatingShell.tsx` close guard prompt（关闭保护提示）；`normalizeMeetingCloseRequest` 兼容 payload（负载） | `tsc --noEmit`；仍需真实 Tauri close/tray quit 人工验证 | exit（退出）路径会等待 summary generation（总结生成）结束后再退出，避免杀掉后台任务。 |
| MR-V1-016 | 总结生成失败时标题降级为“会议记录 时间 - 总结生成失败”。 | `meeting-recording-v1-spec.md` 早期错误处理 | deferred | 最新用户确认：失败时不改标题 | 不适用 | 保留现有标题，避免覆盖用户自定义标题。 |
| MR-V1-017 | 实时区域展示 ASR 原始结果，结束后统一清理标点和分段。 | `meeting-recording-v1-spec.md` V1 范围 | deferred | 最新用户确认：“结束后统一清理标点和分段”先不实现 | 不适用 | 当前保留 ASR final segment（最终识别片段）作为 transcript。 |
| MR-V1-018 | 不做系统声音采集、VAD、speaker diarization、会议专用 ASR/LLM 配置、Word/PDF、云同步。 | `meeting-recording-v1-spec.md` 非 V1 范围 | done | 当前 diff 未引入这些能力 | Scope review（范围审查） | 持续保持。 |

## Deferred Log（延期记录）

| ID | Requirement（需求） | Deferred reason（延期原因） | User confirmation（用户确认） |
| --- | --- | --- | --- |
| MR-V1-016 | `summary_failed` 时标题降级 | 失败时不改标题，避免覆盖用户自定义标题 | 2026-07-06：用户确认“失败时不改标题”。 |
| MR-V1-017 | 结束后统一清理标点和分段 | 本轮先不实现，避免牵动 ASR（语音转文字）主链路和额外清理管线 | 2026-07-06：用户确认“先不实现”。 |

## 当前剩余验证项

在声称会议录音总结 V1 完成前，仍需至少完成一次真实产品路径人工验证：

1. Tauri dev（桌面开发环境）启动后进入“会议”页。
2. 开始会议、暂停、继续、停止。
3. 停止后 summary generation（总结生成）成功和失败路径。
4. retry summary（重试总结）和 rewrite summary（重写总结）二次确认。
5. 编辑标题、overview（概览）、keyDecisions（关键决定）、todos（待办）、risksAndOpenQuestions（风险与开放问题）。
6. Markdown export（Markdown 导出）保存到本地。
7. 删除会议并确认音频清理行为。
8. retained audio（已保留音频）会议重新转写。
9. 设置页调整 `meetingAudioRetentionCount`（会议音频保留数量）。
10. 关闭窗口和 tray quit（托盘退出）时 active meeting guard（活跃会议保护）行为。

## Final Acceptance Gate（最终验收门槛）

- MR-V1-001 到 MR-V1-018 必须全部为 `done` 或用户明确确认的 `deferred`。
- 所有自动验证命令必须按最终 diff（差异）重新运行。
- 人工验证缺口必须在交付中列出，不能描述为已真实验证。
