# 05-管线开关契约：VAD / 标点 / 说话人

## 本次只做一个改动点

只定义后端管线开关的数据契约和能力判断，不实现具体 VAD、标点、说话人算法。

## 为什么先做契约

VAD、标点、说话人识别看起来都是 ASR 附属功能，但它们并不都属于同一层：

- VAD 是音频切分。
- 标点恢复是文本后处理，也可能是模型内置输出。
- 说话人识别是音频后处理，需要 segment 对齐。
- 实时预览是交互事件流，不是最终文本本身。

如果不先定义契约，后续每接一个模型都会重复写一套特殊逻辑。

## 推荐最小结构

先新增或等价表达：

```rust
pub struct AsrPipelineOptions {
    pub use_vad: bool,
    pub use_punctuation: bool,
    pub use_speaker_diarization: bool,
    pub realtime_preview: bool,
    pub language_hint: Option<String>,
}

pub struct AsrModelCapabilities {
    pub vad: CapabilitySupport,
    pub punctuation: CapabilitySupport,
    pub speaker_diarization: CapabilitySupport,
    pub realtime_preview: CapabilitySupport,
    pub hotwords: CapabilitySupport,
}

pub enum CapabilitySupport {
    Unsupported,
    Supported,
    Experimental,
}
```

实际命名应匹配当前代码风格。

## 数据流建议

```text
UserPreferences
  -> provider/model capabilities
  -> effective pipeline options
  -> provider transcribe/stream
  -> RawTranscript
```

关键点：保存的是用户偏好，执行的是 `effective options`。如果某模型不支持说话人识别，用户偏好里开了也不能硬执行。

## 成功标准

- 每个本地 ASR provider 能返回 capability snapshot。
- 设置项可以保存，但不会影响现有推理。
- 不支持的开关在 effective options 中被禁用。
- 单元测试覆盖 capability 合并逻辑。

## 预计涉及文件

```text
openless-all/app/src-tauri/src/types.rs
openless-all/app/src-tauri/src/asr/local/sherpa.rs
openless-all/app/src-tauri/src/commands/*.rs
openless-all/app/src/lib/localAsr.ts
```

## 不要做

- 不接真实 VAD。
- 不接真实标点。
- 不接真实说话人识别。
- 不做 UI 大改，只做必要字段和状态展示。

## 验证命令

```powershell
cd openless-all\app\src-tauri
cargo test capability
cargo check
```

## 给 AI 的提示词

```text
请只实现 ASR 管线开关契约和 capability 判断，不接真实算法。
需要支持 use_vad、use_punctuation、use_speaker_diarization、realtime_preview 四个偏好字段。
请设计 effective options：用户偏好必须经过 provider/model capabilities 过滤。
不要改现有转写结果，不要改默认 provider。
完成后补单元测试并运行 cargo check。
```

## 下次继续需要提供

```text
新增偏好字段：
capability 类型位置：
每个 provider 当前 capability：
effective options 测试结果：
```
