# AI 对话上下文增强需求文档

状态：Backlog  
优先级：P3 / 低  
日期：2026-06-26  
适用范围：VoxHub / OpenLess 后续版本  
需求类型：后续功能记录，不进入当前开发  

## 1. 背景

用户在 Codex、Qoder、Alma、WorkBuddy 等 AI 桌面端中会同时维护多个不同主题的对话。例如：

- 在 WorkBuddy / Work 8D 中，一个对话用于“情感开发”，另一个对话用于“日报”。
- 在 Codex 中，一个对话用于“图片”，另一个对话用于“竞品对比”。

当用户在这些 AI 对话窗口中使用 OpenLess 进行语音输入或文本重写时，如果 OpenLess 只知道当前应用是 WorkBuddy 或 Codex，而不知道当前具体是哪一个对话，则无法安全地引用上下文。应用级上下文会导致“日报”“情感开发”“图片”“竞品对比”等主题互相污染，反而降低生成质量。

因此，本需求的核心不是“识别当前应用”，而是识别：

```text
当前上下文空间 = 来源应用 + 当前具体对话
```

只有当 OpenLess 能可靠识别当前具体对话时，才允许把该对话对应的上下文摘要注入语音润色或文本重写 prompt。

## 2. 目标

- 在用户触发语音输入或文本重写时，识别当前所在的 AI 桌面端及具体对话。
- 从本地只读索引中找到该对话对应的上下文摘要。
- 仅将当前对话的摘要注入本次语音润色或文本重写 prompt。
- 同一应用内不同对话之间上下文隔离，禁止默认串用。
- 识别失败、不唯一或耗时超限时，不注入上下文，主流程继续。

## 3. 非目标

- 不在当前阶段实现。
- 不修改 Codex、Qoder、Alma、WorkBuddy 等第三方应用数据库。
- 不把上下文写回第三方 AI 对话。
- 不做应用级兜底注入。
- 不默认跨对话引用上下文。
- 不默认上传第三方对话原文全文给 LLM。
- 不替代现有截图多模态分析、语音历史、重写历史能力。

## 4. 核心原则

| 原则 | 说明 |
| --- | --- |
| 对话级隔离 | 上下文边界必须细到“应用 + 具体对话”，不能只到应用级。 |
| 识别不到就跳过 | 如果无法判断当前具体对话，则不注入任何 AI 对话上下文。 |
| 只读接入 | 第三方应用数据只读，不回写、不迁移、不破坏原始数据。 |
| 摘要注入 | prompt 中只注入必要摘要，不默认注入完整聊天全文。 |
| 不阻塞主流程 | 上下文识别、索引和摘要读取不能拖慢语音输入、文本重写和插入链路。 |
| 用户可感知 | 用户应知道本次是否引用了 AI 对话上下文，以及引用的是哪个对话。 |

## 5. 使用场景

### 5.1 WorkBuddy 内不同对话

用户在 WorkBuddy 的“情感开发”对话中触发语音输入：

- OpenLess 识别当前来源为 WorkBuddy。
- OpenLess 识别当前具体对话为“情感开发”。
- 语音润色 prompt 只引用“情感开发”对话摘要。
- 不引用 WorkBuddy 中“日报”对话的上下文。

用户切换到 WorkBuddy 的“日报”对话后触发重写：

- OpenLess 识别当前具体对话为“日报”。
- 文本重写 prompt 只引用“日报”上下文。
- 不引用“情感开发”上下文。

### 5.2 Codex 内不同对话

用户在 Codex 的“图片”对话中触发重写：

- OpenLess 只引用“图片”对话摘要。
- 不引用 Codex 中“竞品对比”对话摘要。

用户切换到 Codex 的“竞品对比”对话后触发语音输入：

- OpenLess 只引用“竞品对比”对话摘要。
- 不引用“图片”对话摘要。

### 5.3 识别失败

用户在某个 AI 桌面端窗口中触发重写，但 OpenLess 只能识别应用，无法识别具体对话：

- 不注入 AI 对话上下文。
- 继续执行普通重写流程。
- 小浮窗可提示“未识别到当前对话，未引用上下文”。

