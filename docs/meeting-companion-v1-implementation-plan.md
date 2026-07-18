# 会议桌宠（Meeting Companion）V1 Implementation Plan

状态：ready for target-mode implementation

日期：2026-07-18

基线：当前分支，执行前必须重新检查 `git status --short`

关联文档：

- `docs/meeting-companion-v1-spec.md`
- `docs/meeting-companion-v1-acceptance-checklist.md`
- `docs/meeting-recording-v1-spec.md`
- `docs/future-meeting-recording-notes.md`

## 1. Goal（目标）

在不改变会议录音、ASR（语音转文字）和总结主链路的前提下，新增 Windows 会议桌宠透明悬浮窗口，提供六状态动画、真实计时、暂停 / 继续 / 停止控制、拖动吸附和安全隐藏。

## 2. Architecture（架构）

```text
Existing meeting page / meeting coordinator
  -> existing meeting commands and events
  -> authoritative MeetingRecordingSnapshot
  -> meeting-companion window lifecycle
  -> companion state reducer
  -> one active WebM or poster fallback
  -> hover controls / context menu / timer overlay
```

关键边界：

- 后端 meeting coordinator（会议协调器）仍是 active meeting（活跃会议）的唯一权威。
- 桌宠调用现有 pause / resume / stop IPC（进程间调用），不复制录音管线。
- 新增 Rust 部分限于窗口生命周期、位置、设置和节流音量事件。
- 素材是构建时静态资源，不在运行时下载。

## 3. 不要改动项

- 不改 ASR provider、realtime draft / final、speaker diarization（说话人分离）或总结 prompt（提示词）。
- 不改会议音频保留、回放、重新转写、缓存或删除策略。
- 不复用或重构当前 `capsule`、QA 窗口或 less-computer 窗口的业务行为。
- 不格式化或重构与桌宠无关的 `lib.rs`、`FloatingShell.tsx` 或会议页。
- 不将外部素材目录、生成中间件、GIF、QA 联系表或 atlas 整体复制进仓库。

## 4. Phase 0 - 素材导入门槛

### 实施

1. 从 `C:\Users\25512\Pictures\素材目录\animation-run` 读取当前 `manifest.json`、六个状态 WebM 和 poster。
2. 新增一个可重复执行的素材校验脚本，检查：
   - 状态 id 齐全且无多余文件。
   - WebM 为 VP9 Alpha、`350 x 280`、无音轨。
   - poster 为 `350 x 280` RGBA PNG。
   - 实际 duration 与 spec 的六个时序容差一致。
3. 如 WebM 仍是固定 12 帧 / 12 FPS 导致时长全为 1 秒，先根据 `duration_ms` 复制帧或生成变时长时序并重新编码。
4. 精确复制运行时文件到建议目录 `openless-all/app/src/assets/meeting-companion/`。
5. 生成精简 runtime manifest，只包含 state、webm、poster、loop 和 durationMs。

### 验证

- 素材校验脚本通过。
- `git diff --stat` 不包含 raw frames（原始帧）、GIF、atlas、`generated/`、`work/`或 `qa/`。
- checklist：MC-V1-007、MC-V1-008 -> `done`。

### 停止条件

本阶段未通过时停止，不开始窗口 UI 实施。

## 5. Phase 1 - 设置、状态模型与最小路由

### 实施

1. 在现有设置数据中增加：
   - `meetingCompanionEnabled: boolean`。
   - `meetingCompanionPositionLocked: boolean`。
   - 桌宠位置和显示器定位信息。
2. 在 `SettingsModal` 现有分类中新增小范围“会议桌宠”设置区，复用 `SwitchLite`。
3. 新增 `MeetingCompanionState` 类型和纯 reducer（状态归约器），覆盖 spec 状态表和错误叠加层。
4. 在 `App.tsx` 为 `?window=meeting-companion` 增加独立 lazy route（懒加载路由）。
5. 新建最小 `MeetingCompanion` 组件，此阶段先使用 poster 显示状态。

### 验证

- 设置默认值、序列化和迁移测试通过。
- 状态 reducer 表驱动测试通过。
- `tsc --noEmit` 通过。
- checklist：MC-V1-001、MC-V1-003、MC-V1-009 -> `done`。

## 6. Phase 2 - 窗口生命周期与位置

### 实施

1. 参考当前 lazy window（懒创建窗口）模式，在 desktop-only（仅桌面端）Rust 代码中实现 `ensure_meeting_companion_window`。
2. 窗口使用 `index.html?window=meeting-companion`，无边框、透明、置顶、跳过任务栏、不可缩放。
3. 新增精确的 show / hide / position command（显示 / 隐藏 / 位置命令），不暴露任意窗口 label。
4. 实现默认位置、边缘吸附、work area clamp（工作区边界约束）和拖动结束持久化。
5. 拦截桌宠窗口 close request（关闭请求）并转为 hide。
6. 会议开始且设置启用时 show；主会议页提供“显示会议助手”恢复入口。

### 验证

- Rust 纯函数测试覆盖负坐标、显示器移除、DPI 和吸附边界。
- Windows 单屏与多屏人工拖动验证。
- 关闭 / 隐藏桌宠后会议仍在录音。
- checklist：MC-V1-002、MC-V1-004、MC-V1-005、MC-V1-006、MC-V1-015 -> `done`。

## 7. Phase 3 - 媒体播放、计时器和回退

### 实施

