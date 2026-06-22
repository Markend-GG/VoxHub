# 11-Paraformer Large 支持

## 本次只做一个改动点

只接入 Paraformer Large。不要同时改 SenseVoice 或 Fun-ASR-Nano。

## 关键风险

之前的 FunASR Python 基线使用的是 Paraformer Large PyTorch 模型，但该路线已不再保留。要接入 Paraformer Large，必须先确认是否有稳定、可分发、许可明确的 ONNX/sherpa-onnx 模型文件。

如果没有官方稳定 ONNX 文件，不要临时自行导出并作为产品默认方案，也不要回退到 FunASR Python provider。

## 推荐路线

优先级：

1. 官方 sherpa-onnx / ModelScope / HuggingFace 已发布 Paraformer Large ONNX。
2. 如果只有 Paraformer 普通 ONNX，则 catalog 中明确命名，不冒充 Large。
3. 如果没有 ONNX，则暂缓 Paraformer Large，不引入 Python sidecar。

## 成功标准

- alias 名称准确，不误导。
- 模型来源、文件列表、体积、许可写清楚。
- 如果走 ONNX，能通过 sherpa-onnx batch 转写。
- 如果没有 ONNX，必须明确标记为暂缓，不做兼容 provider。

## 不要做

- 不把普通 Paraformer ONNX 命名为 Large。
- 不在没有验证的情况下加入默认推荐。
- 不同时重构下载器。

## 验证命令

```powershell
cd openless-all\app\src-tauri
cargo check
```

手动验证：

```text
同一段中文长句分别用 paraformer-zh 与 paraformer-large 跑一次，对比文本和耗时。
```

## 给 AI 的提示词

```text
请只处理 Paraformer Large 支持。
第一步必须确认是否有官方稳定 ONNX 模型；如果没有，不要冒充 ONNX，也不要保留 FunASR Python sidecar 方案。
不要改 SenseVoice、Fun-ASR-Nano、VAD、标点、说话人识别。
输出模型来源、alias、文件列表、验证结果。
不要 commit。
```

## 下次继续需要提供

```text
采用路线：ONNX / 暂缓
模型来源链接：
alias：
文件体积：
中文长句测试结果：
耗时：
```
