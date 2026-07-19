# 会议桌宠 V1 Phase 6 已知问题与修复记录

状态：P0 已修复；Windows 多屏 / DPI 真机验收仍为 partial

日期：2026-07-19

## 1. 文档用途

本文记录会议桌宠 V1 Phase 6 最终验收中发现的问题，以及 2026-07-19 后续修复和验证结果。它既保留问题根因，也明确区分“代码已修复”“自动验证通过”和“仍缺真实设备证据”。

本轮按独立稳定性任务修复 ASR / ONNX Runtime（ONNX 运行时）、长录音内存、构建 warning（警告）、更新检查和格式基线；没有改变会议桌宠产品范围。

## 2. 优先级总览

| ID | 优先级 | 问题 | 当前影响 | 证据状态 |
| --- | --- | --- | --- | --- |
| MC-P6-KI-001 | P0 | 暂停或停止会议时 ONNX Runtime API 版本不匹配并崩溃 | OpenLess 进程退出，阻断会议控制和 `summary_failed` 全流程验收 | `done`：依赖版本对齐，真实模型推理和 Windows Tauri 停止流程通过 |
| MC-P6-KI-002 | P0 | 长录音 sherpa 转写尝试申请约 21GB 内存 | 可能触发内存分配失败、进程崩溃或系统卡顿 | `partial`：有界分片已实现并通过两小时等价规划测试，仍缺真实两小时内存曲线 |
| MC-P6-KI-003 | P1 | 自动化与设备环境无法完成四边、多屏和 DPI 验收 | MC-V1-005、006、024 继续为 `partial` | 拖动等单屏证据已补齐一部分；设备缺口仍在 |
| MC-P6-KI-004 | P2 | 构建 warning 与项目级 rustfmt 漂移 | 增加维护噪声，可能掩盖新增问题 | `done`：Rollup、Rust warning、updater、rustfmt 和行尾基线均已处理 |

## 3. MC-P6-KI-001：ONNX Runtime API 不匹配崩溃

### 现象

在 Windows Tauri dev 中停止一场空原文测试会议时，进程输出：

```text
The requested API version [24] is not available,
only API versions [1, 23] are supported in this build.
Current ORT Version is: 1.23.2
STATUS_ACCESS_VIOLATION
```

OpenLess 随后异常退出，流程未进入预期的 `summary_failed` 状态，因此“打开会议”跳转无法完成本轮复验。

后续复验又使用网易虚拟音频设备开始一场约 21 秒的短会议，并从主会议页点击暂停。日志先记录 `cpal Stream dropped (mic released)`，随后出现完全相同的 ORT API 版本错误和 `STATUS_ACCESS_VIOLATION`。这证明问题不只发生在停止流程：batch ASR（批处理语音识别）在暂停时处理当前音频片段，也会进入同一崩溃路径。

### 影响

- 暂停或停止会议都可能导致整个应用崩溃，而不是进入可恢复的识别或总结失败状态。
- 阻断 MC-V1-017 和 MC-V1-024 的最终验收。
- 可能影响所有经过同一 ONNX Runtime 动态库加载路径的本地 ASR 场景，不应只按桌宠问题处理。

### 根因与修复

根因是同一进程内的 ONNX Runtime 原生 ABI（应用二进制接口）版本不一致：sherpa 绑定请求 API 24，但实际被 Foundry / WinML 路径加载的运行时只提供到 API 23。该错误发生在 Rust 能返回普通 `Result` 之前，因此最终表现为原生访问冲突。

修复采用依赖收敛而不是捕获崩溃：

1. 将 `foundry-local-sdk` 精确锁定为 `=1.2.1`。
2. 将 `sherpa-onnx` 精确锁定为 `=1.13.4`，同步更新 `sherpa-onnx-sys` 和锁文件。
3. 保留 sherpa 静态链接配置，避免由未约束的兼容版本再次引入不同 ORT API。
4. 使用已安装 SenseVoice 模型执行真实静音推理，再在 Windows Tauri dev 中依次验证开始、暂停、继续、再次暂停和停止会议。

### 验收结果

