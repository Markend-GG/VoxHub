# 会议录音总结 V1-3 UI 接入执行计划

状态：implemented / ready for verification
日期：2026-07-04
前置计划：

- `docs/meeting-recording-v1-storage-plan.md`
- `docs/meeting-recording-v1-recording-plan.md`

## 1. 背景与目标

V1-1 已完成 `MeetingRecord`（会议记录）、`TranscriptSegment`（会议原文片段）、本地 JSON 存储、会议音频保留策略和会议 `IPC`（Inter-Process Communication，进程间调用）基础。

V1-2 已完成开始、暂停、继续、停止会议录音，并通过现有全局 `ASR provider`（语音转文字服务商）生成会议原文。后端已提供会议列表、详情、录音控制和 active meeting（活跃会议）查询命令，并发送 `meeting:state`、`meeting:transcript-segment`、`meeting:error` event（事件）。

V1-3 目标是把这些能力接入现有主界面，让用户完成最小 UI 闭环：进入会议页、查看会议列表、查看会议详情、开始/暂停/继续/停止会议录音、看到原文追加和录音状态。

## 2. V1-3 范围

- 新增主导航“会议”入口。
- 新增会议页面，使用现有主 `shell`（外壳）内的 `page`（页面）模式。
- 展示会议列表：标题、开始时间、时长、状态、原文预览、音频状态 badge（徽标）。
- 展示会议详情：标题只读、基础元信息、`TranscriptSegment` 原文只读列表、当前录音状态、ASR 中断提示、音频状态最小展示。
- 提供开始、暂停、继续、停止会议录音控件。
- 订阅 `meeting:state`、`meeting:transcript-segment`、`meeting:error`。
- streaming provider（流式语音转文字服务商）收到 segment event 后实时追加。
- batch provider（批处理语音转文字服务商）在 pause / stop 后通过 snapshot（快照）或 refresh（刷新）补齐段落。
- ASR 中断时保留已识别原文，显示 `transcribing_interrupted` 状态和错误提示。
- 停止后刷新列表并选中刚停止的会议详情。

## 3. 明确不做项

V1-3 不做：

- `LLM`（Large Language Model，大语言模型）总结。
- todo 自动生成。
- `VAD`（Voice Activity Detection，语音活动检测）。
- 人声分离 / `speaker diarization`（说话人识别）。
- 系统声音采集。
- 会议专用 ASR/LLM 配置。
- 音频重转写 UI。
- 复杂编辑器。
- 音频播放器。
- Markdown 导出。
- 删除会议记录。
- 编辑标题、摘要、待办、风险/未决问题。
- 改短口述主链路行为。

`docs/meeting-recording-v1-spec.md` 和 `docs/meeting-recording-v1-implementation-plan.md` 的早期 V1-3 描述包含编辑、删除、导出、重转写和总结入口；本计划按最新 V1-3 最小闭环收窄。

## 4. 当前代码结构观察

- `openless-all/app/src/App.tsx`：懒加载 `FloatingShell`，不放会议业务逻辑。
- `openless-all/app/src/components/FloatingShell.tsx`：主导航由 `NAV_BASE` 和 `AppTab` 驱动。
- `openless-all/app/src/components/MobileMoreSheet.tsx`：移动端 More（更多）入口列表。
- `openless-all/app/src/state/useAppState.ts`：只保存当前 tab 和 settings modal（设置弹窗）状态，V1-3 只新增 `meetings` tab。
- `openless-all/app/src/pages/History.tsx`：已有列表 + 详情双栏、移动端列表/详情切换、搜索、loading、error、空态模式。
- `openless-all/app/src/pages/_atoms.tsx`：复用 `PageHeader`、`Card`、`Pill`、`Btn`。
- `openless-all/app/src/lib/ipc/meetings.ts`：已有会议 IPC wrapper（封装函数）。
- `openless-all/app/src/lib/types.ts`：已有会议类型 mirror（镜像类型）。
- `openless-all/app/src/i18n/*.ts`：需同步 `nav.meetings` 和 `meetings.*` 文案。

后端 V1-3 不新增命令；前端只消费 V1-2 已落地的 `CoordinatorState` meeting command（命令）和 event（事件）。

## 5. 推荐 UI 信息架构

主导航新增：

- id：`meetings`
- label：`nav.meetings`
- icon：复用 `mic`

页面布局沿用 `History.tsx`：

- 桌面端：左侧会议列表，右侧会议详情。
- 移动端：默认显示列表，点会议后进入详情，详情顶部提供返回列表按钮。

页面顶部：

- `PageHeader`
- 右侧：刷新、开始会议按钮。
- active meeting 存在时：标题旁显示 phase（阶段）badge。

列表项展示：

- title。
- startedAt。
- durationMs。
- status badge。
- 第一段原文预览。
- audio state badge。
- active badge。

详情展示：

- title、status、audio、startedAt、elapsed/duration、activeAsrProvider。
- 录音控件：recording 显示暂停/停止，paused 显示继续/停止，active interrupted 仍按 snapshot phase 显示可用动作。
- 原文只读列表：每段显示 speaker label（说话人标签）、时间戳、source（来源）和文本。

## 6. 状态流转设计

页面本地状态：

