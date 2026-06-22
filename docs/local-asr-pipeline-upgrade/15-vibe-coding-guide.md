# 15-vibe-coding 开发指南

## 核心原则

本项目的 ASR 升级必须按“小步、可验收、可回退”执行：

- 一次只拿一个子文档。
- 一次只做一个改动点。
- 每次先跑 `git status --short`。
- 未经用户确认不 commit。
- 不把模型、缓存、日志、构建产物加入 Git。
- 每次结束必须留下交接信息。

## 每次开工提示词模板

```text
我要执行本地 ASR 升级的一个子任务。

任务文档：
docs/local-asr-pipeline-upgrade/XX-xxx.md

本次只允许做的改动点：
<从文档复制一句>

成功标准：
<从文档复制>

不要改动：
<从文档复制>

请先做：
1. 运行 git status --short。
2. 阅读任务文档和相关代码。
3. 复述你的假设、影响文件、验证方式。
4. 再开始修改。

提交规则：
可以本地最小 commit，但 git commit 必须先获得我的明确确认。
不要 push。
```

## 让 AI 先分析的提示词

```text
先不要改代码。
请阅读 docs/local-asr-pipeline-upgrade/XX-xxx.md 和相关代码，告诉我：
1. 当前代码是否已经部分实现。
2. 还缺什么。
3. 最小改动文件有哪些。
4. 验证命令是什么。
5. 有哪些风险或歧义需要我确认。
```

## 让 AI 小步实现的提示词

```text
按刚才确认的方案实现。
只做本子文档要求的一个改动点。
不要顺手重构，不要改无关格式，不要新增额外功能。
改完后运行文档里的验证命令。
最后说明每个改动文件为什么必要。
不要 commit。
```

## 让 AI 做 code review 的提示词

```text
请用 code review 视角检查这次改动。
优先找 bug、行为回归、线程/生命周期问题、模型文件路径问题、缺少测试。
请按严重程度列出文件和行号。
如果没有发现问题，也要说明未覆盖的测试风险。
```

## 让 AI 准备提交的提示词

```text
请总结这次改动：
1. 目标
2. 实际完成
3. 修改文件
4. 验证命令和结果
5. 已知风险
6. 建议 commit message

先不要 commit，等我确认。
```

## 每次结束交接模板

```text
本次目标：
实际完成：
涉及文件：
新增/修改的 provider 或命令：
新增配置字段：
新增事件名：
验证命令：
验证结果：
已知问题：
下一步建议：
是否已 commit：
commit hash（如有）：
```

## 官方文档快速入口

- sherpa-onnx 官方仓库：https://github.com/k2-fsa/sherpa-onnx
- sherpa-onnx 文档：https://k2-fsa.github.io/sherpa/onnx/
- sherpa-onnx Rust API：https://docs.rs/sherpa-onnx
- sherpa-onnx 预训练模型：https://k2-fsa.github.io/sherpa/onnx/pretrained_models/index.html
- FunASR 官方资料仅用于确认模型来源和 ONNX 可行性，不再作为 Python runtime 路线：https://github.com/modelscope/FunASR
- SenseVoiceSmall：https://huggingface.co/FunAudioLLM/SenseVoiceSmall

## 判断 AI 是否跑偏

出现以下情况要立刻停：

- 同时改多个子文档范围。
- 还没确认模型来源就写死下载 URL。
- 把 FunASR Python 回滚、Rust 推理、UI、打包一次性全改。
- 把实时预览说成所有模型都支持。
- 用 LLM polish 冒充 ASR 标点恢复。
- 把普通 Paraformer ONNX 命名成 Paraformer Large。
- 把 FunASR Python 留作 fallback。
- 未经确认执行 commit 或 push。
