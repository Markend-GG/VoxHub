# 会议桌宠（Meeting Companion）V1 Acceptance Checklist

状态：active

日期：2026-07-18

## Scope Authority（范围权威）

- 顶层范围以 `docs/meeting-companion-v1-spec.md` 为准。
- 阶段实施以 `docs/meeting-companion-v1-implementation-plan.md` 为准。
- 会议录音业务以 `docs/meeting-recording-v1-spec.md` 和当前 meeting coordinator（会议协调器）为准；桌宠不建立第二套 active session（活跃会话）。
- `docs/future-meeting-recording-notes.md` 只记录未进入本 V1 的后续能力。
- 任何 deferred（延期）必须有用户明确确认；未确认时仍记为 `missing`。

## 状态定义

- `done`：当前工作区有实现证据，且已通过与风险匹配的验证。
- `partial`：只有部分代码、素材或验证，仍不能满足完整验收。
- `missing`：未实现。
- `deferred`：用户已明确确认移出 V1。
- `blocked`：受平台、环境或外部依赖阻塞，已写明原因。

## Checklist

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| MC-V1-001 | 新增可持久化的“会议桌宠”开关，默认关闭。 | Spec 4、5.2、15.1 | done | `UserPreferences` 新增启用、位置锁定和位置字段；`MeetingCompanionSection` 复用现有设置持久化 | Rust 默认值、空配置迁移和序列化往返测试通过；`npm run build` 通过 | 默认关闭；真实窗口重启体验留待 Windows 最终冒烟。 |
| MC-V1-002 | 会议开始后懒创建并显示唯一 `meeting-companion` 透明窗口。 | Spec 5.1、15.1-2 | done | `meeting_companion.rs` 使用独立 label、创建锁和 `?window=meeting-companion` 懒创建；会议开始/停止分别显示和销毁 | 8 线程并发创建 claim 测试通过；Windows Tauri 实测关闭设置不建窗、开启后只建 1 个窗口、重复显示仍唯一、下一场会议重新创建 | 不复用 `capsule`；停止后无额外桌宠 WebView。 |
| MC-V1-003 | 桌宠使用 `?window=meeting-companion` 最小前端路由，不加载完整主窗口 UI。 | Spec 5.1 | done | `main.tsx` / `App.tsx` 路由懒加载 `MeetingCompanion`；生产构建生成独立约 0.75 kB chunk | `npm run build`；Playwright 实测 1 个桌宠根节点、1 张 poster、0 个设置主界面节点，舞台 `350 x 280` | Phase 1 仅显示 `idle` poster，不创建 Tauri 窗口。 |
| MC-V1-004 | 桌宠窗口无边框、透明、置顶、跳过任务栏、不可自由缩放。 | Spec 5.1、15.2 | partial | 独立 builder 固定 `decorations(false)`、`transparent(true)`、`always_on_top(true)`、`skip_taskbar(true)`、`resizable(false)` 和 `350 x 280` | Windows Tauri 捕获实测无边框透明 poster 窗口为 `350 x 280` | 始终置顶、任务栏隐藏和禁止缩放尚未逐项人工操作确认。 |
| MC-V1-005 | 支持左键拖动、16px 边缘吸附、位置记忆和位置锁定。 | Spec 5.2-5.3、15.8 | partial | poster 根节点左键调用精确 drag IPC；后端锁定校验、拖动结束持久化、16px 吸附和 `prefs:changed` 同步已实现 | 单屏边缘吸附、位置锁定和持久化 Rust 测试通过；首次位置实测持久化为工作区右下 16px | Windows 自动化拖动过快，未可靠触发异步原生拖动；真实手动拖动、吸附和锁定仍待用户复测。 |
| MC-V1-006 | 位置恢复覆盖负坐标、DPI 变化、显示器移除和分辨率变化。 | Spec 5.3、15.9 | partial | 保存物理坐标与显示器标识；恢复按当前 work area、scale factor 重新计算并 clamp，显示器缺失回退鼠标/主显示器 | Rust 测试覆盖副屏负坐标、越界 clamp、125%/150% DPI、分辨率变化和显示器移除；单屏 100% Tauri 实测位置恢复为 `(1554,736)` | 当前机器只有单屏 100% DPI，多屏和 125%/150% DPI 尚无真机证据。 |
| MC-V1-007 | 精确导入六个 WebM、六张 poster 和精简 runtime manifest，不导入生成中间件。 | Spec 7、15.11 | done | `openless-all/app/src/assets/meeting-companion/` 仅含 12 个媒体文件和 `manifest.json`；12 个媒体文件与外部交接源 SHA-256 逐一一致 | `npm run check:meeting-companion-assets` 检查精确文件集；`Get-FileHash` 对比结果 12/12 一致 | 未导入 raw frames、GIF、atlas、生成目录或 QA 文件。 |
| MC-V1-008 | 素材校验覆盖 VP9、Alpha、`350 x 280`、无音轨、循环方式和六个实际时长。 | Spec 7.2、15.11 | done | `scripts/check-meeting-companion-assets.mjs` 按 spec 固定值解码并校验六状态 | `npm run check:meeting-companion-assets`；实际时长 `2416/916/2000/2416/1166/2916ms`，误差均小于 1 帧（84ms） | `idle` / `completed` 不循环，其余四状态循环。 |
| MC-V1-009 | 建立六状态桌宠 reducer，以后端 meeting snapshot 为权威状态。 | Spec 6、9、15.3-4 | done | `meetingCompanionState.ts` 覆盖 hidden、六视觉状态和错误叠加层 | `npm run check:meeting-companion-state`；覆盖新会议、过期 meeting id、后端 snapshot 覆盖本地状态及总结成功/失败 | quiet 只接收布尔视觉信号；RMS 滞回计算仍属 Phase 4。 |
| MC-V1-010 | recorder RMS 以最多 10Hz 发送 `meeting:audio-level`，不阻塞音频回调。 | Spec 6.3 | missing | - | Rust 事件节流测试；`cargo check` | 只服务视觉状态。 |
| MC-V1-011 | `recording` / `quiet` 使用 RMS 滞回阈值稳定切换，不使用 ASR 原文停顿。 | Spec 6.3、15.3 | missing | - | 阈值、时间窗和抖动测试 | 进入 0.035/2000ms；退出 0.06/150ms。 |
| MC-V1-012 | 悬停 / 单击控件可暂停、继续、停止，命令期间防重入并丢弃过期结果。 | Spec 5.4、15.4 | missing | - | 组件测试；双窗口人工操作 | 复用现有 meeting IPC。 |
| MC-V1-013 | 停止操作有二次确认，暂停 / 继续不需确认。 | Spec 4、5.4、15.5 | missing | - | 组件测试；Windows 人工验证 | 键盘焦点必须可用。 |
| MC-V1-014 | 右键菜单提供状态相关操作、隐藏和位置锁定。 | Spec 5.5、15.8 | missing | - | 菜单状态测试；Windows 人工验证 | 右键是辅助入口。 |
| MC-V1-015 | 隐藏、桌宠窗口关闭和桌宠失败均不停止录音；主页面可重新显示。 | Spec 9-10、15.5、15.13-14 | done | close request 转 `hide`；当前会议记录手动隐藏抑制；主会议页提供精确 show IPC；窗口失败只记录 warning，不回滚会议命令 | Windows Tauri 实测 Alt+F4 后录音继续、等待后不被会议事件重弹、主页面可恢复、重复恢复仍唯一；停止后销毁且下一场重新自动显示 | 主窗口退出仍复用现有 close guard。 |
| MC-V1-016 | 停止后正确显示 `processing`；成功后播放 `completed` 并在 3 秒后隐藏。 | Spec 4、6、15.6 | missing | - | summary success 人工验证 | completed 不循环。 |
| MC-V1-017 | 总结失败不播放 `completed`，显示错误徽标和打开会议页入口。 | Spec 4、11、15.7 | missing | - | summary failure 人工验证 | 不在桌宠内重试总结。 |
| MC-V1-018 | 粉色计时器显示后端权威已录时长，暂停冻结，停止后保留最终时长。 | Spec 8、15.10 | done | `MeetingCompanion` 接收显式 `timerInput`；`MeetingCompanionElapsedClock` 以 `elapsedMs` 对齐基线，仅用 `performance.now()` 插值显示 | fake clock 覆盖运行、暂停、停止、新 snapshot 重对齐和 meeting id 切换；格式覆盖 `00:00`、`59:59`、`01:00:00`和 `02:04:05`；Playwright 放大截图确认文字位于粉色计时器屏幕内 | Phase 3 不订阅会议事件；后续由 Phase 4 将后端 snapshot 传入该显式接口。 |
| MC-V1-019 | WebM 加载前显示 poster；视频失败回退 poster；poster 失败回退最小状态 UI。 | Spec 7.2、11 | done | runtime manifest 驱动当前状态媒体；单媒体控制器实现 2 秒超时、error、play reject、运行中错误和过期结果隔离 | `npm run check:meeting-companion-runtime`；Playwright 阻断 WebM 时保留 1 张 poster，同时阻断 WebM/poster 时显示含 `00:00` 的最小 UI | 媒体失败仅更改桌宠展示状态，不调用会议 IPC。 |
| MC-V1-020 | 同时只解码一个视频；桌宠隐藏后停止解码、timer 和音量刷新。 | Spec 7.2、12、15.12 | done | DOM 只挂载当前状态 video；切换/隐藏/卸载统一执行 pause、`currentTime = 0`、移除 `src` 和 `load()`，并清理 timeout/interval/listener | 定向测试覆盖快速切换、单活动视频和卸载释放；Playwright 正常路径始终 1 个 video，页面不可见后降为 0 个且 poster 恢复 | Phase 4 尚未引入音量事件，因此当前无额外音量刷新需停止。 |
| MC-V1-021 | reduced motion 显示 poster；图标按钮有 tooltip、键盘焦点和可访问名称。 | Spec 5.4、7.2、13 | partial | `prefers-reduced-motion: reduce` 和页面不可见均不创建活跃 video，只保留 poster | 定向逻辑测试；Playwright reduced motion 路径为 0 个 video、1 张 poster | 图标按钮、tooltip、键盘焦点和可访问名称属于 Phase 5，本阶段不得标记 done。 |
| MC-V1-022 | 中文、英文、日文、韩文、繁体中文文案键完整。 | 项目 UI 回归要求 | missing | - | i18n（国际化）key 扫描；build | 用户可见文案不硬编码。 |
| MC-V1-023 | TypeScript、Rust、状态机、RMS 滞回、位置恢复和素材校验测试通过。 | Spec 15.15 | missing | - | 按计划中的自动命令 | - |
| MC-V1-024 | Windows 真实桌面全流程、多屏、DPI、透明边缘和隐藏不停录音验收通过。 | Spec 16 | missing | - | Tauri dev 人工冒烟 | V1 最终门槛。 |

## Deferred Log（延期记录）

当前无 deferred 条目。

## Final Acceptance Gate（最终验收门槛）

- MC-V1-001 到 MC-V1-024 必须全部为 `done` 或用户明确确认的 `deferred`。
- MC-V1-008 素材实际时长校验未通过前，不得进入窗口 UI 接入。
- MC-V1-024 Windows 真实桌面验收未通过前，不得声称会议桌宠 V1 已完成。
- 人工验收缺口必须在交付中如实列出，不能用自动测试代替。
