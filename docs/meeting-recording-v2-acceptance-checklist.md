# 会议录音 V2 Acceptance Checklist

状态：active
日期：2026-08-12
范围权威：`docs/meeting-recording-v2-plan.md`

## 状态定义

- `done`：已实现并有自动或人工验证证据。
- `partial`：部分实现，仍缺字段、UI、错误处理或真实验证。
- `missing`：未实现。
- `deferred`：用户明确确认移出 V2。
- `blocked`：存在已记录的外部或平台阻塞。

Phase 0 已按当前基线核查代码与自动测试。除 V2-1 现有代码证据外，不因“计划已写”把功能标记为 `done`；后续执行阶段必须逐项更新证据和验证结果。

### Phase 0 基线证据（2026-08-12）

- 基线：`8e815a58`，分支 `codex/meeting-post-asr-routing-20260812`。
- 前端：`tsc --noEmit`、`npm run build` 通过。
- Rust：`cargo test meeting`（101 passed）、`cargo test bailian`（39 passed）、`cargo test preferences`（13 passed）、`cargo test credentials`（45 passed）。
- 已确认 V2-1 存在 `MeetingAsrSettings`、provider capabilities、`fun-asr-realtime` draft/final、`TranscriptSegment.metadata`、provider session guard、`audioPartIndex/sessionStartMs` 连续有效音频时间轴和网络中断保留录音路径。
- 已确认缺口：运行中的 `MeetingSession` 锁定了 provider、model override 和 silence preset，但 `MeetingRecord` 尚未持久化本场实际生效的 realtime ASR 配置；应用重启后不能审计本场实际 provider/model/silence preset。
- 真实百炼会议、暂停/继续、网络中断、短口述回归和 Tauri UI 人工验证尚未完成，因此 MR-V2-001～003 保持 `partial`。

### Phase 1 实现与验证证据（2026-08-12）

- 新增可选 `MeetingRealtimeAsrSnapshot`，将本场配置来源 provider、实际协议 provider、实际归一化模型和 silence preset 持久化到 `MeetingRecord.realtimeAsr`；旧会议 JSON 缺少该字段时继续按 `None` 读取。
- 会议 ASR 构建入口返回实际 `AsrCallLabel`；首次构建成功后原子保存快照，持久化失败会恢复内存状态并中止本次启动。
- pause / resume 继续使用首次锁定的实际模型；设置页 capability 查询使用 effective provider，兼容统一百炼 provider 的实际协议路由。
- browser mock 和 TypeScript 类型已同步 `realtimeAsr`。
- 自动验证：`git diff --check`、`tsc --noEmit`、`npm run build`、MSVC `cargo check` 通过；`cargo test meeting`（104 passed）、`cargo test bailian`（39 passed）、`cargo test preferences`（13 passed）、`cargo test credentials`（45 passed）。
- 真实百炼、Tauri UI、暂停 / 继续、网络中断和短口述人工回归仍未完成，因此 MR-V2-001～003 继续为 `partial`，不能仅凭自动测试标记为 `done`。

### Phase 2 数据契约与任务框架证据（2026-08-12）

- 新增会后 ASR 后端权威注册表，当前只返回 `bailian/fun-asr`（默认）和 `bailian/paraformer-v2`（备选）；前端只提交 `providerId + modelId`，`runtimeKind` 由后端解析。
- 设置页增加独立会后 ASR 下拉及关闭、云端处理、本地处理三态；开始会议弹窗展示实时 ASR、会后 ASR、说话人处理和自动 / 1～20 人预计人数，并将单场配置固化到 `MeetingPostProcessingConfig`。
- 新增持久化 `MeetingPostProcessingState`、`TranscriptRevision`、`SpeakerProfile`、`SpeakerTurn` 和 `ProcessingHold`；停止会议先固化实时原文和任务，再快速返回并在后台启动 job。
- 后处理支持失败、重试、取消、沿用实时原文并总结、发言人重命名和应用启动恢复扫描；状态变更在共享 `MeetingStore` 锁内按当前 `jobId + status` 原子提交，旧任务、重复重试或迟到 worker 不能覆盖新状态。
- 删除会议在同一个 `MeetingStore` 锁内完成活跃状态校验、后处理失效、受管音频删除和记录删除；音频清理失败时磁盘记录保持原样。全局 retention prune 会跳过 processing hold。
- Phase 2 截止时，后台 job 仍明确失败为 `postMeetingAsrAdapterUnavailable`，不伪造云端成功，不自动生成总结，也不自动切换模型；该历史缺口已在下方 Phase 3 证据中更新。本地说话人模型管线仍未实现。
- 自动验证：`cargo test meeting`（122 passed）、`cargo test preferences`（13 passed）、`cargo test credentials`（45 passed）、`cargo test bailian`（39 passed）、MSVC `cargo check`、`tsc --noEmit`、`npm run build`、`git diff --check` 通过。`credentials` 首次串行命令中有一个 localhost 重定向测试受进程内全局代理开关并行状态影响得到 502，单项和完整 45 项复跑均通过；未修改网络模块。真实百炼、Tauri UI、stop 延迟、重启、取消 / 重试和本地模型人工验证尚未完成。

### Phase 3 云端会后 ASR 进行中证据（2026-08-12）

- 新增 `MeetingAudioSource`，支持单个 WAV 和按 1-based 序号连续排列的分片 WAV；逐个校验 16 kHz、单声道、16-bit PCM 标准 WAV，合并流只输出一个 WAV header，并提前计算确定的 content length。
- 音频流按 64 KiB 上限读取 PCM，不构造完整会议 WAV / PCM 副本；运行时取消标志会以 `Interrupted` 终止流。
- 百炼云端会后 ASR 已接通统一的临时 OSS 上传、异步任务提交、轮询和结果下载框架；`fun-asr` 与 `paraformer-v2` 分别通过模型入口解析 sentence time、text、sentenceId 和 speakerId，不自动切换模型。
- 云端说话人处理会发送 `diarization_enabled=true`；预计人数为 2～100 时发送 `speaker_count`，用户选择 1 人时不向百炼发送非法人数提示。关闭说话人处理时发送 `diarization_enabled=false`，并忽略结果中的 provider speakerId。
- 结果先转换为 staging `TranscriptRevision`，经 jobId、status、providerTaskId、modelRef 和时间戳校验后原子激活；云端 speakerId 映射为会议内稳定的 `SpeakerProfile` / `SpeakerTurn`，完成后才触发自动总结并释放 processing hold。
- 重启恢复时，已有 `providerTaskId` 的任务只恢复轮询；`running` / `local_analyzing` / `applying` 但缺少 taskId 时明确失败为 `postMeetingAsrSubmissionOutcomeUnknown`，避免盲目重复提交和重复计费。同一 jobId 只允许一个进程内 worker。
- 取消会先中断本地上传 / 轮询并阻止迟到结果写回；若已持久化 `providerTaskId`，后台尽力调用百炼异步任务取消接口。该接口只接受仍处于 `PENDING` 的任务，已进入 `RUNNING` 的任务不能远端取消，因此远端失败不会回滚本地取消状态。
- 自动验证：`cargo test meeting_audio_source`（6 passed）、`cargo test dashscope_multimodal`（26 passed）、`cargo test meeting_post_processing`（29 passed）、`cargo test meeting`（143 passed）、`cargo test bailian`（39 passed）、`cargo test preferences`（13 passed）、`cargo test credentials`（45 passed）及 MSVC `cargo check` 通过。multipart 集成测试确认两段 WAV 上传体只有一个 RIFF header，且 PCM 顺序完整。
- 真实百炼凭据下的 `fun-asr` / `paraformer-v2` 客户端级短链路已于 2026-08-13 通过；完整 MeetingStore job、上传中断网、应用真实重启、长会议峰值内存和 30/60/120 分钟验证尚未完成，因此相关条目保持 `partial`。

