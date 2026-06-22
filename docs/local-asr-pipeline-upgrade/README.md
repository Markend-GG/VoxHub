# 本地 ASR 管线升级总规划

> 日期：2026-06-21
> 范围：Windows 优先；目标是回滚之前的 FunASR Python 运行时开发内容，统一走 `sherpa-onnx + Paraformer ONNX` 本地 ASR 路线，并在此基础上评估 SenseVoice Small、Paraformer Large、Fun-ASR-Nano 的 ONNX/sherpa-onnx 支持。
> 底线：一次只做一个改动点。每次开发只拿一个子文档执行，完成后提供交接信息，再进入下一步。

## 1. 当前判断

仓库当前已经不是从零开始：

- 已有一批 `funasr-local` Python sidecar 开发内容，相关入口包括 `openless-all/app/src-tauri/python/funasr_server.py`、`openless-all/app/src-tauri/src/asr/local/funasr*.rs`。这些运行时接入不再保留，后续要作为独立回滚任务移除。
- 已有 `sherpa-onnx-local` Rust/ONNX 路线，相关入口包括 `openless-all/app/src-tauri/src/asr/local/sherpa*.rs`、`commands/sherpa_asr.rs`。
- `Cargo.toml` 已出现 `sherpa-onnx` 依赖；后续任务必须先确认当前工作树里这些改动是否已经稳定，再继续扩展。
- 现有主链路应继续复用：`Recorder -> AudioConsumer -> RawTranscript -> polish -> insert -> history`。

我的建议不是一次性重构 ASR 体系，而是先把能力拆成独立、可验收的小步：

1. 先冻结现状，确认哪些 FunASR Python 改动需要回滚、哪些下载能力可以复用。
2. 回滚 FunASR Python 运行时、provider、命令、UI 入口和预检脚本。
3. 保留可复用的下载能力，但改造成 sherpa-onnx ONNX 模型下载：更新下载地址、required files、缓存目录、校验与删除逻辑。
4. 让 `sherpa-onnx + Paraformer ONNX` 作为稳定 batch ASR 跑通。
5. 再设计通用管线开关：VAD、标点恢复、说话人识别。
6. 再做实时预览；实时预览必须按模型能力分级，不能假装所有模型都支持 token streaming。
7. 最后逐个评估 SenseVoice Small、Paraformer Large、Fun-ASR-Nano 是否有可产品化的 ONNX/sherpa-onnx 路线。

## 2. 核心取舍

### sherpa-onnx 是主路线

推荐把 `sherpa-onnx-local` 作为 Windows 本地 ASR 主路线，因为它更适合桌面应用：

- Rust 内集成，减少 Python 环境、Torch、ModelScope 缓存问题。
- 支持 offline ASR、streaming ASR、VAD、说话人相关能力等 Rust API。
- 模型以 ONNX 文件分发，更容易做下载、校验、打包边界控制。

### FunASR Python 运行时必须回滚

用户已经明确不需要 FunASR Python。后续文档和开发都以这个结论为准：

- 不再保留 Python sidecar 作为 ASR runtime。
- 不再保留 `funasr-local` 作为可选 provider。
- 不再保留 FunASR Python 预检、启动、模型加载、stdin/stdout JSON 协议。
- 可以保留下载器里的通用能力，例如断点续传、进度事件、取消、删除、reveal、镜像选择、校验流程。
- 但下载地址、模型目录、文件列表、状态字段必须改为 sherpa-onnx ONNX 模型语义，不能继续下载 FunASR PyTorch / ModelScope 三模型。

结论：第一阶段要先回滚 Python 运行时，再建设 sherpa-onnx ONNX 路线。不要同时保留两套本地中文 ASR runtime，避免后续设置页、下载器、provider 状态和测试矩阵全部分叉。

## 3. 能力是否能通用到全局模型

可以通用，但必须按能力分层。

