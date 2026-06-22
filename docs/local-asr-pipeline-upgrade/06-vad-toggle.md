# 06-VAD 独立开关落地

## 本次只做一个改动点

只实现 `use_vad` 对本地 ASR 的实际影响。不要同时做标点、说话人识别或实时预览。

## 推荐策略

先支持 batch 场景：

- `use_vad=false`：整段音频直接送 ASR。
- `use_vad=true`：先 VAD 切段，再对 speech segments 跑 ASR，最后拼接文本。

如果当前 sherpa runtime 已有 streaming path，也不要在本任务里扩展实时预览。

## sherpa-onnx 路线

优先使用 sherpa-onnx 的 `VoiceActivityDetector` 或官方示例中的 Silero VAD 模型。

注意：

- VAD 模型文件应独立于 ASR 模型管理。
- VAD 需要明确 sample rate。
- VAD 切段会影响长音频处理和句间停顿。

## 非目标

不再接 FunASR Python 的 `use_vad`。如果代码里还存在相关透传逻辑，应在 `02-funasr-python-rollback.md` 中回滚。

## 成功标准

- UI/偏好里的 `use_vad` 能传到 provider。
- `use_vad=false` 和 `use_vad=true` 有可观察差异。
- 静音音频不会导致崩溃。
- 长音频不会明显卡死主线程。

## 不要做

- 不改标点。
- 不改说话人识别。
- 不做实时预览。
- 不新增模型 family。

## 验证样例

至少准备三类音频：

```text
1. 纯短句，无长静音
2. 中间有 2 秒以上静音
3. 大量静音 + 少量语音
```

## 验证命令

```powershell
cd openless-all\app\src-tauri
cargo test vad
cargo check
```

## 给 AI 的提示词

```text
请只实现 ASR 管线里的 use_vad 开关。
先读取 05-pipeline-options-contract.md 的契约，确认 effective options 已存在。
对 sherpa-onnx 优先接 VoiceActivityDetector/Silero VAD。
不要做标点、说话人识别、实时预览。
完成后用有静音的音频说明验证结果。
```

## 下次继续需要提供

```text
VAD 模型路径：
VAD 默认值：
use_vad=true 样例结果：
use_vad=false 样例结果：
长静音行为：
```
