# 会议桌宠（Meeting Companion）V1 Acceptance Checklist

状态：active

日期：2026-07-19

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
| MC-V1-001 | 新增可持久化的“会议桌宠”开关，默认关闭。 | Spec 4、5.2、15.1 | done | `UserPreferences` 新增启用、位置锁定和位置字段；`MeetingCompanionSection` 复用现有设置持久化 | Rust 默认值、空配置迁移和序列化往返测试通过；Windows Tauri 实测默认关闭、临时开启后重启仍保持，测试后已恢复关闭 | 当前用户设置已恢复原值。 |
| MC-V1-002 | 会议开始后懒创建并显示唯一 `meeting-companion` 透明窗口。 | Spec 5.1、15.1-2 | done | `meeting_companion.rs` 使用独立 label、创建锁和 `?window=meeting-companion` 懒创建；会议开始/停止分别显示和销毁 | 8 线程并发创建 claim 测试通过；Windows Tauri 实测关闭设置不建窗、开启后只建 1 个窗口、重复显示仍唯一、下一场会议重新创建 | 不复用 `capsule`；停止后无额外桌宠 WebView。 |
| MC-V1-003 | 桌宠使用 `?window=meeting-companion` 最小前端路由，不加载完整主窗口 UI。 | Spec 5.1 | done | `main.tsx` / `App.tsx` 路由懒加载 `MeetingCompanion`；生产构建生成独立约 0.75 kB chunk | `npm run build`；Playwright 实测 1 个桌宠根节点、1 张 poster、0 个设置主界面节点，舞台 `350 x 280` | Phase 1 仅显示 `idle` poster，不创建 Tauri 窗口。 |
| MC-V1-004 | 桌宠窗口无边框、透明、置顶、跳过任务栏、不可自由缩放。 | Spec 5.1、15.2 | done | 独立 builder 固定 `decorations(false)`、`transparent(true)`、`always_on_top(true)`、`skip_taskbar(true)`、`resizable(false)` 和 `350 x 324`，其中角色舞台为 `350 x 280` | Windows Tauri 实测背景透明且控制区未裁切；Win32 外框与客户区同为 `350 x 324` 且原点一致，确认无边框；`WS_EX_TOPMOST` 存在且无 `WS_THICKFRAME`；任务栏 UI Automation 仅显示 `OpenLess - 1 个运行窗口`，未出现桌宠任务 | 五项原生窗口属性均有 Windows 真实窗口证据。 |
| MC-V1-005 | 支持左键拖动、16px 边缘吸附、位置记忆和位置锁定。 | Spec 5.2-5.3、15.8 | partial | 左键移动达到 3px 才调用精确 drag IPC，普通单击只显示控件且不写位置；后端锁定校验、拖动结束持久化、16px 吸附和 `prefs:changed` 同步已实现 | controls 测试覆盖主键、移动阈值和普通单击；Rust 测试覆盖四边吸附、位置锁定和持久化；Windows Computer Use 真实拖动将窗口从 `(1554,692)` 移到 `(1498,687)`，并确认右 / 下边缘落在 `(1570,708)`；锁定后同一拖动手势前后均为 `(1570,708)`；隐藏 / 主页面恢复保持同一窗口 ID 和位置，应用重启后新窗口仍恢复到 `(1570,708)` | 真实拖动、右 / 下吸附、锁定、隐藏恢复和跨重启恢复已有证据；Computer Use 在原生窗口移动时会重映射窗口相对坐标，无法稳定把窗口送到左 / 上边缘，故四边吸附尚未全部真机确认。 |
| MC-V1-006 | 位置恢复覆盖负坐标、DPI 变化、显示器移除和分辨率变化。 | Spec 5.3、15.9 | partial | 保存物理坐标与显示器标识；恢复按当前 work area、scale factor 重新计算并 clamp，显示器缺失回退鼠标/主显示器 | Rust 测试覆盖副屏负坐标、越界 clamp、125%/150% DPI、分辨率变化和显示器移除；单屏 100% Tauri 在 `1920 x 1032` 工作区内将 `DISPLAY5` 的 `(1570,708)` 持久化，重启后恢复同一坐标且完整可见 | 当前机器只有单屏 100% DPI，多屏负坐标、显示器移除、125% / 150% DPI 和分辨率变化尚无真机证据。 |
| MC-V1-007 | 精确导入六个 WebM、六张 poster 和精简 runtime manifest，不导入生成中间件。 | Spec 7、15.11 | done | `openless-all/app/src/assets/meeting-companion/` 仅含 12 个媒体文件和 `manifest.json`；12 个媒体文件与外部交接源 SHA-256 逐一一致 | `npm run check:meeting-companion-assets` 检查精确文件集；`Get-FileHash` 对比结果 12/12 一致 | 未导入 raw frames、GIF、atlas、生成目录或 QA 文件。 |
| MC-V1-008 | 素材校验覆盖 VP9、Alpha、`350 x 280`、无音轨、循环方式和六个实际时长。 | Spec 7.2、15.11 | done | `scripts/check-meeting-companion-assets.mjs` 按 spec 固定值解码并校验六状态 | `npm run check:meeting-companion-assets`；实际时长 `2416/916/2000/2416/1166/2916ms`，误差均小于 1 帧（84ms） | `idle` / `completed` 不循环，其余四状态循环。 |
| MC-V1-009 | 建立六状态桌宠 reducer，以后端 meeting snapshot 为权威状态。 | Spec 6、9、15.3-4 | done | `meetingCompanionState.ts` 覆盖 hidden、六视觉状态和错误叠加层 | `npm run check:meeting-companion-state`；覆盖新会议、过期 meeting id、后端 snapshot 覆盖本地状态及总结成功/失败 | quiet 只接收布尔视觉信号；RMS 滞回计算仍属 Phase 4。 |
| MC-V1-010 | recorder RMS 以最多 10Hz 发送 `meeting:audio-level`，不阻塞音频回调。 | Spec 6.3 | done | recorder 回调只向容量 1 的内存队列执行 `try_send`；独立 reporter 线程按 100ms 节流并发送带 `meetingId` 的归一化 level；暂停、停止、无当前会议或桌宠隐藏时禁止发送 | Rust 测试覆盖 10Hz、level clamp、满队列非阻塞和发送条件；`cargo test meeting_companion --lib` 18/18 通过；MSVC `cargo check` 通过 | 回调不等待 WebView，不读写磁盘；事件只服务视觉状态。 |
| MC-V1-011 | `recording` / `quiet` 使用 RMS 滞回阈值稳定切换，不使用 ASR 原文停顿。 | Spec 6.3、15.3 | done | `MeetingCompanionQuietDetector` 使用 `<0.035` 连续 2000ms 进入、`>=0.06` 连续 150ms 退出，中间区间保持；暂停、停止、隐藏和会议切换统一复位 | `npm run check:meeting-companion-sync` 覆盖阈值、抖动、复位和旧会议 level；Windows Tauri 在虚拟输入 RMS 接近 0 时实测约 2 秒进入 `quiet` | 当前虚拟输入设备无法稳定制造高 RMS，退出 quiet 的时序由确定性测试验证。 |
| MC-V1-012 | 悬停 / 单击控件可暂停、继续、停止，命令期间防重入并丢弃过期结果。 | Spec 5.4、15.4 | done | 独立 44px 控制区按状态映射暂停 / 继续 / 停止；复用原 meeting IPC；`MeetingCompanionCommandGate` 防重入并校验 generation、当前 meeting id 和返回 meeting id，失败后重读权威 snapshot | `npm run check:meeting-companion-controls` 覆盖重复点击、旧结果、会议切换、失败恢复和停止失败；Windows Tauri 实测桌宠暂停 / 继续 / 停止及主页面继续后桌宠同步 | 命令执行时按钮禁用；桌宠仍是非权威展示层。 |
| MC-V1-013 | 停止操作有二次确认，暂停 / 继续不需确认。 | Spec 4、5.4、15.5 | done | 使用窗口内 `role=dialog` 自定义确认框，不调用系统 confirm；确认期间禁用重复提交，Escape 可取消，Tab / Shift+Tab 在可用按钮间循环 | controls 契约测试通过；Windows Tauri 实测 Escape 取消、Tab 焦点环和 Enter 确认停止，暂停 / 继续直接执行 | 停止确认忙碌期间不可关闭或重复确认。 |
| MC-V1-014 | 右键菜单提供状态相关操作、隐藏和位置锁定。 | Spec 5.5、15.8 | done | 自定义菜单按 recording / quiet、paused、processing / completed、summary_failed 映射操作，并始终提供隐藏和位置锁定；菜单位置按视口 clamp | `npm run check:meeting-companion-controls` 覆盖全部状态映射和越界约束；Windows Tauri 实测 recording / paused 菜单、Escape 关闭、隐藏、锁定 / 解锁及菜单不越界 | 右键是辅助入口；直接控制仍为主要路径。 |
| MC-V1-015 | 隐藏、桌宠窗口关闭和桌宠失败均不停止录音；主页面可重新显示。 | Spec 9-10、15.5、15.13-14 | done | close request 转 `hide`；当前会议记录手动隐藏抑制；主会议页提供精确 show IPC；原生窗口显示时向桌宠发送当前 meeting id 的 `meeting-companion:show`，清除本场前端隐藏状态 | Windows Tauri 实测 Alt+F4 和右键隐藏后录音继续、等待后不重弹；主页面恢复后角色与计时器完整显示且窗口仍唯一；停止后销毁且下一场重新自动显示 | 本轮发现并修复“右键隐藏后恢复为空窗口”；主窗口退出仍复用现有 close guard。 |
| MC-V1-016 | 停止后正确显示 `processing`；成功后播放 `completed` 并在 3 秒后隐藏。 | Spec 4、6、15.6 | done | stop 接受后立即广播 `phase=stopping`；桌宠按 summary 事件切换 `processing` / `completed`；视频 `ended` 或 poster 回退后再完整停留 3 秒并调用精确销毁 IPC，另有 9 秒后端兜底 | `npm run check:meeting-companion-sync` 覆盖慢加载不占用 3 秒停留时间和过期 timer 清理；Windows Tauri 实测 `2202ms` 为 processing、`3293ms` 播放 completed、`4412ms` 到达最终帧、`7446ms` 最终帧仍可见、`11706ms` 窗口已销毁 | 最终帧至少完整停留 `3034ms`；停止不再立即销毁桌宠。 |
| MC-V1-017 | 总结失败不播放 `completed`，显示错误徽标和打开会议页入口。 | Spec 4、11、15.7 | partial | `summary_failed` 保持 processing poster、停止视频并显示红色错误徽标；直接控件和右键菜单提供“打开会议”；精确 IPC 只允许桌宠调用，并让主窗口切到会议页、选择对应 meeting id | reducer / sync 测试覆盖失败不进入 completed；controls 测试覆盖失败态入口；Rust 测试覆盖调用窗口限制和隐藏失败清理；Windows Tauri 使用约 43.7 秒 sherpa 真实会议和临时不可达 LLM endpoint 成功进入 `summary_failed`，主会议页显示总结失败且原文、音频均保留 | ORT API 冲突已经修复，暂停、继续和停止不再崩溃；本轮未从桌宠失败态点击“打开会议”并核对对应 meeting id，因此完整跳转仍缺真机证据，保持 `partial`。 |
| MC-V1-018 | 粉色计时器显示后端权威已录时长，暂停冻结，停止后保留最终时长。 | Spec 8、15.10 | done | Connected companion 将真实 `MeetingRecordingSnapshot.elapsedMs` 接入 `MeetingCompanionElapsedClock`，仅在可录音 phase 本地插值 | fake clock 覆盖运行、暂停、停止、重对齐和会议切换；Windows Tauri 实测暂停后 `01:43` 保持 3.5 秒不变，继续后增长至 `01:57`，停止整理与完成阶段保留最终时长 | 后端 snapshot 是权威基线，本地时钟只负责两次 snapshot 之间的显示。 |
| MC-V1-019 | WebM 加载前显示 poster；视频失败回退 poster；poster 失败回退最小状态 UI。 | Spec 7.2、11 | done | runtime manifest 驱动当前状态媒体；单媒体控制器实现 2 秒超时、error、play reject、运行中错误和过期结果隔离 | `npm run check:meeting-companion-runtime`；Playwright 阻断 WebM 时保留 1 张 poster，同时阻断 WebM/poster 时显示含 `00:00` 的最小 UI | 媒体失败仅更改桌宠展示状态，不调用会议 IPC。 |
| MC-V1-020 | 同时只解码一个视频；桌宠隐藏后停止解码、timer 和音量刷新。 | Spec 7.2、12、15.12 | done | DOM 只挂载当前状态 video；隐藏/卸载停止媒体和 timer、逐项反注册会议监听并复位 quiet；后端 visibility flag 同步禁止 `meeting:audio-level` | runtime / sync 定向测试覆盖快速切换、单活动视频、卸载释放和 timer 清理；Windows Tauri 隐藏后未重弹且录音继续，完成后无遗留桌宠 WebView | 音量回调线程只在桌宠可见且当前会议处于 recording 时发送。 |
| MC-V1-021 | reduced motion 显示 poster；图标按钮有 tooltip、键盘焦点和可访问名称。 | Spec 5.4、7.2、13 | done | `prefers-reduced-motion: reduce` 和页面不可见均只保留 poster；控制按钮使用 lucide 图标、原生 button、`title`、`aria-label` 和 `.ol-focus-ring`；确认框具备语义和焦点循环 | runtime 定向测试与 Playwright reduced motion 路径通过；controls 契约测试通过；Windows Tauri 实测 Tab 焦点环、Escape 和 Enter | 控件使用固定尺寸，显示 / 隐藏不改变窗口舞台位置。 |
| MC-V1-022 | 中文、英文、日文、韩文、繁体中文文案键完整。 | 项目 UI 回归要求 | done | 五份 locale 均新增相同的 `meetingCompanion` 控制、确认、菜单和错误文案；用户可见文本只通过 i18n key 获取 | `npm run check:meeting-companion-controls` 逐键扫描五种语言；`tsc --noEmit` 和 `npm run build` 通过 | 无硬编码的用户可见控制文案。 |
| MC-V1-023 | TypeScript、Rust、状态机、RMS 滞回、位置恢复和素材校验测试通过。 | Spec 15.15 | done | 五个会议桌宠 npm 测试覆盖素材、状态、runtime、同步和控制；Phase 6 新增普通单击 / 拖动阈值回归；Rust 覆盖位置、唯一窗口、RMS、长录音分片和更新清单校验 | 五个 npm 脚本、`tsc --noEmit`、`npm run build` 全通过；MSVC Rust 全量库测试 `781 passed / 0 failed / 1 ignored`，已安装 sherpa 模型真实推理测试通过，`cargo check` 0 warning；`cargo fmt --all -- --check` 和 `git diff --check` 通过 | Phase 6 后续稳定性修复已完成全量自动回归；没有用自动测试替代多屏 / DPI 真机证据。 |
| MC-V1-024 | Windows 真实桌面全流程、多屏、DPI、透明边缘和隐藏不停录音验收通过。 | Spec 16 | partial | Windows Tauri 已验证默认关闭 / 重启保持、唯一窗口、六状态、quiet、计时器暂停冻结、主页面同步、确认框焦点、右键菜单、锁定同步、隐藏 / 关闭不停录音、主页面恢复、ASR 中断警告、processing / completed / 3 秒停留 / 销毁和下一场重建；Phase 6 补齐五项窗口原生属性、普通单击不误拖、真实拖动、锁定、右 / 下吸附、隐藏恢复和跨重启位置恢复 | 单屏 100% DPI 真机流程；浅色主界面、灰色遮罩和深色主界面逐项观察均无矩形底色、明显白边 / 黑边或控件重叠；可见 / 隐藏各 5 秒进程采样均为 10 个进程，私有内存约 `542.4 / 541.4 MB` 且采样期间无增长，可见 / 隐藏 CPU 约 `13.1% / 9.1%`，未发现明显泄漏；ORT 修复后 Windows Tauri 已复验 sherpa 暂停、继续、停止和 `summary_failed`，应用保持运行 | 仍缺左 / 上吸附、多屏负坐标、显示器移除、125% / 150% DPI、分辨率变化和从桌宠失败态“打开会议”的完整跳转，故不能作为 V1 最终完成门槛。 |

