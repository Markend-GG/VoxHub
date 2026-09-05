# 会议录音棚信号条 V1 Acceptance Checklist

状态：active

日期：2026-08-07

更新：2026-08-14 胶囊调整为 `288 x 82` 上下两层布局，时长与操作移到音量条下方。

## 状态定义

- `done`：实现与匹配风险的验证均完成。
- `partial`：已实现但仍缺验证，或仅完成部分路径。
- `missing`：尚未实现。
- `blocked`：被环境或外部条件阻塞。

## Checklist

| ID | Requirement（需求） | Status（状态） | Evidence（证据） | Verification（验证） |
| --- | --- | --- | --- | --- |
| MSR-V1-001 | 旧桌宠规格明确被信号条规格替代，现有媒体资源不删除。 | done | 新 spec / plan / checklist；旧文档标记 superseded | 文档与资源路径检查 |
| MSR-V1-002 | 独立窗口调整为 `288 x 82`，时长与操作位于音量条下方，位置计算适配 DPI 与多屏。 | partial | 前端两层布局；Rust 窗口常量和 DPI / 负坐标 / 分辨率 / 吸附测试已更新 | 会议相关 Rust 测试与 controls 契约通过；仍缺真实胶囊窗口多屏观察 |
| MSR-V1-003 | WebGL 信号条由真实会议音量驱动，最多约 30fps。 | partial | `MeetingSignalRail` 接入现有 `meeting:audio-level`，30fps 上限和低渲染倍率 | 信号映射测试通过；Playwright 确认 WebGL 画布非空，仍缺真机音量变化 |
| MSR-V1-004 | idle / recording / quiet / paused / processing / completed 状态视觉完整。 | partial | 状态映射覆盖六态；暂停冻结、整理汇聚和完成扫光写入 Shader | 状态 / 信号测试通过，idle 浏览器截图通过；其余状态待真机观察 |
| MSR-V1-005 | 暂停 / 继续 / 停止、停止确认、时长和命令错误恢复保留。 | partial | 原命令门禁、计时器与恢复逻辑保留；停止确认改为内联 dialog | controls / runtime / sync 测试通过；待真实会议操作 |
| MSR-V1-006 | 更多、右键、隐藏、打开会议、锁定和拖动使用紧凑内联交互。 | partial | 更多与右键进入同一内联菜单，原拖动和位置 IPC 保留 | controls 契约和 TypeScript 通过；待键盘与真机操作 |
| MSR-V1-007 | 整理与完成 / 失败的既有 3 秒销毁规则保留。 | done | 继续使用 completion / failure dismiss timer，只把完成媒体时长改为信号扫光时长 | sync 测试与 19 个 Rust 定向测试通过 |
| MSR-V1-008 | WebGL / reduced motion 回退不影响会议控制，卸载释放资源。 | partial | WebGL 失败或 reduced motion 使用 DOM 刻度；卸载取消 RAF、删除资源并丢失 context | TypeScript、生产构建和 WebGL 浏览器截图通过；回退路径待人工切换 |
| MSR-V1-009 | 五种语言名称、状态、tooltip 和错误文案完整。 | done | 五份 locale 增加相同状态与内联控制 key，设置名称改为会议胶囊 | controls locale 扫描和 TypeScript 通过 |
| MSR-V1-010 | 普通语音胶囊、ASR、润色、插入、历史链路未改。 | done | 普通 `Capsule.tsx`、ASR client、润色、插入和历史实现未改；仅修复会议的百炼协议选择与 ASR 建连失败降级 | 任务 diff 审计；会议素材文件保留且校验通过 |
| MSR-V1-011 | TypeScript、生产构建、定向测试和 MSVC `cargo check` 通过。 | done | 最终源代码验证完成 | `tsc --noEmit`、`npm run build`、30 个会议协调器测试、3 个百炼路由 / endpoint 测试、五组会议信号测试、MSVC `cargo check` 均通过 |
| MSR-V1-012 | Windows 真实会议场景可用。 | partial | 最新开发版已从当前隔离分支启动；Qwen Realtime 路由与“ASR 建连失败仍保持录音 active、胶囊可初始化”状态回归通过 | 待用户验证真实录音与转写、胶囊出现、暂停 / 继续、停止、整理、完成 / 失败和多屏 |

## 当前统计

- `done 5`
- `partial 7`
- `missing 0`
- `blocked 0`
