# 会议录音 V3-2 系统声音采集 Implementation Plan

**Goal（目标）：** 在不破坏现有麦克风会议录音、实时 ASR（语音转文字）、会后 ASR、说话人处理和音频保留链路的前提下，支持采集电脑输出的 system audio（系统声音），使用户可以录制腾讯会议、飞书会议、微信语音等电脑端会议的对方声音。

**Architecture（架构）：** 保留现有 microphone-only（仅麦克风）路径；新增会议专用 system audio capture（系统声音采集）和 `MeetingAudioMixer`（会议音频混音器）。麦克风和系统声音分别解码为带单调时钟的 PCM frame（PCM 音频帧），统一为 16 kHz / mono（单声道）/ f32，按固定 20 ms 节拍对齐和混音，再一次性输出到现有 `AudioConsumer`、实时 ASR 和 `WavArchiver`。对下游仍只呈现一条标准会议音频流。

**Tech Stack（技术栈）：** Tauri 2、Rust、React、TypeScript、Windows WASAPI loopback（Windows 音频会话 API 回环采集）、macOS ScreenCaptureKit（后续阶段）、现有 CPAL 麦克风采集、meeting coordinator（会议协调器）和 meeting audio retention（会议音频保留）链路。

---

状态：`planning`（已进入计划，未实现）  
日期：2026-08-14  
当前开发分支：`codex/meeting-post-asr-routing-20260812`  
关联验收：`docs/meeting-recording-v3-2-system-audio-capture-acceptance-checklist.md`

## 1. 已确认的实施假设

- 当前优先 Windows 10 / 11 x64，先覆盖腾讯会议、飞书会议和微信语音。
- macOS 保留同一上层数据契约，但在 Windows 稳定后使用 ScreenCaptureKit 单独实现和验收。
- 会议录音提供三种模式：仅麦克风、仅系统声音、麦克风 + 系统声音。
- 为避免隐私意外，现有用户升级后默认仍为“仅麦克风”；只有用户明确选择后才采集系统声音。
- Windows 首版按输出 endpoint（输出设备端点）采集系统混音，不按腾讯会议或微信进程进行白名单采集。
- 系统声音会包含所选输出设备上其他应用的提示音、音乐和通知；首版不提供按应用排除。
- 首版不实现 AEC（Acoustic Echo Cancellation，声学回声消除）。外放时系统声音可能再次被麦克风拾取，建议人工验收同时覆盖耳机和外放，并将 AEC 作为后续独立优化。
- 实时 ASR、会后 ASR、云端 / 本地说话人处理仍读取同一条标准受管 WAV，不建立新的会议记录或后处理链路。

## 2. 产品范围

### 2.1 本阶段必须支持

- 会议开始前选择 audio capture mode（音频采集模式）。
- 选择“仅麦克风”时完全复用当前行为。
- 选择“仅系统声音”时，不要求麦克风必须可用。
- 选择“麦克风 + 系统声音”时，录制本人语音和电脑播放的对方语音。
- 会议开始配置中选择输出设备，默认跟随当前 Windows 输出设备。
- 开始时快照实际采集模式、麦克风设备和输出 endpoint，便于追溯。
- 系统声音和麦克风使用不同时钟时，长会议不得持续累积时间轴漂移。
- 暂停、继续、停止同时控制所有已启用的采集源。
- 系统声音运行中失败时保留已录音频；麦克风仍健康时继续麦克风录音，但必须明确提示系统声音已中断。
- 实时信号轨显示实际混音输出；主会议页可以区分麦克风和系统声音是否有输入。
- 现有播放、会后 ASR、说话人处理、总结、导出、删除和 retention（保留策略）复用混音后的受管 WAV。

### 2.2 首版不做

