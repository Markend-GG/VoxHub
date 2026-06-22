# 07-标点恢复独立开关落地

## 本次只做一个改动点

只实现 `use_punctuation`。不要改 VAD、说话人识别、实时预览。

## 推荐策略

标点有两种来源：

1. ASR 模型内置标点。
2. 独立 punctuation 模型后处理。

统一语义应该是：

```text
use_punctuation=false -> 尽量返回未加独立标点的原始文本
use_punctuation=true  -> 使用模型内置或独立 Punc，失败则回退 raw_text
```

如果某模型总是输出带标点文本，capability 里要标记为 `Supported` 但说明它不是完全可关闭。

## sherpa-onnx 路线

优先调研 sherpa-onnx punctuation API：

- offline punctuation 适合停止录音后的最终文本。
- online punctuation 适合后续 streaming preview，但本任务不做实时。

## 非目标

不再接 FunASR Python 的 `use_punc`。如果代码里还存在相关透传逻辑，应在 `02-funasr-python-rollback.md` 中回滚。

## 成功标准

- `use_punctuation=false` 时不会调用独立 Punc 模型。
- `use_punctuation=true` 时能得到带标点文本。
- Punc 失败时转写仍成功，返回 raw text。
- 单元测试覆盖 options 传递或 fallback。

## 不要做

- 不做 LLM 润色替代标点。
- 不把 polish 阶段当作 ASR 标点恢复。
- 不改 VAD。
- 不改说话人识别。

## 验证命令

```powershell
cd openless-all\app\src-tauri
cargo test punc
cargo check
```

## 给 AI 的提示词

```text
请只实现 ASR 管线里的 use_punctuation 开关。
标点恢复必须是 ASR/Punc 层能力，不要用 LLM polish 代替。
对 sherpa-onnx 先使用官方 punctuation API，如当前 crate 能力不足则只做 capability 标记和清晰的后续说明。
Punc 失败时必须回退 raw_text。
不要 commit。
```

## 下次继续需要提供

```text
标点模型/接口：
use_punctuation=true 输出：
use_punctuation=false 输出：
失败回退行为：
```
