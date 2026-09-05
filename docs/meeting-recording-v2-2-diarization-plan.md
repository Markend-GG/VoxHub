# 会议录音 V2-2 会后 ASR 与说话人处理 Implementation Plan

**Goal:** 为已录制会议增加可选的会后 ASR 模型和“关闭 / 云端处理 / 本地处理”三态 speaker diarization（说话人分离）能力；停止会议后默认用 `fun-asr`、可改选 `paraformer-v2` 生成整理后原文，再按说话人处理方式生成“发言人 + 时间 + 文本”并进入现有会议总结链路。

**Architecture:** 录音停止只负责固化音频和实时原文并创建可恢复的 post-processing job（后处理任务）。会后 ASR 默认执行云端 `fun-asr`，用户可在下拉框改选云端 `paraformer-v2`；识别完成后，根据说话人设置关闭分离、读取云端 `speaker_id`，或在本地执行 speaker segmentation（说话人分割）+ embedding（声学特征）+ clustering（聚类）并与会后原文时间戳对齐。最终结果提交新的 transcript revision（原文修订版本），不直接修改正在使用的原文。

**Tech Stack:** Tauri 2、Rust、React、TypeScript、现有 `MeetingStore`、会议分片 WAV、阿里云百炼异步录音文件识别、sherpa-onnx `OfflineSpeakerDiarization`、Windows MSVC toolchain（工具链）。

---

状态：ready for implementation after approval
日期：2026-08-12
上级计划：`docs/meeting-recording-v2-plan.md`
验收清单：`docs/meeting-recording-v2-acceptance-checklist.md`

## 1. 已确认决策

- 设置入口：设置 -> 服务 -> 区分发言人。
- 设置值：关闭、云端处理、本地处理。
- 同一区域增加“会后 ASR 模型”下拉；默认 `fun-asr`，备选 `paraformer-v2`。
- 全局设置只是下一场会议的默认值；开始会议时将最终配置快照到 `MeetingRecord`。
- 预计发言人数在开始会议时选择：自动或明确数字。开始后锁定；会后手动重新处理时可以重新选择。
- 首版只做会后 batch（批处理），不做会议中的实时说话人标签。
- 会议中的 realtime ASR（实时语音识别）继续使用本场会议 ASR 设置；会议结束后的 cloud file ASR（云端文件语音识别）是独立选择，不强制改写实时模型。
- 云端会后模型下拉首期只有 `fun-asr` 和 `paraformer-v2`；默认 `fun-asr`，`paraformer-v2` 是用户可选备选。
- “备选”表示用户在开始会议或重新处理时主动选择，不表示 `fun-asr` 失败后自动改用 `paraformer-v2`。失败时保持原模型并提供重试或改选模型后重试。
- 选中云端模型返回的句子、时间戳和 `speaker_id` 是本次整理后原文的权威来源；不把云端标签硬贴回文字可能不同的实时原文。
- “本地处理”只表示 speaker diarization 在本机执行。当前会后 ASR 选项均为云端模型，因此完整会议音频仍会上传给所选 `fun-asr` 或 `paraformer-v2`；UI 必须明确提示，不能把它描述为全本地或不上传。
- 停止会议不等待后处理结束；停止成功后进入后台任务，会议详情持续显示进度。
- 失败不得覆盖实时原文、人工编辑内容或已有总结。
- 后处理完成后才自动生成总结；失败时允许重试或“跳过区分并生成总结”。

## 2. 产品范围

### 2.1 设置页

关闭：

- 不下载说话人模型，不执行 speaker diarization。
- 停止会议后仍使用所选会后 ASR 模型重新识别完整会议音频，云端请求设置 `diarization_enabled=false`；识别完成后再生成总结。

云端处理：

- 显示“会后 ASR 模型”下拉框：`fun-asr`（默认）、`paraformer-v2`（备选）。
- 下拉项来自后端 capability registry（能力注册表），只显示支持 file transcription（文件转写）和 speaker diarization（说话人分离）的模型。
- 复用百炼 API Key、endpoint（端点）和 CredentialsVault（凭据存储）。
- 保存前验证百炼凭据和所选模型的基本可用性；正式识别错误仍在单场任务中展示。
- 下拉只决定下一场会议的默认会后模型；开始会议时将最终选择快照到单场会议，设置后续变化不影响进行中会议。

