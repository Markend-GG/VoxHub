# 会议录音 V2-3 音频文件导入识别 Implementation Plan

**Goal:** 允许用户从本机选择已有音频文件，按所选会议 ASR 模型的类型动态路由到云端接口或本地引擎，生成与现场录音一致的 `MeetingRecord`（会议记录）、说话人原文和会议总结，并完整支持重试、取消、重启恢复和音频保留。

**Architecture:** 用户文件只作为只读 source（源文件）。应用先校验并流式复制 / 解码为受管标准 WAV，再根据用户选中的 `AsrModelRef`（ASR 模型引用）从后端 capability registry（能力注册表）解析 `runtimeKind=cloud|local`。云端模型走文件流上传和 provider adapter（服务商适配器），本地模型走本地 batch ASR engine（批量识别引擎）；说话人分离作为独立配置叠加到识别管线，最终通过同一 transcript revision（原文修订版本）和 summary（总结）链路落盘。

**Tech Stack:** Tauri native file dialog（原生文件选择器）、Rust 异步文件 I/O、现有 `MeetingStore`、受管 `meeting-recordings` 目录、V2-2 `MeetingAudioSource`、百炼异步录音文件识别、本地会议处理模型包、React / TypeScript。

---

状态：ready for implementation after approval
日期：2026-08-12
上级计划：`docs/meeting-recording-v2-plan.md`
前置计划：`docs/meeting-recording-v2-2-diarization-plan.md`
验收清单：`docs/meeting-recording-v2-acceptance-checklist.md`

## 1. 产品定义

“导入音频”不是把用户本地路径直接交给 ASR，也不是默认上传云端：

```text
用户选择本地文件
  -> 导入到 OpenLess 受管会议音频目录
  -> 选择会议 ASR 模型
  -> 后端解析模型类型
       cloud -> 上传并调用云端 ASR
       local -> 调用本地 ASR 引擎
```

入口放在会议页，使用“导入音频”命令。它与“开始会议”并列，不放进设置页。

首版每次导入一个音频文件，生成一场会议。批量导入、视频文件、多个文件拼接和文件夹监听不进入 V2-3。

## 2. 导入弹窗

用户选择文件后显示：

- 文件名、格式、大小、预计时长、声道和采样率。
- 会议标题，默认使用文件名去除扩展名。
- ASR 模型：下拉选择可用于 meeting file transcription（会议文件转写）的云端或本地模型；默认继承本场 / 会议设置中的会后 ASR 模型。
- 云端首期选项：`fun-asr`（默认）、`paraformer-v2`（备选）。
- 本地选项：显示已经注册且具备 batch file transcription（批量文件转写）能力的本地模型及下载 / 就绪状态。
- 区分发言人：关闭、云端处理、本地处理，默认继承设置，但可以为本次导入覆盖。
- 预计发言人数：自动或指定数字。
- 是否生成会议总结：默认开启；关闭时完成原文后停在 completed（已完成）且 summary 为空。

“关闭”只表示不区分发言人，不决定 ASR 来自云端还是本地。为避免把“ASR 服务来源”和“是否区分发言人”混为一个开关，导入弹窗应显示两个独立字段：

```text
ASR 模型：fun-asr | paraformer-v2 | 已安装的本地模型...
区分发言人：关闭 | 云端处理 | 本地处理
```

路由与组合规则：

- 云端 ASR + 关闭：调用所选云端文件 ASR，关闭 diarization。
- 云端 ASR + 云端处理：调用所选云端文件 ASR，并由同一云端任务开启 diarization。
- 云端 ASR + 本地处理：先调用云端 ASR 获取带时间戳原文，再在本地对受管音频执行 diarization 并做 transcript alignment（原文对齐）；音频仍因 ASR 上传云端。
- 本地 ASR + 关闭：本地 VAD / 有界分块后执行本地 batch ASR。
- 本地 ASR + 本地处理：本地 diarization-first（先区分说话人），再按 speaker windows（说话人时间窗）执行所选本地 batch ASR。
- 本地 ASR + 云端处理：首版标记为 unsupported（不支持）。当前 `fun-asr` / `paraformer-v2` 提供的是 ASR 与 diarization 合并任务，不是独立的云端 diarization API；若调用它们会重新执行云端 ASR，违背用户选择本地 ASR 的意图。UI 禁用并说明原因，不静默改成其他组合。