- 按应用 / 进程只录腾讯会议、飞书或微信。
- 自动识别当前正在通话的应用。
- AEC（声学回声消除）、降噪、自动增益和音源分离。
- 独立保留麦克风 / 系统声音原始双轨文件。
- 会议进行中切换 capture mode（采集模式）。
- 在不提示的情况下自动从双源降级到单源。
- 为系统声音建立第二套 ASR、原文或会议记录。
- Linux、Android 和 iOS 的系统声音采集。

## 3. 用户流程

```text
会议页 -> 开始会议
  -> 选择音频采集模式
       仅麦克风
       仅系统声音
       麦克风 + 系统声音
  -> 按需选择麦克风 / 系统输出设备
  -> 后端同时校验选中的采集源
  -> 开始会议并显示实际采集状态
  -> 实时 ASR 读取混音后音频
  -> 暂停 / 继续 / 停止
  -> 受管 WAV 进入现有会后处理
```

失败时不得静默改写用户选择：

- 开始前系统音频设备不可用：拒绝开始，允许用户改选“仅麦克风”或其他输出设备。
- 双源模式运行中系统音频中断：麦克风继续，会议标记 `systemAudioInterrupted`，不伪装为完整录音。
- 双源模式运行中麦克风中断：系统音频继续，会议标记 `microphoneInterrupted`。
- 两个采集源都中断：进入 recorder interruption（录音中断）状态，但已保存音频和原文不丢失。

## 4. 数据契约

### 4.1 新增类型

```rust
enum MeetingAudioCaptureMode {
    Microphone,
    SystemAudio,
    MicrophoneAndSystem,
}

enum MeetingCaptureSourceState {
    Starting,
    Active,
    Interrupted,
    Stopped,
}

struct MeetingAudioCaptureSnapshot {
    mode: MeetingAudioCaptureMode,
    microphone_device_name: Option<String>,
    system_output_device_id: Option<String>,
    system_output_device_name: Option<String>,
    backend: String,
    microphone_state: Option<MeetingCaptureSourceState>,
    system_audio_state: Option<MeetingCaptureSourceState>,
}
```

要求：

- `MeetingRecord` 保存开始时的 capture snapshot（采集快照），旧会议缺少字段时按 `Microphone` 处理。
- `MeetingRecordingSnapshot` 增加两个音源状态和三路 level（电平）：`microphoneLevel`、`systemAudioLevel`、`mixedLevel`。
- 开始 IPC 只提交 capture mode 和设备 ID，后端必须重新枚举并验证设备，不信任前端自报设备能力。
- 会后 ASR 配置、说话人配置和音频采集配置相互独立。

### 4.2 文件契约

- 现有 `meeting-recordings/<meeting-id>/part-*.wav` 继续作为权威会议音频。
- 每个 part 仍是 16 kHz / mono / 16-bit PCM WAV。
- 暂停 / 继续生成新 part，不在同一 WAV 内改变音源或设备。
- 不保存第二套隐藏双轨文件，避免播放、上传、删除和 retention 同时扩张。
- 任何采集失败都不得删除已完成的 part。

## 5. Windows 采集方案

### 5.1 WASAPI endpoint loopback

- 使用 `IMMDeviceEnumerator` 枚举 active render endpoint（活跃输出端点）。
- 默认选择当前系统输出设备，同时在会议开始配置中允许选择其他 active endpoint。
- 对选中 render endpoint 以 WASAPI shared mode + `AUDCLNT_STREAMFLAGS_LOOPBACK` 创建 capture client。
- 使用 event-driven（事件驱动）采集，不使用高频空轮询。
- 读取端点原生混音格式，在本地转换为 mono f32 frame。
- 正确处理 silent packet（静音包）、buffer discontinuity（缓冲区不连续）、device invalidated（设备失效）和默认设备变更。
- 不使用 process loopback（进程回环）作为首版主路径，避免多进程会议客户端、浏览器子进程和独立通信设备带来的不确定性。

### 5.2 模块边界

建议新增：

```text
src-tauri/src/audio_capture/mod.rs
src-tauri/src/audio_capture/types.rs
src-tauri/src/audio_capture/windows_wasapi_loopback.rs
src-tauri/src/coordinator/meeting_audio_mixer.rs
```

