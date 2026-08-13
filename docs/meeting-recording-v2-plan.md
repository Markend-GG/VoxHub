# 会议录音 V2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 V1 已完成的会议记录基础上，升级会议 realtime ASR（实时语音转文字）、会后 ASR、云端 / 本地 speaker diarization（说话人分离）、会议音频文件流式上传和音频文件导入识别能力。

**Architecture:** V2 按管线分层推进：meeting audio（会议音频）采集与保留、meeting ASR config（会议语音转文字配置）、ASR provider capabilities（语音转文字服务商能力声明）、realtime transcript event（实时原文事件）、post-processing（后处理）和导入能力分阶段接入。每个阶段必须能单独运行和验证，不能要求一次性重构全部 ASR（语音转文字）体系。

**Tech Stack:** Tauri 2、Rust、React、TypeScript、Windows MSVC toolchain（工具链）、现有 CredentialsVault（凭据存储）、现有 meeting IPC（进程间调用）和 event（事件）。

---

状态：revised for V2-2 / V2-3 implementation planning
日期：2026-07-08  
最新修订：2026-08-13
基线分支：`codex/voxhub-main`（独立功能分支：`codex/meeting-post-asr-routing-20260812`）
关联文档：

- `docs/meeting-recording-v1-spec.md`
- `docs/future-meeting-recording-notes.md`
- `docs/local-asr-pipeline-upgrade/README.md`
- `docs/local-asr-pipeline-upgrade/05-pipeline-options-contract.md`
- `docs/local-asr-pipeline-upgrade/09-realtime-preview.md`
- `docs/meeting-recording-v2-1-realtime-asr-plan.md`
- `docs/meeting-recording-v2-2-diarization-plan.md`
- `docs/meeting-recording-v2-3-audio-import-plan.md`
- `docs/meeting-recording-v2-acceptance-checklist.md`

## 0. 最新进展同步（2026-08-13）

- V2-1：会议独立 ASR 设置、draft/final（临时 / 最终识别）、原文 metadata（元数据）、provider session（服务商会话）防污染和本场实际 realtime ASR 配置快照已进入代码，并通过 TypeScript、前端 build、MSVC `cargo check` 及相关 Rust 自动测试；真实百炼会议和 Tauri 人工验收尚未完成，因此仍标记为 `partial`（部分完成）。
- V2-2：会后 ASR 模型选择、会议音频文件流式上传、云端 `fun-asr` / `paraformer-v2` 任务框架、revision（原文修订版本）、任务恢复和音频生命周期已进入代码；本地说话人模型管理、sherpa-onnx 推理、SpeakerTurn（说话人时间段）生成和句子级时间戳对齐也已完成自动测试。真实百炼、本地模型下载与推理、Tauri UI、应用重启及 30 / 60 / 120 分钟资源验证尚未完成，因此相关验收项仍为 `partial`。
- V2-3：音频文件导入、受管 WAV、可信 selection token（选择令牌）、按模型能力注册表动态路由云端 / 本地 ASR、取消 / 重试 / 恢复、兼容组合校验和会议详情复用已经进入代码并完成自动测试。真实云端 / 本地模型调用、应用重启、磁盘不足、Tauri UI 全流程和长音频资源测试尚未完成，因此 MR-V2-201～212 均为 `partial`，不能标记为 `done`。
- 状态权威统一放在 `docs/meeting-recording-v2-acceptance-checklist.md`；阶段计划写完不等于功能已经实现。

## 1. 背景

V1 已提供独立会议页、麦克风录音、暂停 / 继续 / 停止、会议原文、自动 summary（总结）、编辑、删除、Markdown 导出、重写总结和音频存在时重新转写。

V2 的核心问题不是继续扩展总结模板，也不是一次性做完整视频会议工具，而是把“会议录入管线”做扎实：

- 会议中要更快看到文字。
- provider（服务商）能力差异要被显式建模，不能靠 provider id（服务商标识）到处硬编码。
- online ASR（在线语音转文字）和 local ASR（本地语音转文字）要能在会议场景内被独立选择。
- V2-1 不执行 speaker diarization（说话人分离），但必须保存足够的 audio（音频）、final segments（最终片段）和 timestamp（时间戳），支撑 V2-2 云端 / 本地会后处理。
- V2-2 增加独立“会后 ASR 模型”下拉，默认 `fun-asr`、备选 `paraformer-v2`；“关闭 / 云端处理 / 本地处理”三态只决定 speaker diarization（说话人分离）方式。设置保存全局默认，预计发言人数在开始会议时选择并快照到单场会议。
- 会议中的 realtime ASR 与会后 ASR 分别配置和快照。停止会议后始终使用所选会后 ASR 重新识别完整音频；模型失败时不自动切换，用户可以改选另一个模型后重试。
- V2-3 允许用户选择本地音频文件生成会议记录，并根据所选 ASR 模型的 `cloud/local` 类型动态路由到云端接口或本地引擎，复用 V2-2 的会议记录结构、任务状态和音频生命周期。
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

### V2-2：会后 ASR 与云端 / 本地说话人处理

