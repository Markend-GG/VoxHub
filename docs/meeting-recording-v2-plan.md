# 会议录音 V2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 V1 已完成的会议记录基础上，优先升级会议录入质量、realtime ASR（实时语音转文字）、timestamp（时间戳）、本地 speaker diarization（说话人分离）对齐和上传音频能力。

**Architecture:** V2 按管线分层推进：meeting audio（会议音频）采集与保留、meeting ASR config（会议语音转文字配置）、ASR provider capabilities（语音转文字服务商能力声明）、realtime transcript event（实时原文事件）、post-processing（后处理）和导入能力分阶段接入。每个阶段必须能单独运行和验证，不能要求一次性重构全部 ASR（语音转文字）体系。

**Tech Stack:** Tauri 2、Rust、React、TypeScript、Windows MSVC toolchain（工具链）、现有 CredentialsVault（凭据存储）、现有 meeting IPC（进程间调用）和 event（事件）。

---

状态：draft for implementation planning  
日期：2026-07-08  
基线分支：`codex/meeting-recording-v2`  
关联文档：

- `docs/meeting-recording-v1-spec.md`
- `docs/future-meeting-recording-notes.md`
- `docs/local-asr-pipeline-upgrade/README.md`
- `docs/local-asr-pipeline-upgrade/05-pipeline-options-contract.md`
- `docs/local-asr-pipeline-upgrade/09-realtime-preview.md`
- `docs/meeting-recording-v2-1-realtime-asr-plan.md`

## 1. 背景

V1 已提供独立会议页、麦克风录音、暂停 / 继续 / 停止、会议原文、自动 summary（总结）、编辑、删除、Markdown 导出、重写总结和音频存在时重新转写。

V2 的核心问题不是继续扩展总结模板，也不是一次性做完整视频会议工具，而是把“会议录入管线”做扎实：

- 会议中要更快看到文字。
- provider（服务商）能力差异要被显式建模，不能靠 provider id（服务商标识）到处硬编码。
- online ASR（在线语音转文字）和 local ASR（本地语音转文字）要能在会议场景内被独立选择。
- V2-1 不执行 speaker diarization（说话人分离），但必须保存足够的 audio（音频）、final segments（最终片段）和 timestamp（时间戳），支撑 V2-2 对齐。
- 上传音频进入 V2 总路线，但排在 realtime ASR（实时语音转文字）和本地 speaker diarization（说话人分离）之后。
- system audio capture（系统声音采集）、悬浮入口、实时说话人标签、字幕级逐字刷新和 Word/PDF 导出放入 V3 / future backlog（未来待办），避免 V2 过宽。

## 2. 已确认的 V2-1 决策

- V2-1 优先会议中 realtime visible text（实时可见文字）。
- V2-1 使用 meeting-specific ASR settings（会议专用语音转文字设置），默认 inherit global ASR（继承全局语音转文字）。
- 设置入口放在全局设置 -> 服务，新增“会议 ASR”区域。
- 会议 ASR 允许选择所有现有 ASR provider（语音转文字服务商）。
- 底层 provider 可以复用给短口述，但 V2-1 UI 不把新 provider 能力开放给短口述。
- 复用现有 CredentialsVault（凭据存储），不新增一套密钥保存系统。
- 不暴露 Workspace ID、region（区域）、endpoint（端点）等会议专用字段；如 provider 已有标准 endpoint 配置，继续复用现有服务配置。
- 会议页不显示 ASR 状态。
- realtime text 使用 draft + final（临时识别 + 最终片段）双层。
- draft（临时识别）显示在会议原文列表底部的“正在识别”行。
- draft 不持久化、不总结、不导出、不参与搜索。
- final segment（最终片段）才生成 `TranscriptSegment`（会议原文片段）并持久化。
- final 文本以 provider 输出为 canonical transcript（权威原文），V2-1 只做非语义 normalization（规范化）：trim（去首尾空白）、移除不可见控制字符、规范换行、丢弃空 final、去重、稳定排序。
- 不做第二次 punctuation restoration（标点恢复）、不做第二次 ITN（Inverse Text Normalization，逆文本规范化）、不做 LLM cleanup（大语言模型清理）。
- 优先保存 word/token timestamp（词级/Token 级时间戳），保留 provider 原始粒度，不强转。
- pause（暂停）关闭当前 ASR session（语音转文字会话），resume（继续）打开新 ASR session，同一 `MeetingRecord` 继续。
- 网络断开或服务端报错时，不做自动 reconnect（重连）：继续录音、继续保存音频、实时 ASR 停止、状态进入 `transcribing_interrupted`（转写中断）、清空 draft、保留已保存 final segments。
- V2-1 不提供手动重新生成原文按钮，只保存足够数据给后续阶段。

## 3. V2 总路线

### V2-1：会议 realtime ASR 与数据契约

目标：会议中尽量实时显示 ASR draft（临时识别）和 final（最终片段），并保存 timestamp / metadata（时间戳 / 元数据）。