不建议直接在 `meeting.rs` 写 COM / WASAPI 细节。`meeting.rs` 只负责生命周期、状态持久化和事件。

## 6. MeetingAudioMixer 设计

### 6.1 输入和输出

两个采集源输出：

```text
AudioFrame {
  source,
  capturedAtMonotonic,
  sampleRate,
  channels,
  samples,
  discontinuity
}
```

混音器步骤：

1. 每个音源独立下混为 mono。
2. 每个音源独立重采样为 16 kHz。
3. 根据 monotonic timestamp（单调时间戳）写入两个有界 ring buffer（环形缓冲区）。
4. 以 20 ms / 320 samples 固定节拍输出。
5. 某一音源没有对应帧时补静音，不缩短会议时间轴。
6. 通过缓冲区水位估算 clock drift（时钟漂移），只做小幅度自适应重采样，不丢弃大段音频。
7. 使用固定内部 gain（增益）和 soft limiter（软限幅）防止双源相加削波。
8. 输出 i16 PCM 到现有 `AudioConsumer` 和 `WavArchiver`。

### 6.2 约束

- 实时回调中不做文件 IO、COM 枚举或网络请求。
- ring buffer 和通道必须有硬上限；下游阻塞时记录 discontinuity，不无限占用内存。
- 音频流全程保持 PCM，不在实时回调中引入 MP3 / AAC 编码。
- 录音时长、原文 timestamp 和混音 WAV 时间轴必须使用同一会议单调时钟。

## 7. 生命周期

### 7.1 开始

1. 校验 capture mode。
2. 枚举并锁定实际麦克风 / 输出 endpoint。
3. 预创建会议记录，但未确认的采集能力不标记 active。
4. 启动所需的 capture source。
5. 等待每个所需源返回 ready，再启动 mixer、WAV archiver 和实时 ASR。
6. 任一必需源启动失败时回滚本次新建资源，不静默降级。

### 7.2 暂停 / 继续

- 暂停时停止两个采集源和 mixer 输出，finalize（完结）当前 WAV part，并关闭当前实时 ASR session。
- 继续时按开始时快照重新打开设备；设备已失效时明确失败，不切换到其他设备。
- 继续生成新 WAV part 和新 provider session，时间戳继续使用会议 elapsed time（累计有效时长）。

### 7.3 停止

- 先阻止新音频帧进入，再排空有界 mixer buffer，finalize WAV，最后 flush 实时 ASR。
- 系统声音停止失败不得阻止已完成 WAV 和会议记录持久化。
- 会后任务创建、processing hold、云端 / 本地 ASR 动态路由保持当前行为。

## 8. UI 与设置

### 8.1 开始会议配置

- 使用 segmented control（分段控件）选择“麦克风”、“系统声音”、“麦克风 + 系统声音”。
- 只在模式需要时显示麦克风或系统输出设备下拉。
- 系统声音模式首次启用时显示简短隐私提示：录制所选输出设备上播放的所有声音。
- 保留现有会后 ASR、说话人处理和预计人数选项，不把音源选择混入 ASR 下拉。

### 8.2 会议进行中

- 会议胶囊保留当前上下两层布局，不扩大窗口。
- 上层使用麦克风 / 系统声音图标表达激活源，信号轨仍表示 mixed output（混音输出）。
- 主会议页显示两个音源的 active / interrupted（正常 / 中断）状态，但不新增复杂调音台。
- 运行中不允许切换模式；用户可暂停后结束本场，重新开始时改选。

## 9. macOS 后续方案

- 使用 ScreenCaptureKit `SCStream` 和开启音频的 `SCStreamConfiguration`，只处理 audio sample buffer（音频样本缓冲），不保存视频。
- 使用系统支持的 screen/system audio recording（屏幕 / 系统音频录制）权限检查和申请流程。
- 用户拒绝或权限变更后必须给出明确恢复操作，不循环弹出权限窗口。
- macOS 实现复用同一 `AudioFrame`、mixer、状态和 `MeetingRecord` 契约，不复制一套会议业务逻辑。
- Windows 真实会议验收通过前，不同时展开 macOS 平台实现。