### Phase 4 本地说话人处理进行中证据（2026-08-12）

- 新增独立本地说话人模型 catalog，不混入 ASR 模型下拉；首个固定组合包为 `sherpa-pyannote-3dspeaker-zh-v1`，包含 Pyannote segmentation 与中文 3D-Speaker embedding，两项上游资产、解包后模型和 manifest 均使用固定大小与 SHA-256 校验。
- 复用现有 `SherpaDownloadManager` 和分块下载器，组合包写入同级 `.partial` 目录，全部资产校验通过后才原子激活；支持下载、取消、重试、invalid 状态、删除以及会议录音中 / 活跃后处理任务的占用保护。
- catalog 明确来源、Windows x86_64 平台、实验状态、采样率和 clustering threshold；`maxRecommendedDurationMs` 与内存档位在真实 30 / 60 / 120 分钟测试前保持“待验证”，不填虚假能力值。
- 设置页提供模型选择、进度、取消、重试和删除；浏览器 mock 可完成状态流转。开始会议及重试由后端要求模型真实 `ready`，开始弹窗也读取同一后端 readiness 后才允许开始。
- 新增 sherpa-onnx `OfflineSpeakerDiarization` 本地运行时，按本场预计人数设置 `num_clusters`，自动模式使用 `-1`；运行 segmentation、embedding 和 clustering 后生成会议内稳定的 `speaker-N`、`SpeakerTurn` 及 overlap 标记。全局同时只允许一个本地说话人任务，模型运行中禁止删除。
- 本地模式在云端会后 ASR 成功后进入持久化 `local_analyzing` 状态，再按句子时间戳和 `SpeakerTurn` 最大重叠分配主发言人；主覆盖低于 50%、第二位发言人覆盖达到 20%、缺失时间戳或存在重叠说话时标记“待确认”，不拆字、不复制文本。结果使用 `LocalPostprocess` staging revision，经当前 job / model / provider task / local model 校验后才原子激活；取消或旧任务的迟到结果不能写回。
- 会议详情显示“待确认”“重叠说话”，预计人数与检测人数不一致时保留完成结果并提示人工确认。`local_analyzing` 已纳入取消、占用保护和启动恢复；若该状态缺少 `providerTaskId`，明确拒绝自动重提云端任务，避免重复上传与计费。
- 长会议在读取 waveform（波形）前检查样本数、`Vec<f32>` 分配大小和 Windows 可用物理内存，并预留 512 MiB 运行时空间；分片 PCM 按块读取到单个预分配 waveform，不同时保留完整 WAV、PCM 和 `Vec<i16>` 副本。该预检还没有真实模型峰值数据，因此不能替代时长上限验证。
- 自动验证：`cargo test speaker_diarization`（8 passed）、`cargo test speaker_diarization_runtime`（3 passed）、`cargo test meeting_audio_source`（6 passed）、`cargo test meeting_post_processing`（29 passed）、`cargo test meeting`（143 passed）、`cargo test preferences`（13 passed）、MSVC `cargo check`、`tsc --noEmit`、`npm run build` 和 `git diff --check` 通过。后续补充的下载核心回环测试确认：HTTP Range 截断响应和错误 `Content-Range` 不会被误记为完整 chunk，并发 chunk 失败会回滚临时进度，已有 `.partial.idx` 时只请求缺失 chunk；说话人下载取消使用独立命名空间，不会误取消同名 ASR 下载。该阶段结束时真实本地模型推理尚未完成，后续已在 Phase 6 补充合成短音频真实链路；真实 Tauri 下载取消 / 占用删除、30 / 60 / 120 分钟及一小时会议验证仍未完成。

### Phase 5 音频文件导入进行中证据（2026-08-13）

- 会议页新增单文件“导入音频”入口和原生 WAV 选择器，展示文件信息、ASR 模型、说话人处理方式、本地说话人模型、预计人数和总结开关；前端没有固定“云端 / 本地识别位置”字段，只提交 `providerId + modelId`。
- 后端以一次性、10 分钟过期的 selection token 保存可信文件选择；token 和持久化记录不暴露源文件绝对路径。标准 PCM WAV 会严格校验 header、编码、声道、采样率和位深，并按块下混 / 重采样到 16 kHz 单声道 16-bit PCM。
- 规范化先写入独立 `.partial`，完成后原子生成受管 `part-0001.wav`；成功、失败、取消、重选和删除均不修改用户源文件。取消时无有效受管 WAV 的 Draft 会删除，有完整受管 WAV 时保留 cancelled 记录并立即执行 retention。
- 文件 ASR 注册表只暴露 `bailian/fun-asr`、`bailian/paraformer-v2` 和 ready 的 sherpa-onnx Offline 本地模型；后端从注册表解析 `runtimeKind`。云端模型复用百炼流式上传和异步任务框架，本地模型进入 `MeetingBatchTranscriber`；本地 ASR + 云端说话人处理在上传前明确拒绝。
- 本地关闭说话人处理时按不超过 30 秒的有界绝对时间窗执行 batch ASR；本地说话人处理采用 diarization-first，合并同 speaker 短间隔 turn，长窗口优先在目标切点前 5 秒内寻找低能量静音帧，找不到时回退到 30 秒硬切，最终保存绝对时间戳、稳定 speakerId 和 overlap 待确认标记。
- 导入复用 `MeetingRecord`、`MeetingAudioSource`、post-processing job、staging revision、总结、播放、Markdown 导出、删除和 retention。应用重启会清理 importing `.partial` 并要求重选；有 providerTaskId 的云端任务复用 V2-2 恢复轮询；总结中断会只开放总结重试，不重复 ASR。
- 删除会议采用两阶段停止：先标记并等待导入 / 后处理 worker 释放文件句柄，再删除受管音频和记录；迟到 worker 不能写回已删除或已替换任务。
- 自动验证：`cargo test meeting_audio_import --lib`（23 passed）、`cargo test meeting_post_processing --lib`（34 passed，4 ignored）、`cargo test meeting_summary --lib`（19 passed）、`cargo test meeting_markdown --lib`（3 passed）、`cargo test meeting --lib`（172 passed）、`cargo test dashscope_multimodal --lib`（29 passed）、`cargo test bailian --lib`（39 passed）、`cargo test preferences --lib`（13 passed）、`cargo test credentials --lib`（46 passed）、MSVC `cargo check`、`tsc --noEmit`、`npm run build` 和 `git diff --check` 通过。后续已补充真实百炼双模型、本地 ASR、本地说话人短链路，以及两条本地路径的 30 / 60 / 120 分钟合成音频资源基准；Tauri UI、磁盘不足、真实应用重启、云端长音频和真实会议质量仍未验证，因此 MR-V2-201～212 仍保持 `partial`。

