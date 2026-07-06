# Acceptance Checklist Template

状态：template

## 使用规则

- 本模板用于 Level 2 / Level 3 功能，即跨模块、版本级或已有 `spec`（规格文档）的工作。
- 顶层 `spec` 是 scope authority（范围权威）；阶段 `plan`（计划文档）只能拆解实现，不能静默缩减范围。
- 如果某项从当前版本移出，必须有用户明确确认，并把状态标为 `deferred`（延期）。
- 没有用户确认的未实现项，一律标为 `missing`（未完成），不能标为 `deferred`。

## 状态定义

- `done`：已实现，并有自动测试或人工验证证据。
- `partial`：部分路径已实现，但字段、UI、错误处理或验收不完整。
- `missing`：未实现。
- `deferred`：用户明确确认移出当前版本。
- `blocked`：受环境、依赖或平台能力阻塞，已写明原因。

## Checklist

| ID | Requirement（需求） | Source（来源） | Status（状态） | Evidence（证据） | Verification（验证） | Notes（备注） |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-001 | 示例需求 | `docs/example-spec.md` | missing | - | - | 等待实现 |

## Deferred Log（延期记录）

| ID | Requirement（需求） | Deferred reason（延期原因） | User confirmation（用户确认） |
| --- | --- | --- | --- |
| - | - | - | - |

## Final Acceptance Gate（最终验收门槛）

在声称版本完成前，必须满足：

- 所有当前版本需求为 `done`，或有用户明确确认的 `deferred`。
- 所有 `partial`、`missing`、`blocked` 都已在交付中明确说明。
- 自动验证和人工验证结果已记录。
- 如果 `spec`、`plan`、代码或用户确认存在冲突，冲突已解决或记录为用户确认的延期项。