## 6. 功能需求

### 6.1 数据源授权与检测

- 用户可以在设置中启用或关闭“AI 对话上下文增强”。
- 每个数据源应单独授权和检测。
- 数据源至少包括候选项：Codex、Qoder、Alma、WorkBuddy。
- 未检测到本地存储目录的数据源应显示“未检测到”。
- 授权后只读读取本地对话索引，不修改源数据。

### 6.2 对话列表读取

- 系统应能读取已授权数据源的对话列表。
- 每条对话至少应具备可展示名称、来源应用、更新时间和可用于匹配的本地引用。
- 如果数据源只能读取任务产物，不能读取具体对话，则不得标记为“支持对话上下文增强”。

### 6.3 当前对话识别

- 用户触发语音输入或文本重写时，系统应尝试识别当前 AI 桌面端的具体对话。
- 识别结果必须能区分同一应用内的不同对话。
- 如果识别结果为空、不唯一或置信度不足，应视为识别失败。
- 识别失败时不得退化为应用级上下文注入。

### 6.4 上下文摘要读取

- 系统应从当前对话中生成或读取摘要化上下文。
- 摘要应服务于本次语音润色或文本重写，不默认暴露完整聊天正文。
- 摘要应尽量包含当前对话主题、关键约束、已确认决策、待办和术语。

### 6.5 Prompt 注入

- 仅当当前具体对话识别成功时，才允许注入上下文摘要。
- 注入内容应标明来源应用和对话名称。
- 注入内容应保持简洁，避免覆盖用户本次原文和风格包意图。
- 用户关闭该能力后，不得注入任何 AI 对话上下文。

### 6.6 用户感知

- 语音输入或文本重写浮窗中应轻量提示上下文引用状态。
- 识别成功时展示类似：“已引用：WorkBuddy / 情感开发”。
- 识别失败时可展示：“未识别到当前对话，未引用上下文”。
- 历史详情中应记录本次是否引用 AI 对话上下文、来源应用、对话名称和摘要版本。

## 7. 相关能力识别

| 已有能力 | 能力范围 | 与本需求匹配度 | 能力差距 | 建议方向 | 来源 |
| --- | --- | --- | --- | --- | --- |
| 上下文采集 | 触发语音输入或文本重写时采集应用、窗口、截图并关联历史 | 中 | 当前偏窗口和截图，不具备第三方 AI 对话级识别 | 可复用触发时机、历史关联和用户感知方式 | `docs/context-capture-target-mode.md` |
| 截图多模态分析 | 对截图和原文进行本地授权后的多模态分析，并在历史详情展示摘要 | 中 | 只能分析当前截图，不等同于第三方对话历史上下文 | 可复用摘要字段、授权提示和历史详情展示思路 | `docs/context-vision-analysis-target-mode.md` |
| 语音历史 / 重写历史 | 已独立保存用户语音输入和重写结果 | 中 | 记录的是 OpenLess 内部历史，不包含外部 AI 桌面端完整对话 | 可记录本次引用了哪个外部对话上下文 | `openless-all/app/src/pages/History.tsx` |
| 第三方本地存储勘察 | 已初步定位 Codex、Qoder、Alma 等本地会话数据来源 | 高 | 仍需验证“当前正在看的具体对话”能否稳定匹配到本地记录 | 后续先做 PoC，不直接产品化 | 本轮需求讨论 |

## 8. 性能与稳定性要求

- 不允许在语音输入或文本重写热路径中进行全量数据库扫描。
- 本地对话索引应在后台更新，触发时只做轻量匹配和摘要读取。
- 如果上下文识别或摘要读取超过设定时间，应立即降级为“不注入”。
- 降级不得影响录音、ASR、语音润色、文本重写、插入、剪贴板回退和历史保存。
- 识别状态和注入状态应记录到历史详情，便于后续排查。

## 9. 隐私与安全要求