范围：

- meeting ASR config（会议语音转文字配置）。
- provider capabilities（服务商能力声明）。
- `fun-asr-realtime` 能力增强，优先复用现有 `bailian` provider（阿里云百炼 provider）。
- `meeting:transcript-draft` event（会议临时原文事件）。
- `TranscriptSegment.metadata`（会议原文片段元数据）。
- pause / resume 跨 ASR session 的 providerSessionId（服务商会话标识）和 sequence（序号）边界。
- 网络失败后继续录音并进入 `transcribing_interrupted`。

不进入 V2-1：

- speaker diarization（说话人分离）。
- 上传音频。
- system audio capture（系统声音采集）。
- 手动重新生成原文。
- 会后自动重新转写。
- 短口述 UI 暴露会议专用设置。

执行计划见：`docs/meeting-recording-v2-1-realtime-asr-plan.md`。

### V2-2：本地 VAD + speaker diarization 对齐

目标：停止会议后，基于保存的会议音频和 V2-1 保存的 timestamp（时间戳）做 local VAD（本地语音活动检测）、local speaker diarization（本地说话人分离）与 transcript alignment（原文对齐）。

范围：

- local VAD 只作为 speaker diarization 的前置音频切分能力，不在 V2-2 里重写实时原文分段 UI。
- local diarization-only（本地只做说话人分离）作为首选形态：输入会议音频 + final segments + timestamp，输出 speaker turns（说话人时间段）。
- 将 speaker turns 对齐回 `TranscriptSegment`，把 V1 的“未区分”升级为“发言人 1 / 发言人 2”。
- 支持低置信度段落保留“未区分”或标记“未确认”。
- 支持会后手动重命名 speaker label（说话人标签）。
- 对齐失败时保留原 transcript（原文）和“未区分”speakerLabel，不重跑在线 ASR。

默认不做：

- online file diarization（在线文件说话人分离）作为默认 fallback（降级）。
- 实时 speaker label（实时说话人标签）。
- 重新跑在线 ASR 解决 speaker label。
- 用 local VAD 改写 V2-1 的 realtime draft/final 显示节奏。

### V2-3：上传音频生成会议记录

目标：允许用户上传已有音频文件生成会议记录，不要求必须通过 OpenLess 现场录音。

范围：

- 复用 `MeetingRecord`、`TranscriptSegment` 和 summary（总结）结构。
- 明确支持格式、大小限制、时长限制、转码策略、失败重试、本地临时文件清理。
- 明确上传原文件或转码副本是否进入 `meeting-recordings/`，是否计入 retention（保留数量）。
- 复用会议 ASR config（会议语音转文字配置），不绕开 provider 选择逻辑。

## 4. V3 / future backlog

以下内容来自 `docs/future-meeting-recording-notes.md`，有产品价值，但不进入 V2 的前三个阶段。原因是它们涉及平台音频、复杂实时 UI、跨应用入口或更重的导出集成，会显著扩大验证矩阵。

### V3-1：realtime speaker labels 与 subtitle-grade streaming

目标：在会议进行中显示 speaker label（说话人标签），并提供 subtitle-grade streaming（字幕级低延迟流式刷新）。

范围：

- 逐字或逐短语 partial token（临时 token）刷新。
- partial rollback（临时结果回退）和 final overwrite（最终结果覆盖）。
- 实时 speaker label 的置信度和修正。
- 长会议滚动性能。

不反向塞入 V2-1：V2-1 只做底部 draft 行，不做字幕级逐字 UI。

### V3-2：system audio capture 平台能力

目标：支持线上会议采集对方声音，但必须独立设计 Windows / macOS 音频能力。

范围：

- Windows 与 macOS 分别调研 system audio capture（系统声音采集）方案。
- 明确 mic（麦克风）与 system audio（系统声音）的混音、回声、延迟对齐、权限失败、设备切换策略。
- 不默认把系统声音混进 V2-1 的麦克风录音。

### V3-3：悬浮入口与独立实时展示

目标：支持从悬浮球或独立窗口发起和查看会议记录。

范围：

- 悬浮球直接发起会议。
- 独立弹窗或悬浮窗展示实时会议原文。
- 主会议页与浮窗同步同一个 active session（活跃会话）。
- 关闭展示不等于停止会议，停止必须是明确动作。

### V3-4：长会议总结质量增强

目标：在 V1 rolling context（滚动上下文）基础上提高长会议 summary（总结）质量。

范围：

- 分段笔记可视化。
- 引用回溯。
- 待办去重说明。
- 关键结论置信度。
- 更稳健的上下文压缩策略。

### V3-5：更多导出与集成

目标：在 V1 Markdown 导出的基础上，评估 Word/PDF 导出和第三方任务系统集成。

范围：

- Word/PDF 导出格式。
- 结构化 todo（待办事项）导出。
- 第三方任务系统同步的授权、失败重试和同步状态。

### V3-6：高级文本后处理

目标：在 provider final text（服务商最终文本）之外提供可选文本清理能力。

