# 04-sherpa-onnx + Paraformer ONNX batch 基线

## 本次只做一个改动点

只让 `sherpa-onnx-local` 使用 Paraformer ONNX 完成停止录音后的 batch 转写。不要同时做 VAD、标点恢复、说话人识别、实时预览或 UI 大改。

## 背景

这是回滚 FunASR Python 运行时之后的第一条核心 ASR 路线。Paraformer ONNX 是 Windows 本地中文 batch ASR 的 P0 目标。

## 成功标准

- `sherpa-onnx-local` 能选择 Paraformer ONNX 模型。
- 录音停止后，PCM/WAV 能通过 `sherpa_onnx::OfflineRecognizer` 得到中文文本。
- 成功结果转换为现有 `RawTranscript`，继续走 polish / insert / history。
- 失败时不会影响其他 ASR provider。
- Windows 下 `cargo check` 通过。

## 预计涉及文件

以实际代码为准，优先检查：

```text
openless-all/app/src-tauri/src/asr/local/sherpa.rs
openless-all/app/src-tauri/src/asr/local/sherpa_runtime.rs
openless-all/app/src-tauri/src/asr/local/sherpa_provider.rs
openless-all/app/src-tauri/src/commands/sherpa_asr.rs
openless-all/app/src-tauri/src/coordinator.rs
openless-all/app/src-tauri/src/coordinator/dictation.rs
```

## 实现步骤

1. 确认 Paraformer ONNX catalog alias。
   - 建议 alias：`paraformer-zh`。
   - 必需文件：`model.int8.onnx`、`tokens.txt`。

2. 确认 runtime 创建 `OfflineRecognizer`。
   - 只支持 CPU。
   - 不引入 CUDA / DirectML。
   - 模型文件缺失时返回明确错误。

3. 确认 provider buffer 行为。
   - `consume_pcm_chunk()` 只追加 PCM。
   - `transcribe()` clone 当前 PCM，调用 runtime。
   - 成功或失败后按现有本地 provider 语义清理 buffer。

4. 接入 coordinator。
   - 只 mirror 现有本地 batch provider 分支。
   - 不改 macOS Qwen3。
   - 不改 Foundry。

5. 最小验证。
   - 用短中文音频测试。
   - 确认文本进入现有插入链路。

## 不要做

- 不做实时 partial。
- 不做 VAD 开关。
- 不做标点恢复开关。
- 不做说话人识别。
- 不新增 SenseVoice / Fun-ASR-Nano。
- 不把 `sherpa-onnx-local` 改成默认 provider。

## 验证命令

```powershell
cd openless-all\app\src-tauri
cargo check
cargo test sherpa
```

如果已有桌面启动脚本，再做一次手动录音 smoke test。

## 给 AI 的提示词

```text
请按 docs/local-asr-pipeline-upgrade/04-sherpa-paraformer-batch.md 执行。
本次只做一个改动点：让 sherpa-onnx-local 使用 Paraformer ONNX 完成停止录音后的 batch 转写。
不要做 VAD、标点、说话人识别、实时预览或 UI 大改。
先读现有 sherpa.rs、sherpa_runtime.rs、sherpa_provider.rs、coordinator 相关分支，复用现有模式。
完成后运行 cargo check，并说明如何手动验证中文短句。
不要 commit，除非我明确要求。
```

## 下次继续需要提供

```text
Paraformer alias：
模型目录：
已验证音频类型：
转写结果样例：
cargo check 结果：
仍未解决的问题：
```
