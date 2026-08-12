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

## V2-1 Realtime ASR

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| MR-V2-001 | 会议使用独立 ASR 配置，active session（活跃会议）快照 provider、model 和 silence preset。 | V2 plan 2；V2-1 plan | partial | `MeetingRecord.realtimeAsr` 已持久化配置来源 provider、实际协议 provider、实际模型和 silence preset；旧 JSON、round-trip、模型锁定和持久化回滚测试通过 | 仍需设置变更、重启审计和真实 Tauri 会议人工验收 | 保存的是构建后实际生效模型，不是可能为空的 model override |
| MR-V2-002 | `fun-asr-realtime` 提供 draft/final 和 timestamp metadata，draft 不持久化。 | V2 plan 3；V2-1 plan | partial | `asr/bailian.rs`、`asr/mod.rs`、`coordinator/meeting.rs` 已有 draft/final sink、metadata、stale session guard；draft 仅通过事件进入前端内存状态 | 现有 meeting/bailian 测试已通过；仍需真实百炼会议与网络中断验证 | summary/search/export 读取 `MeetingRecord.transcriptSegments`，不读取 draft 事件状态 |
| MR-V2-003 | 暂停 / 继续保持统一会议时间轴，录音 part 使用 1-based index。 | V2-1 plan | partial | `coordinator/meeting.rs` 在 resume 时创建新 provider session，使用 `elapsedMs` 作为 `sessionStartMs`；`audioPartIndex` 来自 1-based part counter | 现有 meeting 测试已通过；仍需多次暂停恢复真实会议 | V2-2 对齐与流式音频源依赖此项 |

## V2-2 会后 ASR 与说话人处理

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| MR-V2-101 | 设置提供独立的会后 ASR 模型下拉和关闭、云端处理、本地处理三态说话人选项。 | V2-2 plan 1-2 | missing | - | TS/build + UI 人工验证 | 会后模型与说话人处理不能混为一个字段 |
| MR-V2-102 | 开始会议选择自动或预计发言人数，并将本场配置完整快照。 | V2-2 plan 2.2、4 | missing | - | 序列化测试 + 设置变更不影响进行中会议 | 后端保存 `Option<u32>` |
| MR-V2-103 | 会后 ASR 默认 `fun-asr`，下拉可选 `paraformer-v2`，不能路由到其他模型。 | V2 plan 1、3；V2-2 plan 1、7 | missing | - | 默认值、下拉、注册表和非法模型测试 | 实时 ASR 与会后 ASR 独立快照 |
| MR-V2-104 | 停止会议快速返回，后处理任务持久化并在后台执行。 | V2-2 plan 5、10 | missing | - | stop 延迟测试 + 页面离开 / 重启恢复 | stop 成功不等于后处理完成 |
| MR-V2-105 | 会议音频使用 `MeetingAudioSource` 从分片 WAV 按块读取，上传路径不复制完整 PCM/WAV。 | V2-2 plan 6 | missing | - | 合并 header、取消、峰值内存和 120 分钟测试 | 当前内存客户端不能直接用于会议 |
| MR-V2-106 | `fun-asr` 和 `paraformer-v2` 共用任务框架并分别适配请求 / 结果；云端说话人模式解析 sentence time、text、speaker_id。 | V2-2 plan 6.3、7 | missing | - | 双模型 fixture parser + 真实百炼任务 | 关闭说话人时设置 `diarization_enabled=false` |
| MR-V2-107 | 云端结果作为新的整理后原文 revision 原子提交，失败保留实时原文和旧总结。 | V2-2 plan 4、5、7 | missing | - | 故障注入 + revision 恢复测试 | 禁止跨模型硬贴 speaker label |
| MR-V2-108 | 总结只在会后 ASR / 说话人处理完成，或用户明确沿用实时原文后启动。 | V2-2 plan 1、5 | missing | - | 调用顺序和失败 UI 测试 | 关闭说话人也必须先完成会后 ASR |
| MR-V2-109 | 本地模型包支持下载、取消、校验、重试、选择、占用保护和删除。 | V2-2 plan 2.1、8.4 | missing | - | 模型生命周期测试 + Windows 人工验证 | 缺模型不能开始必然失败任务 |
| MR-V2-110 | 本地管线执行 segmentation、embedding、clustering，产生 SpeakerTurn 并对齐所选会后 ASR 生成的整理后原文。 | V2-2 plan 8 | missing | - | 单人 / 多人 / 缺 timestamp / overlap 测试 | 预计人数映射 `num_clusters`；不把说话人标签跨模型硬贴回实时原文 |
| MR-V2-111 | 本地长会议执行预检并遵循已验证的时长 / 内存上限。 | V2-2 plan 8.3 | missing | - | 30/60/120 分钟基准测试 | 未验证前只能标记 experimental |
| MR-V2-112 | 发言人使用稳定 speakerId 和会议级重命名映射。 | V2-2 plan 2.3、4、7 | missing | - | 重命名、重开、导出、重新处理测试 | 不逐段改模型标签字符串 |
| MR-V2-113 | 后处理失败支持重试、取消、沿用实时原文并总结，且不静默切换模型或说话人处理方式。 | V2-2 plan 2.3、3、10 | missing | - | 全部错误操作人工验证 | 错误文案需可执行 |
| MR-V2-114 | processing hold 保护任务音频，retention=0 也只能在终态后删除。 | V2-2 plan 9 | missing | - | retention、取消、删除和重启测试 | 音频保留与处理占用分离 |
| MR-V2-115 | `fun-asr` 失败不自动切换 `paraformer-v2`；用户改选后重试生成新 attempt 和 revision。 | V2-2 plan 3、11、13 | missing | - | 故障注入 + 重试契约测试 | “备选”不是自动 fallback |