### Phase 6 有限桌面验证与补充回归证据（2026-08-13）

- 源码版 Tauri dev 已成功构建并启动。会议页可见“导入音频”入口；导入弹窗默认 `Fun-ASR · 云端`，未选择文件时开始按钮禁用，且没有固定“识别位置”字段。
- 导入 ASR 下拉中云端仅有 `Fun-ASR` 与 `Paraformer V2`，同时按 readiness 展示可用或禁用的本地会议文件 ASR；说话人处理下拉包含关闭、云端处理、本地处理三态。
- 云端 / 本地说话人处理会条件展示预计发言人数。本地处理缺少 ready 模型时阻止开始，并明确提示“完整音频会上送云端 ASR；说话人分离与对齐在本机执行”，没有误称全本地。
- 设置 -> 服务已确认会议实时 ASR 与会后 ASR 是独立区域：会议实时模型显示 `fun-asr-realtime`，会后 ASR 默认显示 `Fun-ASR（默认）`，说话人处理默认关闭。后续模型下拉、开始会议、任务状态和真实识别人工验证因用户需要使用电脑而暂停，不得据此标记为 `done`。
- 补充自动回归：会后配置解析后持有本场独立值，后续修改全局偏好不会改写该 config；文件 selection token 只存在进程内 registry，新 registry（等价于应用重启）不能继续消费旧 token，且源文件保持存在；改选模型重试会同步创建新 job、递增 attempt / processing revision、拒绝旧 staging revision，并保持实时原文为 active revision。
- 原子提交故障注入：规范化已写完但最终路径被占用时，最终 rename 明确失败，`.partial` 被清理，源文件保持不变，占用文件不会被覆盖或误认成有效受管 WAV。该测试不等同于真实磁盘不足验证。
- 路由安全回归：worker 在选择云端 adapter 或本地 engine 前重新解析后端模型注册表；正常云端快照通过，伪造 state `runtimeKind`、伪造 import config `runtimeKind`、模型引用不一致均返回 `meetingAsrRouteMismatch`。
- 导入重试回归：已有受管音频时从 `fun-asr` 改选 `paraformer-v2`，同一原子转换同步更新 import config、post-processing config / state、attempt、processing revision 和 processing hold，拒绝旧 staging revision；同一旧任务不能重复转换。
- 云端上传故障注入：本机回环服务在接收完整会议 multipart 上传后关闭连接且不返回成功响应，客户端明确返回 `DashScope temporary upload` 错误，只发起一次非幂等上传，源分片保持不变。该测试不替代真实公网中断验证。
- 云端任务故障注入：轮询 GET 连续返回 503 时按有界策略共请求 4 次后明确失败；异步提交 POST 在服务端接收完整请求后断开连接时只发送一次，不自动重提未知结果的任务。两项均只使用本机回环服务，不替代真实公网断线验证。
- revision 持久化故障注入：Windows 下将 `meetings.json` 设为只读，强制原子 rename 失败；重新从磁盘读取后旧 active revision、实时原文和旧总结均保持不变，且没有残留临时文件。该测试不替代真实应用重启或磁盘耗尽验证。
- 真实本地 ASR 导入持久化闭环：使用 Windows 离线中文 TTS 生成无隐私 PCM WAV，经一次性 selection token 和隔离 `MeetingStore` 创建导入记录，后端从 `providerId + modelId` 解析并持久化 `runtimeKind=local`，再完成 `.partial` 规范化、受管 WAV 原子提交和 post-processing 状态转换。生产本地 worker 使用本机已安装的 SenseVoice sherpa-onnx 模型按绝对有界时间窗生成 `RetranscribedAsr` segment，原子激活 `Imported` revision 并释放 processing hold；重新打开 `meetings.json` 后仍为 completed，且没有 `providerTaskId` 或云端上传。测试同时确认 token 不可复用、源文件 SHA-256 与 mtime 不变。集成测试默认 `ignore`，仅在显式提供 `OPENLESS_MEETING_ASR_TEST_WAV` 时运行；该证据不替代原生选择器、真实应用重启、真实会议或长音频测试。
- 真实本地双声线导入持久化闭环：使用 Windows 系统离线中文男声与女声交替生成无隐私 PCM WAV，经一次性 selection token 和隔离 `MeetingStore` 创建 `本地 ASR + 本地说话人处理` 导入记录，后端持久化 `runtimeKind=local`、本地模型快照和预计 2 人配置。规范化及状态转换后，生产本地 worker 运行 Pyannote segmentation、3D-Speaker embedding、2 人 clustering、speaker windows 和 SenseVoice batch ASR，持久化两个稳定 `SpeakerProfile`、`SpeakerTurn` 和带 speakerId / 绝对时间戳的 segment，原子激活 `Imported` revision 并释放 processing hold；重新打开 `meetings.json` 后仍为 completed。测试同时确认没有 `providerTaskId` 或云端上传，token 不可复用，源文件 SHA-256 与 mtime 不变。集成测试默认 `ignore`，仅在显式提供 `OPENLESS_MEETING_DIARIZATION_TEST_WAV` 时运行；该证据只覆盖合成短音频，不替代真实人声、抢话、噪声、多人或长会议测试。
- 真实百炼双模型完整 worker 短链路：按 2026-08-13 阿里云百炼官方录音文件识别协议复核异步任务、临时 OSS、`diarization_enabled` 和 `speaker_count` 字段后，使用本机已配置的 `bailian` 凭据和无隐私双声线短音频，为 `fun-asr`、`paraformer-v2` 分别在隔离临时 `MeetingStore` 中创建 `Pending` job。生产 worker 完成 WAV 校验、流式上传、异步提交、轮询、结构化结果解析、进度持久化和 revision 原子激活；重新打开 `meetings.json` 后，两个任务均为 `completed`，processing hold 已释放，并持久化同模型、同 task 的 segment metadata、speaker profiles 和 speaker turns。集成测试默认 `ignore`，仅在显式提供 `OPENLESS_MEETING_CLOUD_ASR_TEST_WAV` 时运行，不读写用户会议数据库，也不输出 API Key、签名 URL 或 taskId。`speaker_count=2` 仅是模型提示，本次合成短音频没有稳定证明两模型都聚成两个 speaker，因此真实人声质量仍未验收。
- 真实百炼双模型关闭说话人完整 worker 短链路：`fun-asr`、`paraformer-v2` 分别以 `diarizationMode=off` 从隔离 `Pending` job 运行，生产 worker 显式发送 `diarization_enabled=false`，并重新读盘确认整理后原文保留时间戳和同模型 provider metadata，同时不生成 `speakerId`、`SpeakerProfile` 或 `SpeakerTurn`，processing hold 已释放。集成测试默认 `ignore`，仅在显式提供 `OPENLESS_MEETING_CLOUD_ASR_TEST_WAV` 时运行；该证据不替代真实会议和 UI 验收。
- 真实百炼双模型加本地说话人处理完整 worker 短链路：使用同类无隐私双声线 PCM WAV 和本机已安装的 `sherpa-pyannote-3dspeaker-zh-v1`，为 `fun-asr`、`paraformer-v2` 分别在隔离临时 `MeetingStore` 中创建 `diarizationMode=local` 的 `Pending` job。生产 worker 先以 `diarization_enabled=false` 获取所选模型的文本与时间戳，再在本机执行 Pyannote segmentation、3D-Speaker embedding 和预计 2 人 clustering，生成两个稳定 `SpeakerTurn`，将至少一个云端句子对齐到本地 speaker，原子激活 `LocalPostprocess` revision 并重新读盘确认 completed 状态、同模型 provider metadata 和 processing hold 释放。集成测试默认 `ignore`，仅在显式提供 `OPENLESS_MEETING_DIARIZATION_TEST_WAV` 时运行；该证据不等同于真实人声、长会议或 UI 验收。
- 真实百炼持久化任务重启等价恢复：使用隔离 `MeetingStore` 先真实上传并提交 `fun-asr` 任务，将 `providerTaskId`、`Running` 状态和 processing hold 写入 `meetings.json`，随后丢弃原 store 并从磁盘重新打开。生产启动扫描规则只选中该非终态 job，worker 从已有 taskId 继续真实轮询并完成 revision 原子激活；恢复阶段的音频 resolver 被强制设为报错且实际调用次数为 0，证明不会重复读取音频、上传或提交。集成测试默认 `ignore`，仅在显式提供 `OPENLESS_MEETING_CLOUD_ASR_TEST_WAV` 时运行；该证据覆盖持久化边界与恢复执行，但不等同于真正退出并重启 Tauri 进程。
- 真实百炼双模型音频导入持久化闭环：`fun-asr`、`paraformer-v2` 分别使用无隐私中文 PCM WAV，经一次性 selection token 和独立隔离 `MeetingStore` 创建导入记录，先写 `.partial`，再规范化并原子生成受管 16 kHz 单声道 WAV。后端从 `providerId + modelId` 解析并持久化 `runtimeKind=cloud`，每个模型分别进入生产云端 worker，完成真实上传、提交、轮询、`Imported` revision 激活和 processing hold 释放；重新打开各自 `meetings.json` 后结果仍为 completed，segment metadata 与所选模型及同一 provider task 一致。测试同时确认 token 不可复用，用户源文件 SHA-256 与 mtime 不变，且不读写用户会议数据库。集成测试默认 `ignore`，仅在显式提供 `OPENLESS_MEETING_CLOUD_ASR_TEST_WAV` 时运行；原生文件选择器、前端进度和 Tauri UI 仍未人工验证。
- 真实百炼显式改选模型重试闭环：使用无隐私中文 PCM WAV 和隔离 `MeetingStore`，先持久化 attempt 1 的 `fun-asr` 失败终态，确认 modelRef、jobId 和 processing hold 均仍指向 `fun-asr`，没有自动 fallback（回退切换）。随后经生产重试转换显式改选 `paraformer-v2`，生成新 jobId、attempt 2 和 processing revision 2，拒绝旧 staging revision；生产云端 worker 真实完成上传、提交、轮询和 `Imported` revision 2 原子激活。重新打开 `meetings.json` 后仍为 completed，provider metadata 指向 `bailian/paraformer-v2` 及新 taskId，processing hold 已释放，源文件 SHA-256 和 mtime 不变。集成测试 `configured_bailian_explicit_retry_switches_model_and_revision` 默认 `ignore`，仅在显式提供 `OPENLESS_MEETING_CLOUD_ASR_TEST_WAV` 时运行；测试中的 `fun-asr` 失败为隔离库内明确构造的终态，不是真实计费失败或公网故障注入。
- 本地 ASR 静音感知有界分块：关闭说话人处理时仍以 30 秒为硬上限，但每个边界会在前 5 秒内按 20 ms 帧寻找低能量静音点，找不到才回退到硬切；该策略复用本地说话人时间窗的已有静音检测，不引入新 VAD 模型或下载依赖。确定性测试确认优先使用附近静音切点、保持绝对时间轴且窗口不超过 30 秒；另使用无隐私长中文合成音频真实运行 SenseVoice 生产导入 worker，完成多时间窗识别、绝对时间戳、`Imported` revision 激活和重开读盘，无 providerTaskId 或云端上传。集成测试 `installed_local_asr_import_persists_multiple_bounded_windows` 默认 `ignore`；后续已使用同一测试入口完成 30 / 60 / 120 分钟本地 ASR 资源基准，但仍不替代真实会议质量验收。
- 本地 ASR 30 / 60 / 120 分钟资源基准（2026-08-13）：在 Windows 11 build 26200、Intel Core i5-14400（10 核 16 线程）、15.6 GB 物理内存上，用 `sense-voice-small-zh` 和无隐私重复中文合成语音运行默认 ignored 的生产导入闭环。三组均完成规范化、静音感知多窗口 ASR、revision 原子激活、重开 `MeetingStore`、源文件 hash / mtime 校验和运行时释放。30 分钟：60.14 秒、峰值工作集 509.9 MiB；60 分钟：119.51 秒、565.5 MiB；120 分钟：242.56 秒、673.5 MiB；实时系数约 0.033～0.034（约 29.7～30 倍速）。进程以 debug 测试二进制、隐藏窗口和 BelowNormal（低于正常）优先级运行；峰值内存包含测试 fixture 为校验源文件不变而额外读取音频的开销，因此是偏保守的端到端进程观测值，不是纯模型 RSS。该基准证明本机“本地 ASR + 关闭说话人处理”可完成 120 分钟输入，不代表真实会议识别质量，也不覆盖云端上传 / 处理；本地说话人长音频证据见下一项。
- 本地说话人处理 30 / 60 / 120 分钟资源基准（2026-08-13）：在同一台 Windows 11 / i5-14400 / 15.6 GB 机器上，使用系统离线中文男声 `Microsoft Kangkang` 与女声 `Microsoft Huihui Desktop` 交替生成无隐私 PCM WAV，预计人数设为 2，以 `sherpa-pyannote-3dspeaker-zh-v1 + sense-voice-small-zh` 运行默认 ignored 的生产 diarization-first 导入闭环。三组均完成 segmentation、embedding、clustering、两个 speaker、speaker windows、SenseVoice、带 `speakerId` 原文、revision 激活、重开 `MeetingStore`、源文件 hash / mtime 校验和无云端上传。30 分钟：391.20 秒、CPU 837.69 秒、峰值工作集 566.2 MiB；60 分钟：775.06 秒、CPU 1667.41 秒、804.1 MiB；120 分钟：1544.22 秒、CPU 3321.81 秒、1454.1 MiB。基于该结果，catalog 声明建议最长 120 分钟、峰值约 1.5 GiB、建议至少 4 GiB 可用内存，后端在完整 waveform 读取前拒绝超过 120 分钟的本地说话人任务。该证据只覆盖合成双声线和单台设备，不能替代真实会议、相似声线、抢话、噪声、多人及更多机型质量验收，因此模型继续保持 experimental。