后端必须通过能力注册表验证组合；前端禁用状态只是提示，不能作为安全或路由依据。

## 3. 格式与首版范围

### 3.1 必须支持

- 标准 PCM WAV。
- 当前现场会议生成的 16 kHz / 单声道 / 16-bit WAV。

### 3.2 常见压缩音频

MP3、M4A/AAC、FLAC、OGG/Opus 是否进入首个开发批次，必须由一次跨平台 decoder（解码器）POC 决定。实现前选择一个可在 Windows/macOS 稳定打包、支持中文路径、无需用户单独安装程序的音频解码方案。

在 decoder POC 通过前：

- UI 文件筛选器只开放已验证格式。
- 不通过仅修改文件扩展名声称支持。
- 不静默调用用户系统中可能不存在的 `ffmpeg`。

视频容器、DRM、网络 URL 和多轨工程文件明确不进入 V2-3。

### 3.3 统一输出

所有输入都规范化为：

```text
PCM WAV
sample rate = 16_000 Hz
channels = 1
bits per sample = 16
```

立体声按明确的 downmix（下混）规则转为单声道；禁止只取左声道。解码和重采样必须按块处理，不能把完整压缩文件和完整 PCM 同时放进内存。

## 4. 受管文件与目录

建议目录：

```text
meeting-import-staging/<import-job-id>.partial
meeting-recordings/<meeting-id>/part-0001.wav
```

规则：

- 用户源文件只读，应用绝不修改、移动或删除它。
- 文件被接受后创建 `importJobId` 和 `meetingId`，写 staging `.partial`。
- 校验、解码、重采样和写入成功后 flush + sync，并原子改名 / 移动为 `part-0001.wav`。
- 只在受管 WAV 完整可读后进入 ASR；`.partial` 永远不能被识别、播放或计入 retention。
- 导入期间只持久化源文件名和媒体元数据，不持久化用户源文件绝对路径。
- 应用在复制过程中退出时，重启后将未完成 staging 标为失败并清理 `.partial`；用户重新选择源文件重试。
- 受管 WAV 完成后，后续任务不再依赖源文件存在。

## 5. 业务状态机

建议独立 `MeetingImportState`，不要继续扩展 `MeetingStatus`：

```text
selected
  -> validating
  -> importing
  -> ready
  -> transcribing
  -> applying
  -> summarizing
  -> completed

任一可恢复阶段 -> failed -> retry
selected/validating/importing/transcribing -> cancelling -> cancelled
```

字段建议：

```text
MeetingImportConfig
  sourceFileName: string
  sourceFormat: string
  asrModelRef: { providerId: string, modelId: string }
  resolvedAsrRuntimeKind: cloud | local
  diarizationMode: off | cloud | local
  diarizationModelId?: string
  expectedSpeakerCount?: u32
  generateSummary: bool
  processingRevision: u32

MeetingImportState
  status: selected | validating | importing | ready | transcribing
          | applying | summarizing | completed | failed
          | cancelling | cancelled
  progress?: number
  errorCode?: string
  errorMessage?: string
  attempt: u32
```

`resolvedAsrRuntimeKind` 由后端解析并快照，只用于持久化、恢复和审计；前端不能单独提交该字段决定路由。建议统一接口：

```text
MeetingAsrModelDescriptor
  providerId: string
  modelId: string
  displayName: string
  runtimeKind: cloud | local
  supportsMeetingFile: bool
  supportsDiarization: bool
  supportsSpeakerCount: bool
  readiness: ready | missing | unavailable

resolve_meeting_asr_model(modelRef) -> MeetingAsrModelDescriptor
```