本地处理：

- 展示可用 diarization model package（说话人分离模型包）。
- 每个模型包显示：大小、版本、来源、平台、安装状态、建议最长音频、预计内存档位。
- 支持下载、取消下载、失败重试、校验、删除和选择。
- 模型包至少包含 segmentation model（分割模型）和 speaker embedding model（说话人特征模型）；不能只下载其中一个文件后显示为“可用”。

### 2.2 开始会议

- 开始弹窗显示本场有效配置：实时 ASR、区分发言人方式、会后 ASR 模型、预计发言人数。
- 预计发言人数默认“自动”，指定人数使用数字值，后端保存为 `Option<u32>`，不保存“5+”这类 UI 文案。
- 云端会后处理不强制修改实时 ASR；实时 ASR 与会后 ASR 分别快照，避免把 `fun-asr-realtime`、`fun-asr` 和 `paraformer-v2` 混为一个设置。
- 本地说话人处理不改变实时 ASR，也不改变会后 ASR 模型；它只在会后云端 ASR 成功后，对本地受管音频运行 diarization，并将 speaker turns（说话人时间段）对齐到会后原文。
- 缺凭据、缺本地模型或本地模型校验失败时，不开始一个必然失败的任务；提示用户修复、切换方式或关闭区分发言人。

### 2.3 会议详情

- 状态文案至少包括：等待处理、准备音频、上传中、云端识别中、本地分析中、应用结果中、已完成、失败、已取消。
- 后台运行时允许离开会议详情，不要求页面保持打开。
- 失败显示可执行原因，并提供按原配置重试、改选会后模型后重试、沿用实时原文并总结、打开设置。
- 支持按会议重新处理；重新处理创建新 revision，不先删除当前可用原文。
- 支持重命名发言人。重命名操作只修改 `speakerId -> displayName` 映射。

## 3. 明确不做

- 不做 source separation（声源分离），即不生成“发言人 1.wav / 发言人 2.wav”独立音轨。
- 不做 realtime diarization（实时说话人分离）。
- 不接入 `fun-asr-mtl`、Qwen 文件模型或 `fun-asr` / `paraformer-v2` 以外的云端会后模型。
- 不做云端与本地之间的自动 fallback（静默降级）。用户选择的处理方式失败后仍保持该方式，除非用户明确切换后重试。
- 不做 `fun-asr` 到 `paraformer-v2` 的自动 fallback；模型切换必须由用户明确触发并创建新的 processing revision（处理修订版本）。
- 不做跨会议声纹身份识别；`speaker-0` 只在当前会议内稳定，不代表已知联系人。
- 不把预计发言人数当成准确人数承诺。

## 4. 核心数据契约

建议在 Rust / TypeScript 中建立等价结构，字段名以实现时现有序列化风格为准。

```text
PostMeetingAsrSettings
  providerId: bailian
  modelId: fun-asr | paraformer-v2  # default = fun-asr

PostMeetingAsrModelDescriptor
  providerId: string
  modelId: string
  displayName: string
  runtimeKind: cloud
  supportsFileTranscription: bool
  supportsDiarization: bool
  supportsSpeakerCount: bool
  isDefault: bool

DiarizationSettings
  mode: off | cloud | local
  localModelId?: string

MeetingPostProcessingConfig
  diarizationMode: off | cloud | local
  realtimeProviderId: string
  realtimeModelId?: string
  postMeetingAsrModelRef: { providerId: string, modelId: string }
  resolvedAsrRuntimeKind: cloud
  localDiarizationModelId?: string
  expectedSpeakerCount?: u32
  modelVersion?: string
  processingRevision: u32

MeetingPostProcessingState
  status: pending | preparing_audio | uploading | running
          | applying | completed | failed | cancelled
  jobId?: string
  providerTaskId?: string
  progress?: number
  attempt: u32
  errorCode?: string
  errorMessage?: string
  startedAt?: string
  completedAt?: string

SpeakerProfile
  id: string              # speaker-0
  providerSpeakerId?: string
  displayName: string     # 发言人 1 / 张三
  manuallyNamed: bool

SpeakerTurn
  speakerId: string
  startMs: u64
  endMs: u64
  confidence?: f32
  overlapping?: bool

TranscriptRevision
  revision: u32
  source: realtime | cloud_postprocess | local_postprocess | imported
  status: staging | active | rejected
  segments: TranscriptSegment[]
  createdAt: string
```

