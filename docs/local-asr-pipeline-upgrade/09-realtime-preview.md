# 09-实时预览文字

## 本次只做一个改动点

只实现“录音中预览文字”的事件链路。不要新增模型，不改最终转写质量策略。

## 关键判断

实时预览不等于所有模型都能实时 token 输出。

推荐分成三档：

| 档位 | 含义 | 适用模型 |
|---|---|---|
| `streaming_partial` | 真 streaming，边录边返回 partial | sherpa online Zipformer、云端 streaming |
| `vad_segment_preview` | VAD 检测到短句结束后转写该段，显示阶段性预览 | offline Paraformer/SenseVoice 可选 |
| `final_only` | 停止录音后才显示最终结果 | 不支持实时的 provider |

第一版建议先做事件和 UI 消费，再接一个 streaming 模型。

## 事件建议

```text
asr-preview-start
asr-preview-partial
asr-preview-segment-final
asr-preview-final
asr-preview-clear
```

payload 最小结构：

```json
{
  "sessionId": "string",
  "provider": "sherpa-onnx-local",
  "model": "zipformer-bilingual-zh-en-streaming",
  "text": "partial text",
  "isFinal": false
}
```

## 成功标准

- 开始录音后预览区域清空。
- streaming provider 能持续更新 partial。
- 停止录音后最终文本与现有插入链路一致。
- provider 不支持实时预览时，UI 不显示假进度。
- 切换 provider 不会收到上一轮 session 的旧事件。

## 预计涉及文件

```text
openless-all/app/src-tauri/src/asr/local/sherpa_provider.rs
openless-all/app/src-tauri/src/asr/local/sherpa_runtime.rs
openless-all/app/src-tauri/src/coordinator/dictation.rs
openless-all/app/src/pages/LocalAsr/index.tsx
openless-all/app/src/lib/localAsr.ts
```

## 不要做

- 不接 SenseVoice。
- 不接 Fun-ASR-Nano。
- 不改变最终插入逻辑。
- 不让 offline 模型伪装成 token streaming。

## 验证命令

```powershell
cd openless-all\app\src-tauri
cargo test preview
cargo check
```

手动验证：

```text
按住录音 -> 预览出现 partial -> 松开 -> final 替换 partial -> 文本正常插入
快速取消 -> 旧 partial 不再出现
切换 provider -> capability 正确禁用预览
```

## 给 AI 的提示词

```text
请只实现 ASR 实时预览的事件链路和一个最小可用 provider 接入。
不要新增模型，不要改最终转写插入逻辑。
请区分 streaming_partial、vad_segment_preview、final_only 三种能力，不支持的 provider 不能显示假实时。
必须用 sessionId/generation 防止旧事件污染新录音。
不要 commit。
```

## 下次继续需要提供

```text
事件名：
payload 结构：
支持实时预览的 provider/model：
不支持时 UI 行为：
取消录音验证结果：
```