- `meetings: MeetingRecord[]`
- `selectedId: string | null`
- `activeSnapshot: MeetingRecordingSnapshot | null`
- `loading: boolean`
- `actionLoading: 'start' | 'pause' | 'resume' | 'stop' | null`
- `loadError: string | null`
- `actionError: string | null`
- `eventError: MeetingErrorEvent | null`
- `mobileDetailOpen: boolean`

初始化：

1. 并行调用 `getActiveMeetingRecording()` 和 `listMeetings()`。
2. 如果 active snapshot 存在，upsert（插入或更新）snapshot.meeting 到列表，并选中 active meeting。
3. 如果无 active meeting，保留现有 selectedId，否则选列表第一项。

停止后：

1. `stopMeetingRecording(id)` 返回 `MeetingRecord`。
2. 清空 active snapshot。
3. upsert 返回的 record。
4. selectedId 设为该 record id。
5. 调 `listMeetings()` 二次刷新，确保 retention/audio state（音频保留状态）同步。

## 7. IPC 和 event 接入设计

IPC 调用：

- `listMeetings()`：页面初始化、停止后、手动刷新。
- `getMeeting(id)`：选择详情时刷新该条记录。
- `startMeetingRecording()`：开始会议。
- `pauseMeetingRecording(id)`：暂停 active meeting。
- `resumeMeetingRecording(id)`：继续 active meeting。
- `stopMeetingRecording(id)`：停止 active meeting。
- `getActiveMeetingRecording()`：页面 mount（挂载）时查询 active meeting。

event 订阅：

- `meeting:state`：更新 active snapshot 和列表。
- `meeting:transcript-segment`：按 meeting id 追加 segment，并按 segment id 去重。
- `meeting:error`：显示错误提示，不清空 transcript。

streaming provider：

- 收到 `meeting:transcript-segment` 后立即追加。
- 原文滚动区在用户接近底部时自动贴底。

batch provider：

- 录音中可能不出现 segment event。
- pause / stop 后通过 `meeting:state` snapshot 或 stop 返回 record 补齐原文。

## 8. 文件改动清单

新增：

- `openless-all/app/src/pages/Meetings.tsx`
- `docs/meeting-recording-v1-ui-plan.md`

修改：

- `openless-all/app/src/state/useAppState.ts`
- `openless-all/app/src/components/FloatingShell.tsx`
- `openless-all/app/src/components/MobileMoreSheet.tsx`
- `openless-all/app/src/i18n/zh-CN.ts`
- `openless-all/app/src/i18n/zh-TW.ts`
- `openless-all/app/src/i18n/en.ts`
- `openless-all/app/src/i18n/ja.ts`
- `openless-all/app/src/i18n/ko.ts`

不修改：

- Rust 后端。
- 短口述 `Capsule`。
- ASR provider。
- meeting IPC wrapper。

## 9. 分任务实施步骤

1. 新增 `Meetings.tsx` 页面，实现本地状态、列表、详情、控制按钮和 event 订阅。
2. 在 `useAppState.ts` 新增 `meetings` tab。
3. 在 `FloatingShell.tsx` 接入桌面导航，移动端将 meetings 归入 More。
4. 在 `MobileMoreSheet.tsx` 加入 meetings。
5. 在五份 i18n 文件补齐 `nav.meetings` 和 `meetings.*`。
6. 运行 TypeScript、build、meeting Rust test、diff 检查。

## 10. 测试计划

自动验证：

```powershell
cd openless-all/app
.\node_modules\.bin\tsc.CMD --noEmit
npm run build
```

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& rustup run stable-x86_64-pc-windows-msvc cargo test meeting"
```

```powershell
git diff --check
```

## 11. 人工验证计划

- 主导航出现“会议”。
- 会议页空列表显示正常。
- 点击开始会议后进入 recording 状态。
- recording 显示暂停和停止按钮。
- 暂停后显示 paused，继续后回到 recording。
- 停止后列表刷新并选中会议详情。
- streaming provider 录音中追加原文。
- batch provider 在 pause / stop 后出现原文。
- ASR 中断后已识别原文不消失，显示 `transcribing_interrupted`。
- 音频状态只显示 badge，不出现播放器。
- 历史页、设置页、短口述主链路行为不变。

## 12. 风险与回退

- event 到达顺序不固定，UI 必须按 meeting id upsert，不依赖固定顺序。
- segment event 和 snapshot 可能重复，按 segment id 去重。
- `transcribing_interrupted` 可能发生在 recording 或 paused，控件以 active snapshot phase 为准。
- 移动端导航空间有限，V1-3 把 meetings 放入 More，降低布局风险。
- 如果 event 订阅失败，开始/暂停/继续/停止 IPC 返回值仍能更新 UI，停止后再 `listMeetings()` 刷新。

## 13. V1-4 / future notes，不要混入 V1-3 实现

V1-4 单独计划：

- 停止后 LLM 总结。
- 标题自动生成。
- 摘要。
- 关键结论。
- 结构化 todo。
- 风险/未决问题。
- 长会 rolling context（滚动上下文）。
- 总结失败重试。

future notes 单独计划：

- 会议专用 ASR 配置。
- VAD 分段。
- speaker diarization（说话人分离）。
- 实时标点。
- subtitle-grade streaming（字幕级低延迟流式）。
- system audio capture（系统声音采集）。
- 悬浮会议入口。
- Word/PDF 导出。
- 第三方任务系统同步。