后端维护 `PostMeetingAsrModelRegistry`（会后 ASR 模型注册表），首期注册 `bailian/fun-asr` 和 `bailian/paraformer-v2`。前端只能提交注册表返回的 `providerId + modelId`；后端创建任务时重新解析 descriptor（描述符）并将最终模型快照到 `MeetingPostProcessingConfig`，不能信任前端自报的 `runtimeKind` 或 capability（能力）。

`TranscriptSegment` 增加可选稳定 `speakerId`，保留现有 `speakerLabel` 兼容旧记录。显示名称优先通过 `SpeakerProfile` 解析；旧记录没有 `speakerId` 时继续显示原 `speakerLabel`。

`MeetingStatus` 继续表达会议录音 / 总结主状态，不扩展出录音状态和后处理状态的笛卡尔积；会后 ASR 和说话人处理使用统一、独立的 `MeetingPostProcessingState`。

## 5. 停止会议与总结顺序

```text
用户点击停止
  -> 停止 recorder
  -> flush 当前 realtime ASR
  -> 关闭并校验 part-*.wav
  -> 保存 realtime transcript revision
  -> 快照本场 post-processing config
  -> 创建持久化 post-processing job + audio processing hold
  -> 清理 active meeting runtime
  -> stop IPC 返回 MeetingRecord

后台任务成功
  -> 使用所选会后 ASR 模型生成 staging transcript revision
  -> diarizationMode=off: 保持“未区分”
  -> diarizationMode=cloud: 读取云端 speaker_id
  -> diarizationMode=local: 执行本地 speaker turns 与会后 ASR 时间戳对齐
  -> 校验时间、顺序、speaker、非空文本
  -> 原子切换 active revision
  -> 更新 SpeakerProfile
  -> 释放 processing hold，并按 retention 决定保留或删除音频
  -> 启动 summary job

后台任务失败
  -> 保留 realtime active revision
  -> 保留音频 processing hold，供重试
  -> 标记 failed 并展示原因
  -> 用户可重试、取消，或明确沿用实时原文直接总结
```

停止 IPC 的成功含义是“录音和实时原文已经安全保存，后处理任务已经持久化”，不是“发言人已经区分完成”。

## 6. 会议音频文件流式上传

### 6.1 目标边界

这里的 streaming file upload（文件流式上传）是指从磁盘按块读取并上传，控制内存峰值；不是把会议结束后的文件重新送入 realtime WebSocket（实时接口）。

当前会议音频为：

```text
meeting-recordings/<meeting-id>/part-0001.wav
meeting-recordings/<meeting-id>/part-0002.wav
...
```

每个 part 都有独立 WAV header（文件头），不能直接把文件字节依次拼接上传。

### 6.2 `MeetingAudioSource`

新增只读音频源抽象，首期仅服务会议和导入音频：

```text
MeetingAudioSource
  SingleWav(path)
  SegmentedWav(parts[])

operations
  probe() -> sampleRate/channels/bits/duration/pcmBytes
  openNormalizedWavStream() -> AsyncRead + exactContentLength
```

`SegmentedWav` 打开时执行：

1. 按 part index 排序并拒绝重复 / 缺失序号。
2. 读取每个 WAV header，确认都是 PCM、16 kHz、单声道、16-bit。
3. 汇总所有 PCM data length，检查 WAV 32-bit data size 边界和总时长限制。
4. 流的前 44 字节只生成一次合并后的 WAV header。
5. 随后依次读取每个 part 的 PCM data 区域，不复制各 part header。
6. 每次只保留固定大小 buffer；取消或读取失败立即终止上传。