## V2-1 Realtime ASR

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| MR-V2-001 | 会议使用独立 ASR 配置，active session（活跃会议）快照 provider、model 和 silence preset。 | V2 plan 2；V2-1 plan | partial | `MeetingRecord.realtimeAsr` 已持久化配置来源 provider、实际协议 provider、实际模型和 silence preset；旧 JSON、round-trip、模型锁定和持久化回滚测试通过 | 仍需设置变更、重启审计和真实 Tauri 会议人工验收 | 保存的是构建后实际生效模型，不是可能为空的 model override |
| MR-V2-002 | `fun-asr-realtime` 提供 draft/final 和 timestamp metadata，draft 不持久化。 | V2 plan 3；V2-1 plan | partial | `asr/bailian.rs`、`asr/mod.rs`、`coordinator/meeting.rs` 已有 draft/final sink、metadata、stale session guard；draft 仅通过事件进入前端内存状态 | 现有 meeting/bailian 测试已通过；仍需真实百炼会议与网络中断验证 | summary/search/export 读取 `MeetingRecord.transcriptSegments`，不读取 draft 事件状态 |
| MR-V2-003 | 暂停 / 继续保持统一会议时间轴，录音 part 使用 1-based index。 | V2-1 plan | partial | `coordinator/meeting.rs` 在 resume 时创建新 provider session，使用 `elapsedMs` 作为 `sessionStartMs`；`audioPartIndex` 来自 1-based part counter | 现有 meeting 测试已通过；仍需多次暂停恢复真实会议 | V2-2 对齐与流式音频源依赖此项 |