- 已安装模型真实推理测试通过，未再出现 API 版本错误或 `STATUS_ACCESS_VIOLATION`。
- Windows Tauri dev 中，sherpa 对约 44 秒会议完成两段分片转写，暂停、继续和停止均未导致应用退出。
- 停止前将 LLM endpoint（接口地址）临时切到 `https://127.0.0.1:1/v1`，会议按预期进入 `summary_failed`，原文和音频均保留；随后已恢复用户原 ARK endpoint。
- 测试后已将默认 ASR 恢复为 `foundry-local-whisper`。
- 自动验证包括真实模型推理、Rust 全量 `781 passed / 0 failed / 1 ignored`、`cargo check` 和 Windows Tauri dev 流程。

结论：MC-P6-KI-001 已修复。打包安装版仍建议在发布候选包阶段再跑一次相同回归，防止打包 DLL 布局与 dev 环境不同。

### 遗留数据

停止流程崩溃后残留一条状态为 `recording` 的测试会议：

```text
0fba3d76-95fe-4028-b5a6-18bf72c0f628
```

暂停流程复验又残留一条状态为 `recording` 的测试会议：

```text
75881a1f-2b2f-4027-8fa6-ef0d00ebeb15
会议记录 2026-07-19 04:48
```

下次处理前先确认这些记录的存储状态和用户数据边界；未经用户确认不要直接删除。

本轮修复验收又新增一条约 43.7 秒的本地测试会议（界面时间 `2026-07-19 15:01`）。该会议使用 sherpa 完成两段真实转写，并通过临时不可达 LLM endpoint 验证 `summary_failed`，当前状态为“总结失败 / 音频已保留”；未擅自删除。

## 4. MC-P6-KI-002：长录音 sherpa 约 21GB 内存申请

### 现象与根因

本次验收现场曾观察到长录音 sherpa 转写尝试申请约 21GB 内存。根因不是模型本身需要 21GB，而是部分路径把整场 PCM（脉冲编码音频数据）或 WAV 一次性读入内存，并让后续识别构造与总时长成比例的连续缓冲区。两小时 16kHz 单声道 `f32` PCM 本身约 439MB，叠加复制、重采样和模型中间量后会进一步放大。

### 修复前影响

- 长会议可能因超大连续内存分配失败而中断转写或使进程崩溃。
- 即使分配成功，也可能导致明显卡顿、换页和系统内存压力。
- 表明长音频处理路径不能按整场时长构造 PCM 或中间张量，必须把内存上限建立在固定分片而不是总时长上。

### 已实现修复

1. 离线 sherpa 录音改为写入临时 PCM 文件，不再在会议期间持续累积整场内存。
2. 离线和在线 sherpa 均使用容量为 256 的 bounded queue（有界队列）；过载时返回明确错误，不静默丢弃音频。
3. 离线识别按 30 秒分片、750ms 重叠读取，单片 PCM 上限固定为 960,000 字节，并对重叠文本去重。
4. 历史 WAV 重新转写改为流式读取，每片最多 5 分钟，不再 `read_to_end` 整场音频。
5. 通过两小时等价分片规划测试，验证最大分片大小固定，不随会议总时长增长。

### 验收结果与缺口

- 分片边界、重叠去重、有界队列、临时文件清理和两小时等价规划测试均通过。
- Windows Tauri dev 的短会议真实转写验证通过，两段分片顺序正确，暂停和停止后临时音频正常落盘。
- 仍未用真实两小时音频记录进程 working set（工作集内存）和峰值曲线，因此本项保持 `partial`，不能仅凭单元测试声称长会议性能已完全验收。

下一轮只需补真实长音频压测：记录基线、转写峰值、完成后回落值，并确认峰值不随总时长线性增长；若通过即可将 MC-P6-KI-002 标记为 `done`。

## 5. MC-P6-KI-003：Windows 真机验收环境限制

### 已确认限制

- 较早使用的桌面自动化通道调用鼠标位置时出现 `GetCursorPos failed: 拒绝访问`，Win32 `SendInput` 返回 `Access denied`，`CopyFromScreen` 出现 `句柄无效`。
- 后续改用官方 Computer Use Window2 通道后，已完成真实拖动、右 / 下吸附、锁定、隐藏 / 恢复和跨重启位置恢复；但原生窗口移动后窗口相对坐标会被重新映射，无法稳定把桌宠送到左 / 上边缘。
- 当前机器只有单显示器、100% DPI，无法提供副屏负坐标、显示器移除、125% / 150% DPI 的真实设备证据。
- Phase 6 约束禁止自动修改 Windows DPI、分辨率和显示器设置。