首期云端注册 `bailian/fun-asr` 和 `bailian/paraformer-v2`；本地模型沿用现有 provider/model catalog（服务商 / 模型目录），只有 `supportsMeetingFile=true` 且 readiness（就绪状态）可用的模型才能开始导入。

导入和说话人处理可以在实现中复用同一个 job runner（任务执行器），但对前端保留导入阶段，用户需要区分“正在把文件导入应用”和“正在识别内容”。

## 6. 创建记录的时机

1. 文件选择 / 基础校验失败：不创建会议记录，只在弹窗提示。
2. 开始写 staging：创建 `Draft` MeetingRecord 和 import state，会议列表显示“正在导入”。
3. 受管 WAV 完成：写入音频元数据和 duration，状态进入 ready/transcribing。
4. ASR 成功：写 staging transcript revision，完整校验后原子激活。
5. 总结成功或用户关闭总结：会议进入 completed。
6. 导入或识别失败：保留可用的受管 WAV 和错误状态，允许重试；没有完整 WAV 时要求用户重新选择文件。

失败记录不能伪装成 completed，也不能生成空标题、空原文和空总结的正常会议。

## 7. 动态 ASR 路由

统一入口：

```text
managed part-0001.wav + MeetingImportConfig
  -> resolve_meeting_asr_model(asrModelRef)
  -> validate ASR + diarization combination
  -> descriptor.runtimeKind == cloud
       ? run_cloud_meeting_file_asr(...)
       : run_local_meeting_file_asr(...)
```

路由判断必须发生在 Rust 后端 job runner（任务执行器）中。React 前端只提交模型引用和说话人处理选择，不提交任意 endpoint（端点）、本地路径或 runtime kind（运行类型）。

### 7.1 云端模型路径

```text
source file
  -> validate / stream decode / normalize
  -> managed part-0001.wav
  -> MeetingAudioSource::SingleWav
  -> streaming temporary upload
  -> selected cloud model
       default: fun-asr
       alternative: paraformer-v2
       diarization_enabled = (diarizationMode == cloud)
       speaker_count = expectedSpeakerCount when supported
  -> poll provider task
  -> download structured transcript
  -> parse sentence timestamps and optional speaker_id
  -> imported transcript revision
  -> optional summary
```

边界：

- 云端只接 `fun-asr` 和 `paraformer-v2`，默认 `fun-asr`；不得根据失败、文件格式或语言自动换模型。
- 不把本地路径提交给百炼；先上传应用可读的受管文件，再提交临时对象引用。
- 上传和任务状态复用 V2-2；有 providerTaskId 时应用重启优先恢复轮询。
- `diarizationMode=off` 时结果统一显示“未区分”，但时间戳和文本仍按结构化句子保存。
- `diarizationMode=local` 时先保存云端 ASR staging result，再运行本地 diarization alignment；两步完整成功后才原子提交最终 revision。

### 7.2 本地模型路径

本地模型路径不再是固定分支，而是 `descriptor.runtimeKind == local` 的路由结果。具体 ASR model/runtime 由 `asrModelRef` 决定，任务执行器只依赖统一 `MeetingBatchTranscriber` 接口：

```text
transcribe_window(modelRef, pcmWindow, absoluteStartMs, cancelToken)
  -> text + optional token timestamps
```

## 8. 本地 ASR 与说话人处理组合

### 8.1 本地会议处理模型包

当用户选择本地 ASR 且开启本地说话人处理时，模型能力组合至少包含：

```text
batchAsrModel
speakerSegmentationModel
speakerEmbeddingModel
clusteringDefaults
supportedLanguages
maxRecommendedDurationMs
memoryClass
manifestVersion
checksums
```

ASR 模型与 diarization model package（说话人模型包）可以独立选择，但任务开始前必须同时 readiness（就绪）；UI 可以组合展示为“本地会议处理配置”，不能要求所有文件必须属于同一个固定模型包。