范围：

- 可选 punctuation restoration（标点恢复）。
- 可选 text normalization（文本规范化）。
- 只对已完成会议做显式后处理，不修改 realtime draft。
- 不默认改变 provider canonical transcript（权威原文）。

## 5. 总体架构

```text
Meeting UI
  -> Meeting ASR settings
  -> Meeting session coordinator
  -> Audio recorder + retained meeting audio
  -> ASR provider selected by effective meeting config
  -> draft event stream
  -> final segment event stream
  -> TranscriptSegment + metadata persistence
  -> Summary / export / future diarization
```

关键边界：

- meeting session（会议会话）继续与短口述 session（短口述会话）分离。
- ASR provider runtime（语音转文字运行时）可以共用，但 effective config（实际配置）必须可区分“短口述”和“会议”。
- `TranscriptSegment` 仍是会议原文持久化的唯一主数据结构，V2-1 只给它补 metadata（元数据），不引入第二套会议原文模型。
- draft（临时识别）只走 event（事件），不落盘。
- provider capabilities（服务商能力声明）决定 UI 能显示哪些选项和后端能启用哪些参数。

## 6. 与 future notes 的对齐检查

已覆盖到 V2 总路线：

- 会议专用 ASR 配置：V2-1。
- VAD 与更自然分段：V2-1 只做 provider capability（服务商能力）和简单 silence preset（静音档位）；V2-2 做 local VAD 作为 speaker diarization 前置；高级自然分段 UI 放 V3。
- 说话人分离：V2-2 做停止后本地对齐；实时 speaker label 放 V3。
- 实时标点与文本清理：V2-1 明确不做二次语义清理，只保存 provider final；高级后处理放 V3。
- 上传音频生成会议记录：V2-3。

进入 V3 / future backlog：

- 字幕级低延迟流式体验：V3-1。
- 系统声音采集：V3-2。
- 悬浮入口与独立实时展示：V3-3。
- 长会议生成质量增强：V3-4。
- 更多导出与集成：V3-5。
- 高级文本后处理：V3-6。

没有偏离总方向的点：

- V2-1 没有提前做 speaker diarization。
- V2-1 没有引入 online fallback（在线后处理降级）作为默认。
- V2-1 没有把上传音频和 system audio capture 混进 realtime ASR。
- V2-1 没有改短口述主链路行为。

## 7. 总体验证策略

每个阶段都必须至少跑：

```powershell
cd D:\codex项目\VOXHUB\openless-all\app
.\node_modules\.bin\tsc.CMD --noEmit
npm run build
```

涉及 Rust 后端时使用 Windows MSVC toolchain（工具链）：

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& set SHERPA_ONNX_ARCHIVE_DIR=D:\openless-deps\sherpa-onnx-archive&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo check"
```

阶段相关测试：

- V2-1：`cargo test meeting`、`cargo test bailian`、新增 capability / draft / metadata 测试。
- V2-2：新增 local VAD、diarization alignment（说话人分离对齐）测试。
- V2-3：上传音频导入、转码、失败清理测试。

## 8. 风险

- 当前 `bailian` provider 已接 `fun-asr-realtime`，但 V2-1 要把 draft、timestamp metadata、capabilities 补齐；不要另起一个重复 provider。
- 当前 CredentialsVault（凭据存储）以 active provider（当前服务商）读取 ASR 凭据。会议专用 ASR 如果要不影响全局短口述，需要新增“按 provider id 读取 ASR 凭据”的最小后端能力。
- Fun-ASR Realtime 官方接口支持 timestamp（时间戳）和 interim/final（临时/最终）结果，但不同 endpoint（端点）形态可能随百炼控制台配置变化；V2-1 不新增 Workspace ID / region UI，只复用已有 endpoint 配置。
- local ASR（本地语音转文字）与 online ASR（在线语音转文字）capabilities 不同，UI 不能显示 provider 实际不支持的假实时能力。
- pause / resume 会产生多个 provider session（服务商会话），sequence（序号）不能跨 session 当作全局唯一。
- 如果 V2-2 本地 diarization 对齐依赖的 word timestamp（词级时间戳）不可用，必须能降级到 sentence timestamp（句级时间戳）或保持“未区分”，不能重跑在线 ASR 作为默认补救。
- system audio capture、悬浮入口和字幕级流式体验都需要独立产品与平台验证，提前塞进 V2 会拖慢 ASR/diarization 主线。

## 9. 阶段推进建议

推荐顺序：

1. 先执行 V2-1，把 meeting ASR config、capabilities、draft/final、timestamp metadata 做稳。
2. 人工验证一小时会议稳定性，包括网络中断。
3. 再进入 V2-2 本地 VAD + speaker diarization 对齐。
4. V2-2 验证通过后，再做 V2-3 上传音频，因为上传音频也会复用 diarization / transcript alignment 能力。
5. V3 再评估 system audio capture、悬浮入口、字幕级低延迟流式和更多导出。