### 6.3 百炼上传与任务提交

```text
resolve selected postMeetingAsrModelId
  -> require model in { fun-asr, paraformer-v2 }
  -> request upload policy for selected model
  -> create multipart Part::stream_with_length(audioStream, contentLength)
  -> upload to temporary OSS object
  -> receive oss:// object reference
  -> submit async transcription:
       model = selected postMeetingAsrModelId
       diarization_enabled = (diarizationMode == cloud)
       speaker_count = expectedSpeakerCount when present
  -> persist providerTaskId
  -> poll task with bounded retry
  -> download transcription JSON
  -> parse structured result
```

实现不得沿用当前 `buffer.clone() -> Vec<i16> -> Vec<u8> WAV -> wav.to_vec()` 的会议路径。短口述现有内存实现可以保持不变；只新增 path / stream API，避免无关重构。

上传失败重试必须重新申请 upload policy；云端任务已提交后优先凭 `providerTaskId` 恢复轮询，不重复上传。临时 `oss://` 地址和签名信息不写日志，不返回前端。

## 7. 云端可选模型管线

```text
会议中
  mic -> recorder parts
  mic -> selected realtime ASR -> draft/final -> realtime revision

会议后
  MeetingAudioSource
  -> streaming temporary upload
  -> selected cloud file ASR
       default: fun-asr
       alternative: paraformer-v2
       diarization_enabled = (diarizationMode == cloud)
  -> transcripts[].sentences[]
  -> begin_time/end_time/text/speaker_id
  -> SpeakerProfile + TranscriptSegment[]
  -> cloud_postprocess revision
  -> summary
```

`fun-asr` 和 `paraformer-v2` 复用统一的 `BailianMeetingFileAsrAdapter`（百炼会议文件识别适配器）：

```text
submit(request: MeetingFileAsrRequest) -> providerTaskId
poll(providerTaskId) -> pending | succeeded(resultUrl) | failed(error)
parse(modelId, providerPayload) -> NormalizedMeetingTranscript

MeetingFileAsrRequest
  modelRef: { providerId, modelId }
  audioObjectUrl: secret reference
  diarizationEnabled: bool  # true only when diarizationMode=cloud
  expectedSpeakerCount?: u32
```

公共层负责上传、任务持久化、轮询、取消语义和标准化结果；模型 adapter（适配器）只负责请求字段与响应字段差异。不得复制两套完整 job runner（任务执行器）。

解析规则：

- `begin_time` / `end_time` 必须是非负、单调可排序的毫秒值。
- `speaker_id` 转为稳定会议内 ID，例如 provider `0` 映射为 `speaker-0`。
- 初始显示名按首次出现顺序生成“发言人 1、发言人 2……”，不假设 provider ID 连续。
- 空句丢弃；结束早于开始、时间溢出或结果完全无有效句子时拒绝整个 staging revision。
- 句子排序后允许轻微时间重叠；重叠元数据保留，不复制或吞掉文本。
- 预计人数只做结果校验。差异明显时任务仍可完成，但 UI 显示“检测到的人数与预计不同”。

## 8. 本地模型完整管线

### 8.1 模型职责

本地说话人处理不是单个 ASR 模型的开关，至少包含：

```text
audio waveform
  -> segmentation / VAD: 哪些时间段有人说话
  -> speaker embedding: 每段声音的说话人特征
  -> clustering: 将相似特征归为 speaker-0/1/2
  -> speaker turns: 谁在什么时间说话
  -> transcript alignment: 将 speaker turns 合并回文字片段
```

sherpa-onnx 1.13.4 已提供 `OfflineSpeakerDiarization`，配置包含 pyannote segmentation、speaker embedding 和 clustering。`expectedSpeakerCount` 非空时映射到 `num_clusters`；自动模式使用 `-1` 和模型包声明的 threshold（阈值）。

### 8.2 现场会议本地管线