### 8.2 开启说话人分离

推荐顺序：

```text
managed WAV
  -> decode waveform
  -> local speaker diarization
  -> SpeakerTurn[]
  -> build ASR windows from turns
       merge adjacent same-speaker turns across short gaps
       split windows above model max duration
       add bounded context padding without changing final timestamps
  -> local batch ASR per window
  -> normalize text
  -> TranscriptSegment(speakerId, startMs, endMs, text)
  -> imported/local revision
  -> optional summary
```

采用 diarization-first（先区分发言人、再按时间窗转写）的原因是现有本地 `RawTranscript` 不能统一保证词级时间戳。此流程不要求把无时间戳的整段文本再反向猜测到 speaker turn。

处理规则：

- 相邻同 speaker 且间隔很短的 turns 可以合并，减少大量 1-2 秒 ASR 请求；内部阈值通过评测确定，不做用户设置。
- 单个 window 超过本地 ASR 的稳定时长时按静音点拆分，并保留绝对会议时间。
- context padding（上下文填充）只改善识别，不扩大发言人的显示时间范围。
- overlap（抢话）窗口首版标记为低置信度，不承诺同时转写两位说话人的重叠内容。
- 某个 window ASR 失败时整个 staging revision 不提交；保留成功中间结果只用于诊断 / 重试，不展示为完整原文。

### 8.3 关闭说话人分离

```text
managed WAV
  -> VAD / duration-bounded chunks
  -> local batch ASR per chunk
  -> absolute chunk timestamps
  -> speakerLabel = 未区分
  -> imported/local revision
  -> optional summary
```

不能把整段两小时 waveform 一次交给只为短口述设计的本地 ASR provider。`MeetingBatchTranscriber`（会议批量转写器）应从现有本地 runtime 复用模型加载和单段识别，但独立负责文件分块、时间轴、取消、进度和结果聚合。

## 9. 取消、重试和恢复

取消：

- validating/importing：停止读取源文件，删除 `.partial`，保留或删除 draft record 由用户确认；首版默认删除无有效音频的 draft。
- cloud uploading：取消本地请求，释放文件句柄；远端残留按临时对象生命周期清理。
- cloud running：停止本地轮询并标记 cancelled；若 API 支持明确取消则调用，否则不宣称远端任务已取消。
- local running：设置 cancellation token（取消令牌），当前模型调用结束后停止后续窗口；不提交部分 revision。

重试：

- 受管 WAV 完整时，从 transcribing 阶段重试，不要求重新选择源文件。
- `.partial` 或受管 WAV 缺失 / 损坏时，提示重新选择文件；不得保存旧源绝对路径后静默访问。
- 修改 ASR 模型、区分发言人方式、说话人模型或预计人数后重试时，重新解析能力并创建新的 processing revision。

应用重启：

- importing 且只有 `.partial`：标记失败、清理 partial、要求重新选择。
- cloud uploading：重新上传。
- cloud running 且有 providerTaskId：恢复轮询。
- local running：从受管 WAV 重新开始当前 attempt；已完成窗口可通过内部 checkpoint（检查点）优化，但首版允许幂等重跑。
- applying：按 V2-2 revision 规则恢复或回滚 staging。

## 10. 音频保留与删除

- 受管音频从 ready 到识别终态持有 processing hold，不受 retention 清理影响。
- 完成后按最近 N 场会议音频策略处理；导入音频与现场录音使用同一计数，不另建隐形保留池。
- retention=0 时在识别 / 总结所需阶段保留，任务完成或用户取消后删除受管副本。
- 删除会议时删除受管副本、staging、revision 和本地任务记录；不删除用户源文件。
- 失败且可重试的导入默认保留受管 WAV。用户主动“取消并删除音频”后才释放并删除。

## 11. IPC 与事件建议