## 10. 分阶段实施任务

### Phase 0：数据契约与纯函数混音器

- [ ] 新增 capture mode、capture snapshot 和 source state 类型，为旧会议提供默认值。
- [ ] 实现不依赖真实设备的 `MeetingAudioMixer` 纯核心：时间对齐、静音补齐、混音、限幅和漂移校正。
- [ ] 使用合成正弦波、脉冲和不同样本率覆盖 10 分钟 / 1 小时虚拟时间轴。
- [ ] 不改变当前 `Recorder` 的 microphone-only 输出。

验证：定向 Rust 单元测试；旧会议记录 JSON 反序列化；`cargo test meeting_audio_mixer --lib`。

### Phase 1：Windows WASAPI system-only 采集

- [ ] 枚举输出 endpoint，返回稳定 ID、显示名称和默认标志。
- [ ] 实现 event-driven WASAPI loopback capture source。
- [ ] 实现开始、停止、取消、静音包和设备失效错误分类。
- [ ] 先只接入隔离测试命令，不立即改会议 UI。
- [ ] 用本机无隐私测试音频证明录制 WAV 可播放、时长正确且非全静音。

验证：MSVC `cargo test system_audio_capture --lib`、`cargo check`；Windows 10 / 11 真实输出设备冒烟。

### Phase 2：会议 mixer 与现有录音链路

- [ ] 实现 system-only 会议录音。
- [ ] 实现 microphone + system 双源录音。
- [ ] 只在 mixed output 后调用现有 `AudioConsumer`、ASR 和 `WavArchiver`。
- [ ] 接入暂停、继续、停止、运行时错误和资源清理。
- [ ] 确认仅麦克风路径与当前行为一致。

验证：`cargo test meeting --lib`、WAV 结构 / 时长 / 波形断言、暂停分片和旧会议回归。

### Phase 3：产品 UI 与状态可见性

- [ ] 会议开始配置增加三态 capture mode 和条件设备下拉。
- [ ] 会议中显示麦克风 / 系统声音状态和明确中断错误。
- [ ] 胶囊保持 288 x 82 和上下两层，只增加紧凑音源图标。
- [ ] 五份 i18n（国际化）文案完整。
- [ ] 隐私提示、权限 / 设备失败和恢复操作可执行。

验证：`tsc --noEmit`、`npm run build`、相关 UI contract test（界面契约测试）、受影响页面的隔离无头验证。

### Phase 4：真实桌面会议验收

- [ ] 腾讯会议桌面端：耳机和外放各一次。
- [ ] 飞书会议桌面端：耳机和外放各一次。
- [ ] 微信语音 / 视频通话：耳机和外放各一次。
- [ ] 仅系统声音、仅麦克风、双源各完成实时 ASR 和会后 ASR。
- [ ] 拔出耳机、切换默认输出设备、蓝牙断开、系统静音、暂停 / 继续。
- [ ] 完成 1 小时会议，核对 WAV 时长、双方语音、时间轴和资源占用。

验证：产品人工验收；保留设备、模式、会议 ID、开始 / 停止时间、音频和日志。

### Phase 5：macOS ScreenCaptureKit

- [ ] 完成权限、audio-only stream（仅音频流）和 Rust bridge（Rust 桥接）技术 spike（探针）。
- [ ] 实现 macOS system audio source，复用同一 mixer 和上层契约。
- [ ] 覆盖权限拒绝、权限撤销、设备变更、暂停 / 继续和应用重启。
- [ ] 在 macOS 真机执行最小完整会议闭环。

## 11. 自动测试矩阵

### 纯 mixer

