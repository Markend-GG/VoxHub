# 10-SenseVoice Small 支持

## 本次只做一个改动点

只接入 SenseVoice Small。不要同时接 Paraformer Large 或 Fun-ASR-Nano。

## 接入路线

优先使用 sherpa-onnx offline SenseVoice Small：

```text
provider: sherpa-onnx-local
family: SenseVoice
mode: Offline
alias: sense-voice-small-zh
```

SenseVoice Small 不只是 ASR，还可能输出语言、情绪、音频事件等信息。第一版不要把这些额外标签强塞进插入文本，建议放到 metadata 或暂不展示。

## 成功标准

- 模型可下载/检测/删除。
- 能转写中文和中英混合短句。
- `language_hint` 对支持的语言不报错。
- 输出文本继续走现有 polish / insert / history。
- 额外标签不会污染用户要插入的正文。

## 不要做

- 不做情绪识别 UI。
- 不做音频事件 UI。
- 不改变 Paraformer 默认行为。
- 不做 Fun-ASR-Nano。

## 验证样例

```text
中文短句
中英混合短句
粤语或日/韩短句（如果有样例）
静音/噪声
```

## 验证命令

```powershell
cd openless-all\app\src-tauri
cargo test sense
cargo check
```

## 给 AI 的提示词

```text
请只接入 SenseVoice Small，不要碰 Paraformer Large 或 Fun-ASR-Nano。
优先使用 sherpa-onnx offline SenseVoice 模型。
额外的语言/情绪/音频事件标签只能进入 metadata 或日志，不能污染插入文本。
完成后验证中文和中英混合短句。
不要 commit。
```

## 下次继续需要提供

```text
SenseVoice alias：
模型文件：
中文测试结果：
中英混合测试结果：
metadata 处理方式：
```