## V2-2 会后 ASR 与说话人处理

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| MR-V2-101 | 设置提供独立的会后 ASR 模型下拉和关闭、云端处理、本地处理三态说话人选项。 | V2-2 plan 1-2 | partial | `ProvidersSection.tsx` 从后端注册表渲染会后模型下拉和说话人三态；源码版 Tauri 已确认独立会后设置、三态选项和本地处理上传边界文案 | 仍需恢复模型下拉、本地模型就绪和保存后的完整人工验证 | 会后模型与说话人处理已拆成独立字段 |
| MR-V2-102 | 开始会议选择自动或预计发言人数，并将本场配置完整快照。 | V2-2 plan 2.2、4 | partial | `Meetings.tsx` 提供 auto / 1～20 人；后端持久化 `MeetingPostProcessingConfig`；新增测试确认配置解析后不受后续全局偏好变化影响 | 仍需开始会议、进行中修改设置和重启后的 Tauri 集成验证 | 后端保存 `Option<u32>`，实时实际模型建立后同步写入已固化配置 |
| MR-V2-103 | 会后 ASR 默认 `fun-asr`，下拉可选 `paraformer-v2`，不能路由到其他模型。 | V2 plan 1、3；V2-2 plan 1、7 | partial | 后端注册表只返回两个模型，默认和非法模型测试通过；设置和重试下拉均使用注册表；worker 只按快照中的显式 modelRef 构建对应百炼客户端；两个模型均完成真实完整 worker 短音频任务 | 双模型真实百炼 worker 链路通过；仍需 Tauri 下拉、现场会议和改选重试人工验证 | 实时 ASR 与会后 ASR 独立快照；备选不表示自动 fallback |
| MR-V2-104 | 停止会议快速返回，后处理任务持久化并在后台执行。 | V2-2 plan 5、10 | partial | `stop_meeting_recording` 固化实时 revision、job 和 hold，持久化后清理 runtime 并 spawn 后台 job；隔离临时 `MeetingStore` 中的真实百炼 worker 已从 `Pending` 完整执行并落盘；重开 store 后生产启动扫描规则可选中已有 taskId 的 `Running` job 并继续轮询完成 | 真实双模型 worker 落盘、持久化恢复及状态机测试通过；仍缺 stop 延迟测量、自动总结顺序和真正 Tauri 进程重启人工验证 | stop IPC 不等待云端任务完成 |
| MR-V2-105 | 会议音频使用 `MeetingAudioSource` 从分片 WAV 按块读取，上传路径不复制完整 PCM/WAV。 | V2-2 plan 6 | partial | `MeetingAudioSource` 以单 header + 64 KiB PCM 块输出确定长度；百炼 multipart 使用 `Body::wrap_stream`，集成测试确认两段 WAV 上传体只有一个 RIFF 且 PCM 顺序完整；上传连接关闭时明确失败、只尝试一次且保留源分片；真实短音频已通过百炼临时 OSS 流式上传 | 音频源、multipart、本机回环断线和真实 OSS 短上传通过；仍缺真实公网中断、峰值内存和 120 分钟测试 | 现有短口述内存客户端不用于会议；未知结果的上传 POST 不静默重试 |
| MR-V2-106 | `fun-asr` 和 `paraformer-v2` 共用任务框架并分别适配请求 / 结果；云端说话人模式解析 sentence time、text、speaker_id。 | V2-2 plan 6.3、7 | partial | 统一完成临时 OSS 上传、异步提交、轮询和结果下载；双模型专属解析入口保留时间戳、sentenceId、speakerId；请求测试覆盖 diarization、合法 speakerCount、轮询瞬态失败和排队任务远端取消；两个模型均已在真实百炼凭据下分别完成云端分离、关闭分离和本地分离完整 worker | parser / 故障测试及双模型三种说话人模式的真实完整 worker 通过；仍需真实多人质量和 Tauri 验证 | 关闭或本地说话人处理时设置 `diarization_enabled=false`；1 人不发送非法 speakerCount |
| MR-V2-107 | 云端结果作为新的整理后原文 revision 原子提交，失败保留实时原文和旧总结。 | V2-2 plan 4、5、7 | partial | worker 将结构化结果写入 staging revision，并在 jobId、status、providerTaskId、modelRef 全部匹配后原子激活；真实双模型 worker 与重开 store 后恢复的 `fun-asr` worker 均将 completed revision 和同 task provider metadata 写入隔离 `meetings.json`；无效结果、迟到 job 及原子持久化 rename 失败均保留旧 active revision、原文和总结 | 双模型真实 worker、持久化恢复、自动状态机和 Windows 持久化故障注入通过；仍缺真正 Tauri 进程重启 | speakerId 只应用于同一模型返回的文本，不跨模型硬贴 |
| MR-V2-108 | 总结只在会后 ASR / 说话人处理完成，或用户明确沿用实时原文后启动。 | V2-2 plan 1、5 | partial | 云端结果原子激活并将任务置为 `completed` 后才调用自动总结；`meeting_summary.rs` 仍只接受 `completed` / `realtime_accepted`；失败或取消后可明确沿用实时原文 | meeting 和 meeting_post_processing 自动测试通过；仍缺真实完成顺序和 Tauri 失败 UI 验证 | 关闭说话人也必须先完成会后 ASR |
| MR-V2-109 | 本地模型包支持下载、取消、校验、重试、选择、占用保护和删除。 | V2-2 plan 2.1、8.4 | partial | 独立组合包 catalog、双资产下载与 SHA-256、`.partial` 原子激活、readiness、设置页生命周期操作、浏览器 mock、后端模型 ID / ready 校验及活跃会议 / 任务占用保护已实现；本机已安装组合包通过 manifest / checksum 校验并完成真实推理；共享 Range 下载核心会拒绝截断响应和错误 `Content-Range`、回滚失败 chunk 的临时进度并按 `.partial.idx` 只续传缺失 chunk，说话人下载取消使用独立任务 key | 下载核心 8 项、说话人组合包 6 项、下载管理 11 项定向测试及 MSVC `cargo check` 通过；仍需真实 Tauri 下载取消、占用删除和损坏包重试 UI 验证 | 缺模型或模型 invalid 时前后端均阻止开始；不会出现在 ASR 下拉 |
| MR-V2-110 | 本地管线执行 segmentation、embedding、clustering，产生 SpeakerTurn 并对齐所选会后 ASR 生成的整理后原文。 | V2-2 plan 8 | partial | `speaker_diarization_runtime.rs` 创建 sherpa-onnx `OfflineSpeakerDiarization`，预计人数映射 `num_clusters`，输出稳定 `speaker-N` / overlap；worker 在云端无说话人结果后进入 `local_analyzing`，按句子与 turn 最大重叠对齐，低覆盖、多显著 speaker、缺 timestamp 和 overlap 标记待确认，最后原子激活 `LocalPostprocess` revision；真实 `fun-asr` / `paraformer-v2 + 本地说话人处理` worker 均已运行 segmentation、embedding、2 人 clustering，将云端句子绑定到本地 speaker 并写入隔离 `meetings.json` | 定向单元测试、双模型真实完整 worker 组合、合成双声线长音频及重新读盘验证通过；仍需真实人声质量和 Tauri 验证 | 不拆字、不复制文本，不把本地 speaker 标签贴回实时原文；预计人数不一致只提示，不使任务失败 |
| MR-V2-111 | 本地长会议执行预检并遵循已验证的时长 / 内存上限。 | V2-2 plan 8.3 | partial | 推理前按 PCM 样本数计算单个 `Vec<f32>` waveform 与 512 MiB runtime headroom，读取前检查 Windows 可用物理内存和 sherpa 可寻址样本上限；本机合成双声线 30 / 60 / 120 分钟真实模型基准峰值分别为 566.2 / 804.1 / 1454.1 MiB。catalog 现声明建议最长 120 分钟和“峰值约 1.5 GiB，建议至少 4 GiB 可用内存”，后端在读取完整 waveform 前强制拒绝超过 120 分钟的任务并提示改用云端 | 120 分钟允许、超过 120 分钟拒绝、catalog 能力值和分片 waveform 测试通过；仍需真实会议质量和更多机型验证 | 模型继续保持 experimental；资源上限已落成后端约束，不只依赖 UI |
| MR-V2-112 | 发言人使用稳定 speakerId 和会议级重命名映射。 | V2-2 plan 2.3、4、7 | partial | 云端 provider speakerId 按首次出现顺序映射为会议内稳定 `speaker-N`，同时持久化 `SpeakerProfile` / `SpeakerTurn`；提供后端重命名 IPC 和会议详情编辑 UI；会议详情、Markdown 导出、完整总结 prompt 和 rolling-context chunk 均通过 `speakerId -> SpeakerProfile.displayName` 解析人工名称，未知 speakerId / 旧记录回退原 `speakerLabel`；重试挂起期间保留当前 active revision 和人工姓名，只有新 revision 完整激活后才用新模型生成的 speaker identity 替换旧映射 | 名称映射、JSON 重新加载后导出、总结完整 prompt、长会议分块和重新处理状态机测试通过；浏览器 mock 无头交互确认保存后编辑框和会议原文标签同步从“发言人 1”更新为“张三”；仍缺真实多人重命名和真正 Tauri 重开验证 | 不逐段改模型标签字符串；重新处理后 speaker identity 可能变化，不按相同 ID 盲目继承人工姓名 |
| MR-V2-113 | 后处理失败支持重试、取消、沿用实时原文并总结，且不静默切换模型或说话人处理方式。 | V2-2 plan 2.3、3、10 | partial | worker 已输出上传、提交、轮询、结果校验和凭据等分阶段错误码；取消会中断本地上传 / 轮询、阻止迟到写回，并在已有 taskId 时后台尽力请求远端取消；轮询瞬态 GET 有界重试，异步提交 POST 结果未知时只发送一次；旧 job、重复重试 / 取消和恢复状态测试通过 | 仍需真实公网错误注入和全部 Tauri 操作；百炼只允许远端取消 `PENDING` 任务，需真实验证排队 / 运行中两种结果 | 失败保留 processing hold 以便重试；远端取消失败不回滚本地取消 |
| MR-V2-114 | processing hold 保护任务音频，retention=0 也只能在终态后删除。 | V2-2 plan 9 | partial | stop 创建 hold；retention=0 和全局 prune 均跳过 hold；已有 taskId 的真实 `Running` job 持有 hold 写盘，重开 store 并恢复完成后才释放；取消 / 沿用实时原文也释放 hold；删除在存储锁内失效任务并清理音频 | 自动 retention、真实 provider 持久化恢复、删除事务和迟到 worker 测试通过；仍缺真正 Tauri 进程重启与 retention UI 人工验证 | 音频保留与处理占用分离 |
| MR-V2-115 | `fun-asr` 失败不自动切换 `paraformer-v2`；用户改选后重试生成新 attempt 和 revision。 | V2-2 plan 3、11、13 | partial | worker 对快照 modelRef 只执行一次所选模型链路，没有 fallback 分支；重试显式解析新选择并原子生成新 jobId、attempt 和 processing revision，拒绝旧 staging revision且保留实时 active revision；隔离导入任务已证明 `fun-asr` 失败终态不改写路由，显式改选后真实 `paraformer-v2` worker 完成 attempt 2 / revision 2 并重开读盘 | 状态机、真实百炼改选重试和持久化验证通过；仍缺真实公网故障注入和 Tauri 人工改选操作 | “备选”不是自动 fallback |