1. 实现单一媒体播放器：每次只 mount（挂载）当前状态 WebM。
2. 状态切换时暂停旧视频、重置时间、切换 poster 并启动新视频。
3. 实现 2 秒 `canplay` 超时、media error（媒体错误）和播放拒绝的 poster 回退。
4. 实现 poster 失败时的最小状态胶囊。
5. 在粉色计时器屏幕上叠加 `elapsedMs`，覆盖暂停冻结和停止后最终时长。
6. 实现 reduced motion -> poster 路径。

### 验证

- 媒体组件测试覆盖加载、超时、错误、切换和卸载。
- 只有一个 video 元素处于活跃解码状态。
- 视频隐藏后不继续播放。
- checklist：MC-V1-018、MC-V1-019、MC-V1-020 -> `done`；MC-V1-021 -> `partial`，等待 Phase 5 补齐键盘和可访问名称。

## 8. Phase 4 - 会议同步与 quiet 检测

### 实施

1. 桌宠加载后调用 `getActiveMeetingRecording()`，然后订阅 `meeting:state`、`meeting:summary`、`meeting:error`。
2. recorder 复用已有 RMS，录音时以最多 10Hz 发送 `meeting:audio-level`；暂停、停止和无活跃会议时停止发送。
3. 前端实现 quiet hysteresis（安静滞回）并复位逻辑。
4. 实现 `idle -> recording/quiet -> paused -> processing -> completed` 的状态切换。
5. `transcribing_interrupted` 保持录音 / 安静动画并叠加警告。

### 验证

- Rust 节流频率测试不超过 10Hz。
- TypeScript 测试覆盖阈值附近抖动、暂停复位、会议 id 切换和过期事件。
- 音频回调不等待 WebView 或磁盘写入。
- checklist：MC-V1-010、MC-V1-011、MC-V1-016 -> `done`；MC-V1-017 -> `partial`，等待 Phase 5 完成失败 UI 和打开会议页命令。

## 9. Phase 5 - 控制、右键菜单与错误边界

### 实施

1. 实现悬停 / 单击控制区和 800ms 收起延迟。
2. 复用现有 `pauseMeetingRecording`、`resumeMeetingRecording`、`stopMeetingRecording`。
3. 命令执行期间禁用重复点击，结果必须校验 meeting id 和最新 snapshot。
4. 实现停止二次确认，覆盖 `Escape`、焦点锁定和忙碌状态。
5. 实现状态相关的自定义右键菜单。
6. 实现命令失败、ASR 中断、summary failed（总结失败）和打开会议页。
7. 补齐五份 i18n（国际化）文案和无障碍名称。

### 验证

- 组件测试覆盖控件显示、禁用、确认、错误恢复和右键菜单。
- 键盘可完成暂停 / 继续 / 停止确认。
- 所有用户可见文案都来自 i18n key。
- checklist：MC-V1-012、MC-V1-013、MC-V1-014、MC-V1-017、MC-V1-021、MC-V1-022 -> `done`。

## 10. Phase 6 - 自动验证与 Windows 真机验收

### 自动验证

在 `openless-all/app/` 下：

```powershell
.\node_modules\.bin\tsc.CMD --noEmit
npm run build
```

如新增了独立前端测试脚本，执行该定向测试。

在 Windows MSVC（微软 C++ 工具链）环境下：

```powershell
cmd /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 >nul && set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc&& set CARGO_TARGET_DIR=D:\openless-deps\cargo-target-openless-msvc&& set TMP=D:\openless-deps\tmp&& set TEMP=D:\openless-deps\tmp&& cd /d D:\codex项目\VOXHUB\openless-all\app\src-tauri&& cargo check"
```

运行桌宠相关 Rust 定向测试，并确保现有 meeting 测试不回归。

### Windows 人工验收

1. 桌宠开关默认关闭，开启后重启仍保留。
2. 开始会议，桌宠显示并按实际音量切换录音 / 安静动画。
3. 暂停、继续、停止二次确认与主页面状态一致。
4. 隐藏和桌宠窗口关闭不停止录音，主页面可重新显示。
5. 停止后显示整理中，总结成功后播放完成动画并隐藏。
6. 制造总结失败，确认不播放完成动画且可打开会议页。
7. 检查浅色 / 深色桌面上的透明边缘、黑边、白边和角色跳动。
8. 拖动、边缘吸附、位置锁定、副屏负坐标、125% / 150% DPI 和显示器拔插。
9. 隐藏桌宠后检查无后台视频播放和持续音量事件。
10. 主窗口关闭和托盘退出仍经过活跃会议保护。

### 最终验收

- 检查 `docs/meeting-companion-v1-acceptance-checklist.md` 每一项状态。
- checklist：MC-V1-023、MC-V1-024 -> `done`。
- 查看最终 `git diff --stat` 和任务相关 diff，确认无生成缓存、日志、原始帧或无关重构。

## 11. 建议的最小 Commit（提交）分段

仅在用户允许 commit 且每阶段通过匹配风险的验证后执行：

1. `feat: add meeting companion assets and state model`
2. `feat: add meeting companion window lifecycle`
3. `feat: connect meeting companion controls and animations`
4. `test: verify meeting companion desktop behavior`

每次只 stage（暂存）当前阶段直接相关文件，不使用 `git add .`，不提交日志或外部生成目录。

## 12. 主要风险与回退

- 透明 WebM 在 WebView2 的实际 Alpha 解码失败：回退 poster，不阻塞会议。
- 窗口位置恢复落在屏幕外：忽略旧位置并回到鼠标所在显示器右下角。
- RMS 导致动画频繁切换：优先调整滞回阈值和时间窗，不修改 ASR / 录音分段。
- 独立 WebView 内存高：确认懒创建、隐藏时停止解码；V1 不引入新动画引擎。
- 桌宠命令失败或前端崩溃：主会议页和后端会议继续，桌宠只是非权威展示层。
