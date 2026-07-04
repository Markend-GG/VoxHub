# 会议录音总结 V1 实施拆解

状态：draft / ready for implementation planning
日期：2026-07-04
关联文档：

- `docs/meeting-recording-v1-spec.md`
- `docs/future-meeting-recording-notes.md`

## 规划原则

V1 按“可独立验收的产品闭环”拆分，而不是按底层能力管线拆分。

V1 不包含：

- VAD（Voice Activity Detection，语音活动检测）
- speaker diarization（说话人分离）
- system audio capture（系统声音采集）
- subtitle-grade streaming（字幕级低延迟流式刷新）
- 会议专用 ASR/LLM 配置

这些后续能力只记录在 `docs/future-meeting-recording-notes.md`，不进入 V1 实施计划。

## V1 总体执行顺序

1. V1-1 会议基础数据与本地存储
2. V1-2 会议录音与原文生成
3. V1-3 会议页面与记录管理
4. V1-4 会后总结、待办与长会处理

这个顺序的目标是每一步都能单独验收，避免先做大量底层管线但用户仍无法完成一次会议。

## V1-1 会议基础数据与本地存储

### 目标

把“会议”作为独立数据对象落地，不复用短口述 history 的数据结构。

### 范围

- 新增 `MeetingRecord`、`TranscriptSegment`、`MeetingTodo` 等类型。
- 新增会议记录本地 JSON 存储。
- 支持创建、读取、更新、删除会议记录。
- 支持原始音频状态字段。
- 实现会议音频最近 N 场保留策略：
  - 默认 20。
  - 最大 100。
  - 可设为 0。
  - 超出后只清理最旧音频，不删除文本记录。
- 删除会议记录时同步删除关联音频。

### 不做

- 不做录音。
- 不做 ASR。
- 不做 UI。
- 不做总结。

### 主要代码区域

- `openless-all/app/src-tauri/src/types.rs`
- `openless-all/app/src-tauri/src/persistence/`
- `openless-all/app/src-tauri/src/commands/`
- `openless-all/app/src/lib/ipc/`
- `openless-all/app/src/lib/types.ts`

### 验收标准

- 能通过后端命令创建一条会议记录。
- 能列出会议记录，顺序为 newest-first（最新在前）。
- 能更新标题、总结、待办等字段。
- 能删除会议记录。
- 删除会议记录时，关联音频文件被清理。
- 超过音频保留数量时，最旧会议的音频被清理，文本记录保留。

### 建议验证

- Rust 单测覆盖：
  - 会议记录 JSON 读写。
  - 删除记录时清理音频。
  - 音频保留数量 clamp 到 `0..=100`。
  - 超出保留数量时只清理音频，不删文本记录。
- TypeScript 类型检查覆盖新增 IPC 类型。

## V1-2 会议录音与原文生成

### 目标

完成从会议开始到停止的录音原文闭环：用户真实说一段话后，会议记录里能看到带时间戳的原文。

### 范围

- 新增会议 session 状态。
- 支持开始、暂停、继续、停止会议录音。
- 只录麦克风。
- 复用全局 ASR provider（语音转文字服务商）。
- 使用 ASR final segment（语音识别最终片段）追加 `TranscriptSegment`。
- V1 默认 `speakerLabel` 为“未区分”。
- 录音期间保留临时音频。
- ASR 中断时：
  - 保留已识别原文。
  - 标记会议为“转写中断”。
  - 允许继续录音。
- 音频仍存在时，支持手动重新转写。

### 不做

- 不做 VAD 分段。
- 不做说话人识别。
- 不做系统声音采集。
- 不做实时标点恢复。

### 主要代码区域

- `openless-all/app/src-tauri/src/recorder.rs`
- `openless-all/app/src-tauri/src/coordinator/`
- `openless-all/app/src-tauri/src/coordinator/asr_wiring.rs`
- `openless-all/app/src-tauri/src/asr/`
- `openless-all/app/src-tauri/src/commands/`
- `openless-all/app/src/lib/ipc/`

### 验收标准

- 能开始会议录音。
- 能暂停录音，暂停期间不采集音频。
- 能继续录音，继续后仍写入同一会议。
- 能停止录音并保存会议记录。
- 实时原文按 `[未区分][00:12:34] 文本` 所需数据结构保存。
- ASR 中断后已识别内容不丢。
- ASR 中断后仍可继续录音。
- 音频存在时可重新转写并更新原文。

### 建议验证

- Rust 单测覆盖 session 状态流转：
  - idle -> recording -> paused -> recording -> stopped。
  - ASR interrupted 不清空已存在 transcript segments。
- 手工验证：
  - Windows 麦克风录一段中文短句。
  - 暂停后说话不应进入原文。
  - 继续后新内容写入同一会议。
  - 停止后会议记录可读取。

## V1-3 会议页面与记录管理

### 目标

让用户在主界面完成会议入口、录音工作台、会议列表和详情管理。

### 范围

- 新增主导航“会议”页。
- 会议页默认显示：
  - 搜索框。
  - 开始会议按钮。
  - 会议记录列表。