## V2-3 音频文件导入

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| MR-V2-201 | 会议页提供单文件“导入音频”，显示文件信息、ASR 模型、区分发言人、人数和总结选项。 | V2-3 plan 1-2 | partial | `Meetings.tsx` 已接原生选择、配置弹窗、模型 readiness、上传提示、进度、取消、重试和总结失败提示；源码版 Tauri 已确认入口、默认模型、三态、条件人数、总结开关及无固定运行位置 | 仍需真实文件选择、IPC、进度、取消和重试人工验证 | 不再提供固定“识别位置”字段 |
| MR-V2-202 | 文件选择使用可信 selection token，前端不能提交任意路径让后端读取。 | V2-3 plan 11 | partial | `choose_meeting_audio_file` 在可信 command 内探测路径，只返回一次性 10 分钟 token 和公开元数据；测试确认不暴露路径、过期 / 重用 / registry 重启均拒绝；`fun-asr`、`paraformer-v2` 及本地两种导入闭环均通过一次性 token 创建并持久化任务，第二次消费被拒绝 | 仍需真实路径攻击、原生文件选择器和完整应用重启人工验证 | token 仅存进程内存，不持久化源文件绝对路径 |
| MR-V2-203 | 标准 PCM WAV 必须支持；其他格式仅在 decoder POC 通过后开放。 | V2-3 plan 3 | partial | 选择器首版只开放 WAV；probe 严格拒绝伪扩展和压缩 WAV，接受包含额外 chunk 的中文路径 PCM WAV；规范化支持 PCM 声道下混和采样率转换 | 自动格式与中文路径测试通过；仍需完整格式矩阵和 Windows/macOS 打包验证 | 未接 decoder，不依赖系统 ffmpeg，也未宣称支持其他格式 |
| MR-V2-204 | 源文件只读，按块复制 / 解码到 staging，完成后原子生成受管标准 WAV。 | V2-3 plan 3-4 | partial | `normalize_pcm_wav` 按块读取、下混 / 重采样，先写 `.partial` 再原子 rename；hash / mtime、取消清理和最终 rename 失败测试确认源文件不变、partial 清理且不覆盖占用目标；真实闭环已将中文路径 22.05 kHz 双声道 PCM WAV 转为受管 16 kHz 单声道 WAV，并确认源文件 SHA-256 与 mtime 不变；重选禁止把本会议受管副本当源文件 | 自动故障边界和真实规范化闭环通过；仍缺真实磁盘不足与进程强杀故障注入 | 不修改、移动或删除用户源文件 |
| MR-V2-205 | 导入状态可展示、取消、重试和跨应用重启恢复，失败不伪装为 completed。 | V2-3 plan 5-6、9 | partial | 持久化 `MeetingImportState` 驱动前端；取消覆盖复制和后处理 worker，重试复用有效受管 WAV 或要求重选；启动恢复会清理 `.partial`、释放 hold，并把总结中断标记为仅重试总结；真实云端和本地导入闭环均从独立 `meetings.json` 重开读盘后保持 completed 与 active revision 一致 | 导入持久化、删除等待、重启转换和总结中断测试通过；仍需真正退出并重启 Tauri 进程与全状态 UI 验证 | 无有效受管 WAV 的取消 Draft 删除；完整 WAV 的 cancelled 记录保留并执行 retention |
| MR-V2-206 | 后端根据 ASR 模型 descriptor 的 `runtimeKind` 动态路由云端 adapter 或本地 engine，不能信任前端自报类型。 | V2-3 plan 5、7 | partial | `resolve_meeting_asr_model` 只接受后端注册表中的 `providerId + modelId`；worker 在 dispatch 前再次核对 descriptor、state 和 import config；测试确认伪造两处 `runtimeKind` 或模型引用不一致均拒绝；请求类型没有前端可提交的 `runtimeKind` 或 endpoint 字段；真实 `fun-asr`、`paraformer-v2` 和本地 SenseVoice 两种组合均从 selection token、持久化快照、规范化进入各自 worker，并完成 revision 和重开读盘 | 注册表、安全路由、快照篡改及双云端 / 双本地组合完整导入持久化闭环通过；仍需原生文件选择器和前端进度观测 | 云端默认 `fun-asr`，可选 `paraformer-v2` |
| MR-V2-207 | 本地 ASR 模型和本地说话人模型分别完成 capability / readiness 检查。 | V2-3 plan 7.2、8.1 | partial | 本地文件 ASR 只列出 `supportsMeetingFile=true` 且 Offline 的 sherpa 模型并返回 readiness；本地说话人模型沿用独立 catalog、manifest / checksum 和 ready 校验，组合开始前再次由后端验证；本机已安装 SenseVoice 和说话人组合包均通过真实运行时加载与推理 | 注册表、模型加载、本地 ASR 和本地说话人短链路通过；仍需组合包下载取消、损坏恢复和 Tauri readiness 人工验证 | 两类模型独立选择，不创建第二套下载器 |
| MR-V2-208 | 本地开启区分发言人时执行 diarization-first，再按 speaker windows 做 batch ASR。 | V2-3 plan 8.2 | partial | 本地路径先运行 speaker diarization，再合并同 speaker 短间隔 turn；长 window 优先在 30 秒目标前寻找低能量静音帧，找不到才硬切，逐窗 ASR 后保存绝对时间戳、speakerId 和 overlap 标记；本机合成双声线已完成 30 / 60 / 120 分钟真实 diarization-first + SenseVoice 生产闭环，三组均检测两个 speaker 并完成 revision 激活和重开读盘 | 长 turn、静音辅助切分、overlap 单元测试及合成长音频完整导入持久化闭环通过；仍需真实多人、相似声线、抢话、噪声和更多设备验证 | 不依赖不存在的整段词级时间戳；本地说话人处理硬上限为 120 分钟 |
| MR-V2-209 | 本地关闭区分发言人时执行 VAD / 有界分块 batch ASR，保留绝对时间戳。 | V2-3 plan 8.3 | partial | 本地路径以 30 秒为硬上限，在每个边界前 5 秒内寻找 20 ms 低能量静音帧，无合格切点才硬切；`MeetingBatchTranscriber` 逐窗读取 PCM 并写入 provider start / end。30 / 60 / 120 分钟无隐私合成语音均已真实完成 SenseVoice 多窗口导入、绝对时间戳、revision 激活和重开读盘，120 分钟输入耗时 242.56 秒 | 静音感知切点、有界回退、两小时边界及本地 30 / 60 / 120 分钟资源基准通过；仍未做真实会议识别质量 | 不引入新 VAD 模型；不把两小时音频一次喂给短口述 provider |
| MR-V2-210 | 导入完成后复用会议详情、总结、播放、导出、删除和 retention。 | V2-3 plan 10、14 | partial | 导入直接创建 `MeetingRecord` 并复用现有详情、原文、总结、音频播放器和 Markdown 导出；真实 `fun-asr`、`paraformer-v2` 与本地 SenseVoice 两种组合均写入 `Imported` active revision、保留受管音频并在 completed 后释放 processing hold，重新打开 `meetings.json` 后状态一致；详情、导出和总结统一解析人工重命名后的 `SpeakerProfile.displayName`；删除先等待 worker 释放句柄再清理受管音频 | 双云端 / 双本地持久化闭环、自动状态机、重命名名称的 JSON 重开导出与总结输入、浏览器 mock 详情交互、删除等待和 retention 测试通过；仍需真实 Tauri 的进度、播放、导出、删除和总结人工验证 | 不另建第二套历史页或隐藏音频池 |
| MR-V2-211 | 云端 ASR + 关闭 / 云端分离 / 本地分离，以及本地 ASR + 关闭 / 本地分离按兼容矩阵执行；本地 ASR + 云端分离被明确拒绝。 | V2-3 plan 2、7-8 | partial | 前后端均按 descriptor 能力渲染 / 校验组合；云端三种路径复用会后 adapter 与本地 alignment，本地两种路径进入 batch engine；本地 + 云端在任何上传前返回明确错误；本地 ASR + 关闭和本地 ASR + 本地分离均已完成完整导入持久化闭环，双模型三种云端组合已有完整 worker 证据 | 禁止组合、无上传契约、云端完整 worker及两种本地完整导入闭环通过；仍需真实人声、长会议和 UI 验证 | 禁止静默改写组合或意外上传 |
| MR-V2-212 | 所选 ASR 模型失败后不自动改用其他云端或本地模型；用户改选重试创建新 revision。 | V2-3 plan 9、13 | partial | 每个 import config 固化显式 modelRef 和 resolved runtime；worker 无 fallback 分支，并在调用云端 adapter / 本地 engine 前重新核对后端注册表与持久化快照；已有受管音频的改选模型会原子更新 import / post-processing 配置、attempt、processingRevision、旧 staging 和 hold；真实百炼测试进一步确认 `fun-asr` 失败后未自动切换，用户显式改选才创建新 job，并由 `paraformer-v2` 完成 revision 2 与重开读盘 | 状态机、revision、真实云端 worker 和导入持久化改选重试通过；仍需真实公网故障注入和 Tauri 人工改选操作 | 路由可追溯，`paraformer-v2` 仅为手动备选 |