## Phase 6 Final Audit（最终审计）

- 状态统计：`done 20`、`partial 4`、`missing 0`、`deferred 0`、`blocked 0`。
- 仍为 `partial`：MC-V1-005、MC-V1-006、MC-V1-017、MC-V1-024。
- 自动验证：全部会议桌宠 npm 测试、TypeScript、生产构建、Rust 定向测试、Rust 全量库测试、`cargo check`、runtime 严格文件集、五语言 key 和 `git diff --check` 均通过。
- Windows 补充证据：Computer Use 已完成真实拖动、右 / 下吸附、锁定、隐藏 / 恢复、跨重启位置恢复，以及浅色 / 灰色 / 深色背景透明边缘检查；测试后已恢复桌宠关闭、位置解锁、主题跟随系统和主窗口原尺寸。
- 范围审计：Phase 6 原始代码 diff 仅含会议桌宠拖动判定及其测试；后续独立稳定性任务修复了 ORT 依赖冲突、sherpa 长录音分片、重新转写流式读取、更新检查、构建 warning 和格式基线，未改变桌宠产品范围。
- Windows 环境风险：ORT API 冲突已通过依赖收敛修复，Windows Tauri 暂停、继续和停止复验不再崩溃；长录音已改为固定分片，但真实两小时进程内存曲线仍待补充。
- 已知问题与下次修复入口：`docs/meeting-companion-v1-phase6-known-issues.md`。
- 结论：因四项验收仍为 `partial`，会议桌宠 V1 当前不能声明完成。

## Deferred Log（延期记录）

当前无 deferred 条目。

## Final Acceptance Gate（最终验收门槛）

- MC-V1-001 到 MC-V1-024 必须全部为 `done` 或用户明确确认的 `deferred`。
- MC-V1-008 素材实际时长校验未通过前，不得进入窗口 UI 接入。
- MC-V1-024 Windows 真实桌面验收未通过前，不得声称会议桌宠 V1 已完成。
- 人工验收缺口必须在交付中如实列出，不能用自动测试代替。