### 受影响条目

- MC-V1-005：左 / 上边缘吸附仍缺真机证据；其余拖动、锁定、右 / 下吸附和恢复证据已补齐。
- MC-V1-006：副屏负坐标、保存显示器被移除、125% / 150% DPI 和分辨率变化。
- MC-V1-024：Windows 完整桌面流程最终门槛。

### 下次验收方式

由用户在真实 Windows 桌面手动执行左 / 上吸附和多屏 / DPI 场景，代理只读取结果和日志，不静默调整系统显示设置。每个场景记录显示器布局、缩放比例、窗口保存坐标、重启后坐标和可见性；只有获得真实设备证据后，相关 checklist 才能从 `partial` 改为 `done`。

### 本轮新增验收数据

- 本轮为验证跨重启位置恢复新增了两条本地测试会议，界面时间分别为 `2026-07-19 04:10` 和 `2026-07-19 04:20`，当前均为暂停状态。
- 为避免把测试中捕获的内容发送到总结服务，本轮没有点击停止，也没有删除上述会议；后续处理前需先取得用户确认。
- 测试结束后已确认 `meetingCompanionEnabled=false`、`meetingCompanionPositionLocked=false`、`themeMode=system`，桌宠位置保留为 `DISPLAY5` 的 `(1570,708)`。

## 6. MC-P6-KI-004：构建 warning 与格式漂移

### 已实现修复

- Rollup：页面改为 lazy loading（懒加载），并拆除造成循环 chunk 的桶式导入；构建不再报告循环 chunk 或超过 500kB 的 chunk，最大业务 chunk 为 294.37kB。
- Rust warning：逐项修正未使用导入、变量、条件编译和测试代码问题，没有使用 crate 级全局 `allow` 隐藏警告；`cargo check --message-format=short` 为 0 warning。
- updater（更新检查）：调用 Tauri updater 前先探测并校验 manifest（更新清单）；正式版 manifest 404 按“尚未发布”处理，后台网络失败不再持久化为 ERROR，手动检查仍向用户显示错误。
- rustfmt：新增 `src-tauri/rustfmt.toml`，对当前 Rust 源码建立统一格式基线；`cargo fmt --all -- --check` 通过。
- 行尾：新增仓库 `.gitattributes`，明确文本文件行尾规则；`git diff --check` 通过，不再依赖开发者本机 Git 默认设置。

### 验收结果与残余 warning

- `npm run build` 通过，无 Rollup 循环或大 chunk warning。
- Rust 全量测试为 `781 passed / 0 failed / 1 ignored`，`cargo check` 为 0 warning。
- `cargo fmt --all -- --check` 和 `git diff --check` 均通过。
- 启动日志没有新增 updater 持久 ERROR。
- 仍能看到 sherpa 模型下载检查访问 GitHub release API 时返回 403，并自动回退 HEAD 请求。这不属于 updater 修复范围，且当前回退有效；后续可单独降低重复探测或处理 API 限流噪声。

结论：原 MC-P6-KI-004 所列问题已修复；GitHub release API 403 回退 warning 作为新的低优先级网络噪声保留。

## 7. 下一轮建议顺序

1. 使用真实两小时录音补 MC-P6-KI-002 的内存曲线和取消 / 完成后内存回落证据。
2. 在发布候选安装包中复跑 sherpa 暂停、继续、停止和 `summary_failed`，确认打包 DLL 布局没有重新引入 ORT 冲突。
3. 由用户在真实多屏、125% / 150% DPI 环境补 MC-P6-KI-003 和 MC-V1-024 的设备证据。
4. 单独处理 sherpa GitHub release API 403 的重复 warning，避免把可恢复网络回退写成高噪声日志。

MC-P6-KI-001 和 MC-P6-KI-004 已完成；MC-P6-KI-002、MC-P6-KI-003 仍为 `partial`，因此会议桌宠 V1 仍不能声称完成全部最终验收。