## Cross-Cutting Verification（跨阶段验证）

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| MR-V2-301 | 真实音频覆盖 1/2/4/8 人、相似声线、抢话、静音和背景噪声。 | V2-2 plan 13；V2-3 plan 13 | missing | - | 标注集与人工记录 | 记录 DER、人数差异、字错率 |
| MR-V2-302 | 30/60/120 分钟覆盖上传内存、云端耗时、本地内存和处理耗时。 | V2-2 plan 13；V2-3 plan 13 | partial | 2026-08-13 在 i5-14400 / 15.6 GB / Windows 11 上完成两条本地生产导入基准：`sense-voice-small-zh + 关闭说话人处理` 为 30 分钟 60.14 秒 / 509.9 MiB、60 分钟 119.51 秒 / 565.5 MiB、120 分钟 242.56 秒 / 673.5 MiB；`sherpa-pyannote-3dspeaker-zh-v1 + sense-voice-small-zh` 为 30 分钟 391.20 秒 / 566.2 MiB、60 分钟 775.06 秒 / 804.1 MiB、120 分钟 1544.22 秒 / 1454.1 MiB。全部完成识别、持久化、重开验证和清理 | Windows 真实机器、合成中文音频、debug 测试进程、隐藏窗口、BelowNormal 优先级；峰值工作集是偏保守的端到端进程观测值 | 仍缺云端 30 / 60 / 120 分钟上传与处理耗时、真实会议质量和更多机型 |
| MR-V2-303 | 网络中断、模型缺失、磁盘不足、应用退出、删除会议均有确定恢复或清理结果。 | V2-2 plan 9-10；V2-3 plan 9-10 | partial | 已覆盖缺本地模型拒绝、非终态启动扫描、同 jobId 单 worker、旧 job guard、processing hold、导入 `.partial` 清理、原子提交失败清理、上传连接关闭明确失败且不重试 POST、轮询连续 5xx 有界失败、提交连接断开只发送一次、持久化 rename 失败保留旧 revision、总结中断释放 hold、两阶段删除等待和原子删除；真实已有 providerTaskId 的 `Running` job 重开 store 后只继续轮询，音频 resolver 调用为 0；提交结果未知时拒绝自动重提 | 本机回环、文件故障注入及真实百炼持久化恢复通过；仍缺真实公网上传 / 提交断网、磁盘不足和真正 Tauri 进程重启 | 不重复提交未知结果的云端任务，不留下不可解释写回 |
| MR-V2-304 | 当前其他 ASR、短口述、会议播放、总结和导出链路无回归。 | AGENTS.md；V2 plan | partial | Phase 6 的 `meeting_audio_import` 23 项、`meeting_post_processing` 34 项（另 4 项真实集成测试默认 ignored）、`meeting_summary` 19 项、`meeting_markdown` 3 项、完整 meeting 172 项、dashscope_multimodal 29 项、bailian 39 项、preferences 13 项、credentials 46 项、下载核心 / 说话人包 / 下载管理 8 / 6 / 11 项、MSVC `cargo check`、`tsc --noEmit` 和生产 build 均通过；另有默认 ignored 的真实本地多窗口导入测试通过 | Tauri dev 与短口述 / 播放 / 总结 / 导出人工回归按用户要求暂停 | ASR 为高风险区域 |