```text
part-*.wav -> normalized waveform
  -> OfflineSpeakerDiarization.process
  -> SpeakerTurn[]

post-meeting ASR TranscriptSegment[] + token/sentence timestamp
  -> alignment
  -> local_postprocess revision
  -> summary
```

对齐规则：

- 有 token / word / char timestamp 时，按 token 与 speaker turn 的最大时间重叠分配，并按连续 speaker 分组生成新 segment。
- 只有 sentence timestamp 时，按重叠时长最多的 speaker 分配；最大重叠比例低于内部验收阈值时标记为“未确认”。
- 同一句跨越多个显著 speaker turn 且没有 token timestamp 时，不猜测拆字，不复制文本；保留整句并标记“未确认”。
- diarization 没覆盖到的静音 / 噪声区不创建空 speaker segment。
- 模型返回重叠说话时保留 `overlapping=true`；首版 UI 仍显示一个主 speaker，并标记低置信度，不伪造两份相同文本。

### 8.3 本地资源与长会议限制

`OfflineSpeakerDiarization::process(&[f32])` 需要完整 waveform（波形）。因此本地实现必须：

- 从文件按块解码到预分配 waveform，禁止同时保留 WAV、PCM bytes、`Vec<i16>` 和 `Vec<f32>` 多份完整副本。
- 在模型 catalog（目录）中保存经真实机器验证的 `maxRecommendedDurationMs` 和内存档位。
- 开始处理前根据音频时长、模型声明和可用内存做 preflight（预检）。超过已验证上限时阻止执行，并提示切换云端；不能自动上传。
- 第一版本地模型在完成 30 / 60 / 120 分钟基准测试前状态只能是 experimental（实验性），不能标记为稳定默认。
- 对长会议做分块 diarization 不是简单切片：跨块 speaker identity merge（身份合并）需要单独算法和测试，首版不在没有证据时宣称支持。

### 8.4 本地模型生命周期

```text
missing -> downloading -> verifying -> ready
                    -> failed -> retry
ready -> loading -> running -> idle/unloaded
ready -> deleting -> missing
```

- 下载写 `.partial`，完成后校验文件清单、大小和 checksum（校验和），再原子改名。
- 运行中模型不能删除；删除按钮显示占用原因。
- 下载失败不影响云端或其他本地 ASR。
- 应用升级不删除模型目录；模型 manifest（清单）版本不兼容时标记 invalid，不静默重新下载。

## 9. 音频生命周期

新增 `processingHold` 概念，与用户设置的最近 N 场音频保留数量分离：

- 创建云端 / 本地后处理任务时持有音频。
- pending / running / failed-retryable 时不能被 retention prune（保留清理）删除。
- completed 后释放占用，再按当前单场快照的保留决策处理。
- cancelled 或“沿用实时原文并总结”后释放占用。
- 删除会议时先取消本地任务；已提交的云端任务无法保证远端立即取消时，本地停止轮询并删除本地记录，远端临时对象按服务生命周期失效。
- 用户设置保留数量为 0 时，仍保留到任务终态；之后删除音频但保留文字和总结。

## 10. 任务恢复与并发

- job、配置快照、attempt、providerTaskId 和状态必须持久化，不能只保存在 `Inner` 内存。
- 应用启动时扫描非终态任务：
  - `pending/preparing_audio/uploading`：从本地音频源重新开始，旧 upload policy 作废。
  - `running` 且有 `providerTaskId`：恢复轮询。
  - `applying`：检查 staging revision；完整则幂等提交，不完整则删除 staging 并重试。
- 同一 meeting 同时只允许一个 active post-processing job；点击重试增加 attempt，不并行启动第二个任务。
- 首期全局最多运行一个本地 diarization job，避免模型和 waveform 内存叠加。
- 云端任务并发上限先设为可配置内部常量，UI 不暴露；超过上限排队而不是报错丢任务。

## 11. IPC 与事件建议

```text
start_meeting_recording(options)
list_post_meeting_asr_models() -> PostMeetingAsrModelDescriptor[]
retry_meeting_post_processing(id, options?)
cancel_meeting_post_processing(id)
use_realtime_transcript_and_summarize(id)
rename_meeting_speaker(meetingId, speakerId, displayName)

meeting:post-processing-state
meeting:record-updated
meeting:error
```