- 单源直通。
- 两个相同脉冲对齐。
- 一个源延迟 50 / 200 / 500 ms。
- 44.1 kHz + 48 kHz 输入混到 16 kHz。
- 一个源断续时用静音补齐，输出时长不缩短。
- 双源满幅输入时 limiter 不溢出。
- 1 小时虚拟时钟漂移后累计偏差不超过 200 ms。
- 通道过载时内存有界，并产生可观测的 discontinuity 计数。

### Windows capture

- endpoint 枚举和默认值。
- 静音 packet、无 packet 窗口和真实音频 packet。
- 设备失效和 COM / WASAPI 错误分类。
- 重复 start / stop 不泄漏 thread、event handle 或 COM 对象。
- 录制完成后 WAV 可重新打开并且 duration 正确。

### 会议回归

- microphone-only 开始、暂停、继续、停止和实时 ASR。
- system-only 不要求麦克风权限。
- 双源任一源运行中中断时另一源继续。
- 开始失败不留下伪 active meeting。
- 暂停后不输出新音频。
- 停止后不再采集系统声音。
- 混音文件继续支持播放、云端 / 本地会后 ASR、总结、导出和删除。

## 12. 人工验收指标

- 仅麦克风与当前版本行为一致。
- 仅系统声音可清晰听到腾讯会议、飞书和微信的对方声音。
- 双源可同时听到本人和对方，不出现持续削波、长时间静音或时间轴明显错位。
- 使用耳机时，对方语音不应因混音链路被人为复制两次。
- 使用外放时，允许存在麦克风拾取对方声音的物理回声，但必须记录该限制，不宣称已做 AEC。
- 1 小时 WAV 时长与有效会议时长差异不超过 1 秒，双源累积对齐偏差不超过 200 ms。
- 系统声音中断不得删除已保存音频、已识别原文或旧总结。
- 停止或退出会议后，不得继续采集或保持音频设备句柄。

## 13. 发布策略

1. 先以隐藏开发开关接入 Windows system-only capture，不改变默认会议行为。
2. system-only 真实设备冒烟通过后再接入双源 mixer。
3. 双源自动测试和三个会议客户端手工验收通过后，才在会议开始 UI 开放。
4. Windows 已稳定后再启动 macOS ScreenCaptureKit 阶段。
5. 不因该功能追溯修改 V1 / V2 已验收范围；V3-2 使用独立 acceptance checklist（验收清单）。

## 14. 工程量估算

- Windows 采集技术 spike：1～2 开发日。
- mixer、漂移处理和纯函数测试：2～3 开发日。
- meeting coordinator、WAV、ASR 和生命周期接入：2～3 开发日。
- UI、设备列表、状态与文案：1～2 开发日。
- Windows 自动回归和三个真实会议客户端验收：2～4 开发 / 测试日。

Windows 首个可人工验收版预计 8～12 开发日；达到可发布候选状态预计 10～14 开发 / 测试日。macOS 平台实现和权限验收另计 5～8 开发 / 测试日。

## 15. 实施前必须再确认的产品决策

1. Windows 首发、macOS 后续的顺序是否确认。
2. 现有用户默认保持“仅麦克风”是否确认。
3. 首版按输出设备录制所有系统声音，不做按应用过滤，是否确认。
4. 首版不实现 AEC，外放重复声音作为已知限制，是否确认。
5. 首版只保留混音后 WAV，不另存麦克风 / 系统声音双轨，是否确认。

## 16. 参考资料

- Microsoft Learn: WASAPI Loopback Recording  
  `https://learn.microsoft.com/windows/win32/coreaudio/loopback-recording`
- Microsoft Windows classic sample: Application loopback audio capture  
  `https://learn.microsoft.com/samples/microsoft/windows-classic-samples/applicationloopbackaudio-sample/`
- Apple ScreenCaptureKit documentation  
  `https://developer.apple.com/documentation/screencapturekit/`
- Apple `SCStreamConfiguration.capturesAudio`  
  `https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturesaudio`