- 第三方对话数据默认不读取，必须由用户开启并授权。
- 只读访问第三方本地数据，不回写、不删除、不迁移。
- 默认只保存摘要和引用信息，不复制完整第三方聊天历史。
- 如需把摘要发送给 LLM，应在设置中明确说明。
- 不展示、不记录、不上传 API Key、token、OAuth token 等敏感字段。
- 对 Alma 这类可能包含截图和活动记录的数据源，应单独提示隐私风险。

## 10. PoC 成功门槛

该需求必须先经过 PoC，PoC 不通过则不进入正式开发。

PoC 必须证明：

- 能读取至少一个数据源的对话列表和摘要。
- 能在同一应用内区分两个不同对话。
- 用户切换对话后，系统能识别当前具体对话。
- 识别不到具体对话时不会注入任何上下文。
- 本地匹配不明显拖慢语音输入和文本重写。
- 第三方应用数据库没有被修改。

如果只能做到“识别当前应用”，但做不到“识别当前具体对话”，则该需求应暂停或否决。

## 11. 验收标准

- [ ] 在同一 AI 桌面端内切换两个不同对话时，上下文不串用。
- [ ] 当前具体对话识别成功时，语音输入和重写可引用该对话摘要。
- [ ] 当前具体对话识别失败时，不注入应用级上下文。
- [ ] 用户可在浮窗或历史详情中看到本次引用状态。
- [ ] 关闭功能后，不读取第三方对话上下文，不注入 prompt。
- [ ] 上下文读取失败、超时或索引异常不影响主流程。
- [ ] 历史详情能记录本次引用来源，便于回溯生成质量。

## 12. 后续开放问题

- WorkBuddy 的完整对话数据库位置是否存在，还是只能读取任务产物目录。
- 不同 AI 桌面端的“当前对话”是否能通过窗口标题、线程 ID、活动文件、数据库最近更新时间等方式稳定定位。
- 摘要生成应由本地规则完成，还是由 LLM 后台异步生成。
- 用户是否需要手动选择“当前对话”作为识别失败时的补救。
- 是否允许用户显式选择其他对话作为临时参考上下文。

## 13. 追溯元数据

```yaml
TRACEABILITY-METADATA:
  schema:
    profile: prd-profile-v1
    version: 1
  artifact:
    id: PRD-AI-CONVERSATION-CONTEXT-ENHANCEMENT
    type: PRD
    title: AI Conversation Context Enhancement
    status: backlog
    source_documents:
      - docs/context-capture-target-mode.md
      - docs/context-vision-analysis-target-mode.md
  entities:
    requirements:
      - id: REQ-AICTX-001
        class: functional
        title: Detect current AI conversation
        statement: The system should identify the current source app and concrete conversation before injecting any AI conversation context.
        priority: P3
        status: backlog
        scope: future
        acceptance_criteria:
          - Same-app different conversations can be distinguished.
          - App-level-only detection does not trigger context injection.
      - id: REQ-AICTX-002
        class: functional
        title: Inject only current conversation summary
        statement: The system should inject only the matched current conversation summary into voice polish or rewrite prompts.
        priority: P3
        status: backlog
        scope: future
        acceptance_criteria:
          - Context from unrelated conversations is not injected by default.
          - Injection is skipped when matching is ambiguous.
      - id: REQ-AICTX-003
        class: non_functional
        title: Avoid blocking voice and rewrite flow
        statement: Conversation context matching should not block voice input, text rewrite, insertion, clipboard fallback, or history save.
        priority: P3
        status: backlog
        scope: future
        acceptance_criteria:
          - Timeout or failure falls back to no context injection.
          - Main user flow continues normally.
      - id: REQ-AICTX-004
        class: security
        title: Read third-party conversation data safely
        statement: Third-party AI conversation data should be read only after user authorization and never written back.
        priority: P3
        status: backlog
        scope: future
        acceptance_criteria:
          - Third-party databases are not modified.
          - Users can disable each data source.
  relations:
    - type: derived_from
      from: REQ-AICTX-001
      to: docs/context-capture-target-mode.md
    - type: derived_from
      from: REQ-AICTX-002
      to: docs/context-vision-analysis-target-mode.md
```