| 能力 | 是否可全局通用 | 推荐实现方式 | 注意 |
|---|---|---|---|
| VAD | 基本可以 | 独立音频预处理模块或 provider 内置 VAD | 对 streaming 和 batch 的切分语义不同 |
| 标点恢复 | 可以部分通用 | 独立 Punc 后处理，或模型内置标点 | 英文、中文、多语标点模型不能混用 |
| 说话人识别/分离 | 可以作为后处理通用 | 对完整音频跑 diarization，再把 speaker label 合并回文本段 | 实时场景成本高，先做 batch |
| 实时预览 | 不能完全通用 | streaming 模型走 partial token；offline 模型只能做 VAD 分段预览或停止后预览 | Paraformer/SenseVoice offline 不等于真正实时 token |
| 热词 | 取决于模型和 API | provider capability 暴露 | 不要做全局假开关 |
| 语言识别 | 取决于模型 | SenseVoice 或其他 ONNX 模型可暴露 metadata | 不要影响现有文本插入链路 |

因此 UI 可以做统一开关，但后端必须有 `capabilities`：

```text
provider/model -> capabilities -> UI 是否可点 -> runtime 实际启用
```

不要让用户打开一个 provider 根本不支持的开关。

## 4. 目标模型策略

| 模型 | 优先级 | 推荐接入路线 | 第一版验收 |
|---|---:|---|---|
| Paraformer ONNX | P0 | sherpa-onnx offline | 中文短句/长句 batch 转写稳定 |
| SenseVoice Small | P1 | sherpa-onnx offline | 中英混合、粤语/日/韩基础可用；额外标签先作为 metadata |
| Paraformer Large | P2 | 仅在有稳定 ONNX/sherpa-onnx 路线时接入 | 没有 ONNX 就暂缓，不回退到 Python |
| Fun-ASR-Nano | P3 | 仅做 ONNX/sherpa-onnx 可行性评估 | 没有稳定 ONNX 就暂缓，不引入 Python/HTTP sidecar |

## 5. 管线设计建议

建议把 ASR 结果拆成两层：

```text
RawTranscript
  text: 最终插入文本
  duration_ms
  language?
  segments?
  metadata?

AsrPipelineOptions
  use_vad: bool
  use_punctuation: bool
  use_speaker_diarization: bool
  realtime_preview: bool
  language_hint?: string
  hotwords?: string[]
```

第一版不要大改 `RawTranscript`。可以先在 provider 内部消费 `AsrPipelineOptions`，只返回现有 `RawTranscript`。等说话人识别和实时预览真的需要结构化段落时，再最小扩展 `segments`。

## 6. 推荐路线图

按下面顺序执行，每次只拿一个子文档：

1. [01-确认现状与冻结边界](./01-current-state-freeze.md)
2. [02-回滚 FunASR Python 运行时](./02-funasr-python-rollback.md)
3. [03-模型 catalog 与下载校验](./03-model-catalog-downloads.md)
4. [04-sherpa-onnx + Paraformer ONNX batch 基线](./04-sherpa-paraformer-batch.md)
5. [05-管线开关契约：VAD / 标点 / 说话人](./05-pipeline-options-contract.md)
6. [06-VAD 独立开关落地](./06-vad-toggle.md)
7. [07-标点恢复独立开关落地](./07-punctuation-toggle.md)
8. [08-说话人识别/分离落地](./08-speaker-diarization.md)
9. [09-实时预览文字](./09-realtime-preview.md)
10. [10-SenseVoice Small 支持](./10-sensevoice-small.md)
11. [11-Paraformer Large 支持](./11-paraformer-large.md)
12. [12-Fun-ASR-Nano ONNX 可行性评估](./12-funasr-nano-onnx-feasibility.md)
13. [13-设置页与用户体验收口](./13-settings-ux.md)
14. [14-打包、测试与发布](./14-packaging-testing-release.md)
15. [15-vibe-coding-开发指南](./15-vibe-coding-guide.md)

## 7. 每次开发的交接信息

每完成一个子任务，请给下一轮开发保留这段信息：

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

没有这些信息，不建议直接进入下一步。

## 8. 官方资料

- sherpa-onnx 官方仓库：https://github.com/k2-fsa/sherpa-onnx
- sherpa-onnx 官方文档：https://k2-fsa.github.io/sherpa/onnx/
- sherpa-onnx Rust API：https://docs.rs/sherpa-onnx
- sherpa-onnx 预训练模型：https://k2-fsa.github.io/sherpa/onnx/pretrained_models/index.html
- FunASR 官方资料仅用于确认模型来源和 ONNX 可行性，不再作为 Python runtime 路线：https://github.com/modelscope/FunASR
- SenseVoiceSmall 模型页：https://huggingface.co/FunAudioLLM/SenseVoiceSmall
