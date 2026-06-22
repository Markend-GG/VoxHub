# 08-说话人识别/分离落地

## 本次只做一个改动点

只实现 `use_speaker_diarization` 的 batch 后处理。不要做实时说话人预览。

## 术语边界

这里建议先做 speaker diarization，也就是“谁在什么时候说话”，不是用户声纹登录，也不是声纹识别某个已知联系人。

第一版输出建议：

```text
Speaker 1: ...
Speaker 2: ...
```

不要做“张三/李四”实名识别。

## sherpa-onnx 路线

sherpa-onnx Rust API 有 `OfflineSpeakerDiarization`，适合完整音频后处理。

建议第一版：

1. 录音结束后保留完整 PCM。
2. 如果 `use_speaker_diarization=true`，先跑 diarization 得到 segments。
3. ASR 仍按当前方式转写。
4. 如果暂时无法精确按 speaker 切文本，先把 speaker segments 放进 metadata，不改变插入文本。

更完整版本再做：

```text
speaker segments -> 按时间切音频 -> 每段 ASR -> 拼接带 speaker label 的文本
```

## 非目标

不再通过 FunASR Python 的 `spk_model="cam++"` 接说话人识别。第一版只考虑 sherpa-onnx Rust API 或独立 ONNX diarization 模型。

## 成功标准

- 开关关闭时没有额外耗时。
- 开关打开时能返回 speaker metadata 或带 speaker label 的文本。
- 单人录音不应产生大量错误 speaker 切换。
- 失败时回退普通 ASR 文本。

## 不要做

- 不做实名声纹识别。
- 不做实时 speaker label。
- 不强制所有模型都支持。
- 不破坏现有插入文本格式。

## 验证命令

```powershell
cd openless-all\app\src-tauri
cargo test speaker
cargo check
```

手动验证：

```text
单人音频：应只有一个 speaker 或不显示 speaker label
双人音频：应出现两个 speaker segment
失败场景：仍返回普通文本
```

## 给 AI 的提示词

```text
请只实现 use_speaker_diarization 的 batch 后处理。
第一版不要做实名声纹识别，也不要做实时 speaker 预览。
优先让结果进入 metadata；只有在能可靠按 segment 转写时，才输出 Speaker 1/Speaker 2 文本。
失败必须回退普通 ASR。
不要改 VAD 和标点逻辑。
```

## 下次继续需要提供

```text
使用的 diarization 模型/接口：
单人测试结果：
双人测试结果：
输出格式：
失败回退行为：
```