`startMeetingRecording()` 当前无参数，V2-2 增加 options，至少携带本场预计人数和可选的 `postMeetingAsrModelRef`。后端仍负责根据 preferences 和模型注册表解析有效模式及模型，不能信任前端传任意 provider/model。`retry_meeting_post_processing` 允许用户明确改选 `fun-asr` 或 `paraformer-v2` 后重试；改选必须增加 attempt 和 processing revision。

## 12. 实施顺序

1. 数据契约：settings、meeting snapshot、state、speaker、revision、job persistence。
2. 状态机：停止快速返回、processing hold、恢复、重试、取消、跳过后总结。
3. `MeetingAudioSource`：单 WAV / 分片 WAV probe、合并 header、文件流、取消和内存测试。
4. 百炼：会后模型注册表、`fun-asr` 默认值、`paraformer-v2` 备选、统一 adapter、`diarization_enabled`、结构化结果 parser、任务恢复。
5. UI：设置三态、会后模型下拉、开始人数、详情状态、改选模型重试 / 跳过 / 重命名。
6. 本地模型：catalog、下载 / 校验 / 删除、runtime、alignment。
7. 总结、导出、删除和音频 retention 回归。

## 13. 验证计划

自动测试：

- 配置序列化、旧会议兼容、开始会议快照不受设置后续变化影响。
- 默认配置解析为 `bailian/fun-asr`；下拉只包含 `fun-asr` 与 `paraformer-v2`，非法或已下线模型被后端拒绝。
- 分别验证 `fun-asr`、`paraformer-v2` 的请求字段、任务提交、轮询和标准化 parser；两者都覆盖开启说话人分离与预计人数。
- `fun-asr` 失败时不会自动提交 `paraformer-v2`；用户改选模型重试后产生新 attempt 和 revision。
- 分片 WAV 合并流只有一个 header，PCM 顺序正确，内存不随文件大小线性复制多份。
- 上传取消、上传失败重试、任务提交、轮询恢复、结果字段缺失和时间戳异常。
- 云端 parser 对 `speaker_id/begin_time/end_time/text` 生成稳定 segment。
- revision 原子提交；失败保持旧 active revision。
- retention=0 时 processing hold 仍保留音频，终态后才清理。
- 本地 speaker turn 对齐：单 speaker、多 speaker、跨句、缺 timestamp、重叠说话。
- 同一会议重复重试不产生两个 active job。

真实验证：

- 1、2、4、8 人会议，包含相似声线、短句、抢话、长静音。
- 暂停 / 继续产生多个 part，云端上传后时间连续。
- 30、60、120 分钟会议的上传内存、云端耗时、本地内存和处理耗时。
- 上传中断网、任务运行中退出应用、应用重启恢复。
- 本地缺模型、下载失败、校验失败、运行时内存预检失败。
- 后处理失败后重试，以及跳过区分直接总结。
- 人工重命名后重新打开、导出和总结引用名称一致。

质量指标至少记录 DER（Diarization Error Rate，说话人分离错误率）、人数差异、字错率、处理时间、峰值内存和失败率。没有真实数据前不承诺“准确识别每位发言人”。

## 14. 成功标准

- 云端会后默认使用 `fun-asr`，用户可下拉选择 `paraformer-v2`；除这两个模型外不能选择或意外路由到其他模型。
- 任一云端模型失败后不得自动切换另一个模型，只有用户明确改选并重试才能改变任务模型。
- 两小时级云端测试不通过完整 PCM/WAV 内存复制路径。
- 本地说话人处理有清晰的模型下载、就绪、运行、失败和删除状态，并明确会后 ASR 仍会上传完整音频。
- 停止会议快速返回，后处理状态可跨页面和应用重启恢复。
- 后处理成功后总结读取整理后原文；失败时旧原文和旧总结不丢失。
- retention=0、删除会议、取消任务和应用退出均不会留下不可解释的音频或悬空任务。