目标：停止会议后，基于固化的会议音频使用所选会后 ASR 模型生成整理后原文，再按本场配置关闭说话人分离、使用云端说话人分离或使用本地说话人分离，最后生成会议总结。

范围：

- 设置 -> 服务新增“区分发言人”：关闭、云端处理、本地处理。
- 设置 -> 服务新增“会后 ASR 模型”下拉：`fun-asr` 默认，`paraformer-v2` 备选。
- 关闭区分发言人时仍执行所选会后 ASR，只关闭 `diarization_enabled`；不能把“关闭说话人分离”解释成“跳过会后 ASR”。
- 云端从会议录音分片构造只读 `MeetingAudioSource`（会议音频源），通过文件流上传临时对象，禁止把完整 PCM / WAV 复制进内存。
- 本地说话人处理下载并选择 speaker diarization model package（说话人分离模型包），使用本地 segmentation（分割）、speaker embedding（说话人特征）和 clustering（聚类）得到 speaker turns（说话人时间段），再对齐所选会后 ASR 的时间戳；当前仍会为会后 ASR 上传完整音频。
- 预计发言人数支持“自动”或明确数字；云端首期只用于结果校验，本地映射为 clustering 的 `num_clusters`。
- 处理结果先写入新的 transcript revision（原文修订版本），完整校验后再原子切换为当前版本；失败时保留实时原文和旧总结。
- 会后处理完成后才自动生成 summary（总结）；用户可以在失败时按原模型重试、改选模型后重试，或明确选择“沿用实时原文并生成总结”。
- 支持会后手动重命名 speaker（说话人），重命名只更新会议级 `SpeakerProfile` 映射，不逐段改写模型标签。

明确不做：

- 实时 speaker label（实时说话人标签）。
- 用 local VAD 改写 V2-1 的 realtime draft/final 显示节奏。
- 云端 `fun-asr-mtl`、Qwen 文件模型或 `fun-asr` / `paraformer-v2` 以外的会后 ASR 模型。
- 云端失败后静默切换本地，或本地失败后静默上传云端。
- 不做 `fun-asr` 失败后自动切换 `paraformer-v2`，或反向自动切换。

执行计划见：`docs/meeting-recording-v2-2-diarization-plan.md`。

### V2-3：音频文件导入生成会议记录

目标：允许用户上传已有音频文件生成会议记录，不要求必须通过 OpenLess 现场录音。

范围：

- 复用 `MeetingRecord`、`TranscriptSegment` 和 summary（总结）结构。
- 用户选择的源文件只读，应用流式复制到受管会议目录并转为标准 16 kHz / 单声道 / 16-bit PCM WAV；处理中不依赖用户原文件继续存在。
- 导入弹窗选择可用于会议文件转写的 ASR 模型；后端通过能力注册表解析模型类型。云端模型调用云端文件 ASR adapter（适配器），本地模型调用本地 batch ASR engine（批量识别引擎），不得由前端单独传 `cloud/local` 决定路由。
- 云端首期模型为 `fun-asr`（默认）和 `paraformer-v2`（备选）；本地模型来自已有 provider/model catalog（服务商 / 模型目录）并要求 `supportsMeetingFile=true`。
- 说话人分离与 ASR 来源独立选择：本地 ASR + 本地分离采用 diarization-first；本地 ASR + 关闭采用 VAD / 有界分块；云端 ASR 可以关闭云端分离、开启云端分离，或转入本地分离对齐。
- 明确格式、大小、时长、转码、失败重试、取消、应用重启恢复、源文件和临时文件清理。
- 导入副本在任务完成前受 processing hold（处理占用）保护，不受最近 N 场音频清理影响；完成后再按会议音频保留策略处理。
- 导入失败不得生成空白 completed meeting（已完成会议），不得修改或删除用户源文件。

执行计划见：`docs/meeting-recording-v2-3-audio-import-plan.md`。

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
  -> Audio recorder -> meeting-recordings/<meeting-id>/part-*.wav
  -> Realtime ASR -> draft/final -> realtime transcript revision
  -> Stop returns after audio/transcript persistence
  -> Meeting post-processing job
       -> streaming file upload -> selected post-meeting ASR
            default fun-asr | alternative paraformer-v2
       -> diarization off: keep normalized ASR segments as 未区分
          or cloud: read provider speaker_id
          or local: local diarization -> speaker turns -> transcript alignment
  -> atomically activate final transcript revision
  -> Summary -> export/history

Local audio import
  -> select source file -> validate -> stream-copy/decode to managed WAV
  -> resolve selected ASR model descriptor
       -> cloud model: streaming file upload -> cloud adapter
       -> local model + local diarization: diarization -> speaker windows -> local batch ASR
       -> local model + diarization off: VAD / bounded chunks -> local batch ASR
       -> cloud model + local diarization: cloud ASR -> local speaker alignment
  -> MeetingRecord -> Summary -> export/history