## V2-3 音频文件导入

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| MR-V2-201 | 会议页提供单文件“导入音频”，显示文件信息、ASR 模型、区分发言人、人数和总结选项。 | V2-3 plan 1-2 | missing | - | UI + IPC 人工验证 | 不再提供固定“识别位置”字段 |
| MR-V2-202 | 文件选择使用可信 selection token，前端不能提交任意路径让后端读取。 | V2-3 plan 11 | missing | - | token 一次性、过期和路径攻击测试 | 不持久化源文件绝对路径 |
| MR-V2-203 | 标准 PCM WAV 必须支持；其他格式仅在 decoder POC 通过后开放。 | V2-3 plan 3 | missing | - | 格式矩阵与中文路径测试 | 不静默依赖系统 ffmpeg |
| MR-V2-204 | 源文件只读，按块复制 / 解码到 staging，完成后原子生成受管标准 WAV。 | V2-3 plan 3-4 | missing | - | hash/mtime、磁盘不足、取消、partial 清理 | 不修改或删除用户源文件 |
| MR-V2-205 | 导入状态可展示、取消、重试和跨应用重启恢复，失败不伪装为 completed。 | V2-3 plan 5-6、9 | missing | - | 状态机和恢复测试 | Draft 记录只在开始 staging 后创建 |
| MR-V2-206 | 后端根据 ASR 模型 descriptor 的 `runtimeKind` 动态路由云端 adapter 或本地 engine，不能信任前端自报类型。 | V2-3 plan 5、7 | missing | - | 路由、伪造字段、非法模型测试 | 云端默认 `fun-asr`，可选 `paraformer-v2` |
| MR-V2-207 | 本地 ASR 模型和本地说话人模型分别完成 capability / readiness 检查。 | V2-3 plan 7.2、8.1 | missing | - | manifest、缺文件、checksum 和组合测试 | 两类模型可以独立选择 |
| MR-V2-208 | 本地开启区分发言人时执行 diarization-first，再按 speaker windows 做 batch ASR。 | V2-3 plan 8.2 | missing | - | 多人、长 turn、overlap 和窗口切分测试 | 不依赖不存在的整段词级时间戳 |
| MR-V2-209 | 本地关闭区分发言人时执行 VAD / 有界分块 batch ASR，保留绝对时间戳。 | V2-3 plan 8.3 | missing | - | 长音频分块与拼接测试 | 不把两小时音频一次喂给短口述 provider |
| MR-V2-210 | 导入完成后复用会议详情、总结、播放、导出、删除和 retention。 | V2-3 plan 10、14 | missing | - | 端到端人工验证 | 不另建第二套历史页 |
| MR-V2-211 | 云端 ASR + 关闭 / 云端分离 / 本地分离，以及本地 ASR + 关闭 / 本地分离按兼容矩阵执行；本地 ASR + 云端分离被明确拒绝。 | V2-3 plan 2、7-8 | missing | - | 全组合 UI + 后端契约测试 | 禁止静默改写组合或意外上传 |
| MR-V2-212 | 所选 ASR 模型失败后不自动改用其他云端或本地模型；用户改选重试创建新 revision。 | V2-3 plan 9、13 | missing | - | 故障注入 + revision 测试 | 路由可追溯 |

## Cross-Cutting Verification（跨阶段验证）

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| MR-V2-301 | 真实音频覆盖 1/2/4/8 人、相似声线、抢话、静音和背景噪声。 | V2-2 plan 13；V2-3 plan 13 | missing | - | 标注集与人工记录 | 记录 DER、人数差异、字错率 |
| MR-V2-302 | 30/60/120 分钟覆盖上传内存、云端耗时、本地内存和处理耗时。 | V2-2 plan 13；V2-3 plan 13 | missing | - | Windows 真实机器基准 | 作为本地模型能力上限依据 |
| MR-V2-303 | 网络中断、模型缺失、磁盘不足、应用退出、删除会议均有确定恢复或清理结果。 | V2-2 plan 9-10；V2-3 plan 9-10 | missing | - | 故障注入 + 人工验证 | 不留下悬空任务或不可解释音频 |
| MR-V2-304 | 当前其他 ASR、短口述、会议播放、总结和导出链路无回归。 | AGENTS.md；V2 plan | partial | Phase 1 的 TypeScript/build、MSVC `cargo check` 及 meeting/bailian/preferences/credentials 测试通过 | 仍需 Tauri dev 与短口述/播放/总结/导出人工回归 | ASR 为高风险区域 |

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