## Deferred Log（延期记录）

| ID | Requirement（需求） | Deferred reason（延期原因） | User confirmation（用户确认） |
| --- | --- | --- | --- |
| MR-V2-D01 | 实时说话人标签 | 用户已确认首版做会后批处理 | 2026-08-12 之前的需求确认 |
| MR-V2-D02 | `fun-asr`、`paraformer-v2` 之外的云端会后模型 | 用户明确首期只实现这两个模型 | 2026-08-12 当前请求 |

批量文件、视频和多文件拼接导入只是 V2-3 的当前范围边界，不是从已确认 V2 requirement（需求）中延期的条目，因此不记为 `deferred`。

## Final Acceptance Gate（最终验收门槛）

在声称会议 V2 完成前：

- 当前版本所有 requirement 必须为 `done`，或有明确用户确认的 `deferred`。
- `partial/missing/blocked` 必须在交付中逐项说明，不能用“最小闭环”静默移出。
- 云端和本地都必须完成至少一条真实端到端路径；mock（模拟）和 parser fixture（解析样例）不能替代真实 ASR。
- 必须完成一小时会议稳定性和 120 分钟资源基准；如果本地模型无法支持 120 分钟，应把验证结果转成明确产品限制，而不是忽略该项。
- 所有文档、代码、UI 文案、默认值和能力矩阵必须保持一致。