- 录音中同页切换为会议工作台：
  - 状态。
  - 时长。
  - 暂停/继续。
  - 停止。
  - 实时滚动原文。
- 会议详情：
  - 原文只读。
  - 标题、总结、待办、风险/未决问题可编辑。
  - 重新生成总结入口。
  - 重新转写入口。
  - 导出 Markdown。
  - 删除记录，二次确认。
- 列表支持按标题/摘要搜索。

### 不做

- 不做复杂全文搜索。
- 不做 Word/PDF 导出。
- 不做悬浮字幕窗口。

### 主要代码区域

- `openless-all/app/src/state/useAppState.ts`
- `openless-all/app/src/components/FloatingShell.tsx`
- `openless-all/app/src/pages/`
- `openless-all/app/src/i18n/zh-CN.ts`
- `openless-all/app/src/i18n/zh-TW.ts`
- `openless-all/app/src/i18n/en.ts`
- `openless-all/app/src/i18n/ja.ts`
- `openless-all/app/src/lib/ipc/`

### 验收标准

- 主导航出现“会议”。
- 可以从会议页开始会议。
- 录音中可以看到时长和实时原文。
- 可以暂停、继续、停止。
- 会议列表能显示标题、时间、时长、状态。
- 列表搜索能匹配标题/摘要。
- 详情页原文只读。
- 标题、总结、待办、风险/未决问题可编辑并保存。
- 删除记录前有二次确认。
- Markdown 导出内容包含标题、时间、原文、摘要、关键结论、待办、风险/未决问题。

### 建议验证

- `npm run build`
- `.\node_modules\.bin\tsc.CMD --noEmit`
- 手工检查桌面端主界面：
  - 会议页入口。
  - 空列表。
  - 录音工作台。
  - 详情页编辑。
  - 删除确认。
  - Markdown 导出。

## V1-4 会后总结、待办与长会处理

### 目标

停止会议后自动生成可编辑会议纪要，并处理长会议上下文不连续的问题。

### 范围

- 停止录音后自动调用 LLM provider（大语言模型服务商）。
- 固定模板输出：
  - 标题。
  - 摘要。
  - 关键结论。
  - 结构化待办。
  - 风险/未决问题。
- 待办字段：
  - 内容。
  - 负责人。
  - 截止时间。
  - 来源片段。
- 总结失败时：
  - 保留原文。
  - 状态为“总结生成失败”。
  - 标题降级为“会议记录 时间 - 总结生成失败”。
  - 允许手动重试。
- 重新生成总结时：
  - 二次确认。
  - 覆盖 AI 生成字段。
- 长会议处理：
  - 2 小时软限制，不强制停止。
  - 音频/文本可分段保存。
  - 超过 LLM context window（上下文窗口）时使用 rolling context（滚动上下文）。
  - 分段不能孤立总结，最终需要统一合成、去重并合并待办。

### 不做

- 不做多模板选择。
- 不做用户自定义模板。
- 不做第三方任务系统同步。

### 主要代码区域

- `openless-all/app/src-tauri/src/polish/`
- `openless-all/app/src-tauri/src/coordinator/polish_flow.rs`
- `openless-all/app/src-tauri/src/commands/`
- `openless-all/app/src/lib/ipc/`
- `openless-all/app/src/pages/`

### 验收标准

- 停止会议后自动进入总结生成态。
- 成功后生成标题、摘要、关键结论、待办、风险/未决问题。
- 待办是结构化数据。
- 每条待办能关联来源片段或来源摘录。
- LLM 失败时会议原文保留。
- LLM 失败后可手动重试。
- 重新生成总结前有二次确认。
- 长文本生成时不按段孤立总结。

### 建议验证

- Rust 单测覆盖：
  - LLM 输出解析为结构化会议纪要。
  - 总结失败时不清空原文。
  - 重新生成总结覆盖 AI 字段。
  - rolling context 输入中包含前序分段笔记。
- 手工验证：
  - 录一段短会议并生成总结。
  - 模拟 LLM 失败后重试。
  - 用长文本样本验证分段合成不会丢前文关键结论。

## 跨计划约束

- 每个计划都必须保持 V1 范围，不把 `docs/future-meeting-recording-notes.md` 中的能力混入实现。
- 修改前必须先看 `git status --short`。
- 不要自动 commit；只有用户明确要求提交时才 commit。
- 优先精确 stage 文件，不使用 `git add .`。
- Windows 开发默认走 MSVC 工具链。
- 任何涉及 ASR 主链路的改动都要验证不会破坏现有短口述流程。

## 推荐实施方式

建议先为每个 V1 子计划分别写更细的 execution plan（执行计划），再逐个开发：

1. `meeting-recording-v1-storage-plan.md`
2. `meeting-recording-v1-recording-plan.md`
3. `meeting-recording-v1-ui-plan.md`
4. `meeting-recording-v1-summary-plan.md`

每个 execution plan 都应列出具体文件、测试、验证命令和最小提交边界。
