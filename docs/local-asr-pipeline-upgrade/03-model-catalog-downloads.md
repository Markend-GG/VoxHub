# 03-模型 catalog 与下载校验

## 本次只做一个改动点

只完善 sherpa-onnx 模型 catalog、下载、缓存、校验、删除、状态查询。不要改 ASR 推理逻辑。

## 成功标准

- 前端/命令能看到 sherpa 模型列表。
- 每个模型有稳定 alias、显示名、family、mode、languages、required files。
- 下载支持进度、取消、失败重试。
- 缓存目录可定位、可删除、可 reveal。
- 文件缺失时 status 能准确报告。
- 如果复用之前 FunASR 下载器代码，必须把下载地址、模型目录、文件列表和状态字段全部改成 sherpa-onnx ONNX 语义。

## 第一批模型

```text
paraformer-zh
sense-voice-small-zh
zipformer-bilingual-zh-en-streaming（仅为后续实时预览预留，可不默认展示）
```

`whisper-small-multi` 可作为后续 fallback，但本任务不强制加入。

## 下载地址与目录要求

下载地址必须来自 sherpa-onnx 官方文档、k2-fsa 发布资产、HuggingFace 官方/可信镜像，或项目自建 CDN 镜像。不要继续使用 FunASR PyTorch 三模型的 ModelScope 下载地址。

每个 alias 必须显式维护：

```text
alias
source repo / release URL
mirror URL
required files
expected bytes
sha256（能拿到时必须填）
```

目录建议：

```text
%APPDATA%\OpenLess\models\sherpa-onnx\<alias>\
```

不要把模型放进 Git。

## 校验策略

最低要求：

- required files 存在。
- 文件大小不为 0。
- 下载中使用 `.partial`。
- 取消后不留下误判为完整的文件。

更稳要求：

- SHA-256 固定校验。
- 支持镜像源。
- 支持断点续传。

## 预计涉及文件

```text
openless-all/app/src-tauri/src/asr/local/sherpa.rs
openless-all/app/src-tauri/src/asr/local/sherpa_download.rs
openless-all/app/src-tauri/src/commands/sherpa_asr.rs
openless-all/app/src/lib/localAsr.ts
openless-all/app/src/pages/LocalAsr/index.tsx
```

## 不要做

- 不改转写 runtime。
- 不接 Fun-ASR-Nano。
- 不做管线开关。
- 不改默认 provider。
- 不保留 FunASR ModelScope 三模型下载入口。

## 验证命令

```powershell
cd openless-all\app\src-tauri
cargo test sherpa_download
cargo check
```

手动验证：

```text
打开本地 ASR 设置页 -> 选择 sherpa 模型 -> 下载 -> 取消 -> 继续下载 -> 删除 -> reveal 目录
```

## 给 AI 的提示词

```text
请只完善 sherpa-onnx 模型 catalog 和下载校验，不改推理逻辑。
目标模型先覆盖 paraformer-zh、sense-voice-small-zh，并为 streaming zipformer 预留 catalog。
请复用现有下载管理器模式，支持状态、下载、取消、删除、reveal。
如果发现 FunASR 下载器里有可复用的断点续传/进度逻辑，可以迁移；但下载地址、required files、目录和状态字段必须全部变成 sherpa-onnx ONNX 模型。
完成后运行 cargo test sherpa_download 和 cargo check。
不要 commit。
```

## 下次继续需要提供

```text
新增 alias：
每个 alias 的 required files：
模型缓存根目录：
下载验证结果：
失败/取消行为：
```