```text
choose_meeting_audio_file() -> probed file metadata
list_meeting_file_asr_models() -> MeetingAsrModelDescriptor[]
start_meeting_audio_import(options) -> MeetingRecord
cancel_meeting_audio_import(id)
retry_meeting_audio_import(id, options?)

meeting:import-state
meeting:post-processing-state
meeting:record-updated
meeting:error
```

前端不能把任意 path（路径）字符串传给后端读取。原生文件选择器返回的路径只在可信 Tauri command（命令）内部转换为短生命周期 selection token（选择令牌），`start_meeting_audio_import` 使用该 token，防止路径遍历和错误读取其他文件。

## 12. 实施顺序

1. 文件选择、selection token、probe 和格式拒绝。
2. staging、流式复制 / WAV 规范化、进度、取消和清理。
3. import state、Draft MeetingRecord、重启恢复和 processing hold。
4. 建立统一 ASR 模型 descriptor、能力注册表、组合校验和后端动态 router（路由器）。
5. 复用 V2-2 云端 `MeetingAudioSource -> fun-asr/paraformer-v2` adapter。
6. 接入现有本地 provider/model catalog 和统一 `MeetingBatchTranscriber`。
7. 本地 diarization-first + selected local ASR windows 管线，以及 cloud ASR + local diarization alignment 管线。
8. UI、总结开关、失败重试、会议删除和 retention 回归。
9. 常见压缩音频 decoder POC；只有通过后才扩展文件筛选器。

## 13. 验证矩阵

自动测试：

- 中文路径、超长文件名、零字节、伪扩展名、损坏 WAV、非 PCM WAV、多声道和异常 header。
- 流式复制 / 规范化的取消、磁盘不足、应用退出、partial 清理和原子完成。
- selection token 一次性使用、过期和错误 token 拒绝。
- 用户源文件 hash / mtime 不变，删除会议不影响源文件。
- 云端开启 / 关闭 diarization 的请求和结果解析。
- 默认模型解析为 `fun-asr`，用户可选择 `paraformer-v2`；两个模型分别验证开启 / 关闭云端 diarization。
- 云端模型路由一定调用云端 adapter，本地模型路由一定调用本地 engine；伪造 `runtimeKind` 不得改变后端路由。
- 云端 ASR + 本地 diarization 的两阶段 staging、失败回滚和原子提交。
- 本地 ASR + 本地 diarization、以及本地 ASR + 关闭的窗口生成、绝对时间轴和稳定 speakerId。
- 本地 ASR + 云端 diarization 组合被 UI 和后端共同拒绝，且不会上传音频。
- 所选模型失败时不自动换用其他云端或本地模型；用户改选后重试产生新 revision。
- retry 产生新 revision，不覆盖当前可用原文。
- retention=0 与失败可重试的音频生命周期。

人工验证：

- 5 分钟、30 分钟、60 分钟、120 分钟文件。
- 单人、双人、多人、抢话、长静音和背景音乐。
- 云端上传中断网、任务运行中退出、恢复轮询。
- 本地模型缺失、下载失败、磁盘不足、内存预检失败和中途取消。
- 导入成功后编辑标题、重命名发言人、生成 / 重试总结、播放、导出和删除。
- Windows 中文路径与 macOS 路径；常见压缩格式只测试 POC 已宣布支持的集合。

## 14. 成功标准

- 用户能明确知道文件是否会上传云端。
- 用户源文件在所有成功、失败、取消和删除路径中保持不变。
- 大文件导入、规范化和云端上传不通过完整文件多份内存复制实现。
- 云端默认使用 `fun-asr`，可选择 `paraformer-v2`；本地 ASR 模型由用户选择并通过后端能力注册表动态路由。
- 选择本地 ASR 且不选择云端说话人处理时不产生网络上传；任何会上传音频的组合必须在 UI 中明确显示。
- ASR 模型和说话人模型分别完成 readiness 检查，不出现半可用配置。
- 任务状态可以在会议列表和详情恢复，失败有重试或清理路径。
- 导入记录最终复用现有会议详情、总结、播放、导出和删除能力，不另建第二套历史页。