```

关键边界：

- meeting session（会议会话）继续与短口述 session（短口述会话）分离。
- ASR provider runtime（语音转文字运行时）可以共用，但 effective config（实际配置）必须可区分“短口述”和“会议”。
- `TranscriptSegment` 仍是会议原文持久化的唯一主数据结构，V2-1 只给它补 metadata（元数据），不引入第二套会议原文模型。
- draft（临时识别）只走 event（事件），不落盘。
- provider capabilities（服务商能力声明）决定 UI 能显示哪些选项和后端能启用哪些参数。
- `MeetingStatus` 不承载录音、导入、说话人处理和总结的全部组合；V2-2 新增独立、可持久化的 post-processing state（后处理状态）。
- 会议开始或音频导入任务创建时快照 realtime ASR、会后 / 文件 ASR 模型、解析后的 runtime kind（运行类型）、说话人处理模式、预计发言人数和 processing revision（处理版本），运行中修改全局设置只影响下一任务。
- 音频 retention（保留）与 processing hold（处理占用）分离；保留数量为 0 时，也必须等待后处理结束后再删除临时音频。

## 6. 与 future notes 的对齐检查

已覆盖到 V2 总路线：

- 会议专用 ASR 配置：V2-1。
- VAD 与更自然分段：V2-1 只做 provider capability（服务商能力）和简单 silence preset（静音档位）；V2-2 的 local VAD 只服务于本地 speaker diarization，不改实时分段 UI；高级自然分段 UI 放 V3。
- 说话人分离：V2-2 做停止后云端完整重转写或本地对齐；实时 speaker label 放 V3。
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
- V2-1 自身不执行在线后处理；V2-2 使用独立会后 ASR 模型快照和说话人处理快照，不做云端 / 本地或 `fun-asr` / `paraformer-v2` 的静默 fallback（降级切换）。
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
- V2-2：新增 `fun-asr` 默认 / `paraformer-v2` 备选模型下拉、文件流上传、双模型 adapter、结构化结果解析、本地模型 readiness（就绪状态）、diarization alignment（说话人分离对齐）、状态恢复和音频 processing hold 测试。
- V2-3：新增音频文件导入、流式复制 / 转码、模型 capability registry（能力注册表）、云端 / 本地动态路由、组合校验、取消、重试和失败清理测试。

## 8. 风险

- 当前 `bailian` provider 已接 `fun-asr-realtime`，V2-1 的 draft、timestamp metadata（时间戳元数据）和相关会议事件结构也已存在；进入 V2-2 前应先按 V2-1 验收清单补齐真实验证，不要另起一个重复 provider。
- 当前 CredentialsVault（凭据存储）以 active provider（当前服务商）读取 ASR 凭据。会议专用 ASR 如果要不影响全局短口述，需要新增“按 provider id 读取 ASR 凭据”的最小后端能力。
- Fun-ASR Realtime 官方接口支持 timestamp（时间戳）和 interim/final（临时/最终）结果，但不同 endpoint（端点）形态可能随百炼控制台配置变化；V2-1 不新增 Workspace ID / region UI，只复用已有 endpoint 配置。
- local ASR（本地语音转文字）与 online ASR（在线语音转文字）capabilities 不同，UI 不能显示 provider 实际不支持的假实时能力。
- pause / resume 会产生多个 provider session（服务商会话），sequence（序号）不能跨 session 当作全局唯一。
- 当前百炼异步客户端会复制完整 PCM、构造完整 WAV 并用 `Part::bytes` 再复制上传；V2-2 不得复用该内存路径处理会议文件，必须增加 path / stream source（文件 / 流式音频源）。
- 所选 `fun-asr` 或 `paraformer-v2` 返回的结构化句子是本次整理后原文的权威来源，不把 `speaker_id` 强行贴回 realtime ASR 的旧文本，避免跨模型文本和时间戳错配。
- 动态路由必须以后端模型注册表为权威，不能信任前端传入的 `runtimeKind`；否则可能把本地模型意外路由到云端并上传音频。
- 如果 V2-2 本地 diarization 对齐依赖的 word timestamp（词级时间戳）不可用，必须降级到 sentence timestamp（句级时间戳）或保持“未确认”，不能静默调用云端补救。
- sherpa-onnx `OfflineSpeakerDiarization::process(&[f32])` 接收完整 waveform（波形）；本地首版必须在真实机器基准测试后写入明确的时长 / 内存能力上限，超过上限时在开始前阻止执行并提示改用云端，不能运行到内存耗尽。
- system audio capture、悬浮入口和字幕级流式体验都需要独立产品与平台验证，提前塞进 V2 会拖慢 ASR/diarization 主线。

## 9. 阶段推进建议

推荐顺序：

1. 先执行 V2-1，把 meeting ASR config、capabilities、draft/final、timestamp metadata 做稳。
2. 人工验证一小时会议稳定性，包括网络中断。
3. 执行 V2-2 契约与任务状态机，再完成 `fun-asr` 默认、`paraformer-v2` 备选的会后模型下拉、统一百炼 adapter 和会议音频文件流式上传。
4. 在云端链路稳定后完成本地模型下载 / readiness、本地 diarization 和 transcript alignment。
5. V2-2 验证通过后执行 V2-3，音频文件导入复用同一 post-processing job、模型能力注册表、音频源和 revision 提交机制。
6. V3 再评估 system audio capture、悬浮入口、字幕级低延迟流式和更多导出。
