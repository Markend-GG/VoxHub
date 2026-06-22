# 16-Sherpa FunASR 模型 Provider

## 目标

新增 Windows 本地 ASR provider：`funasr-models`，设置页展示名为 `Sherpa FunASR`。它只作为 FunASR 相关 ONNX 模型的选择和状态隔离层，不恢复 FunASR Python sidecar，不新增 HTTP 进程，也不引入 ModelScope PyTorch 运行时。

底层必须复用现有 `sherpa-onnx` runtime、下载器、缓存目录和批量转写链路。下载完成只标记模型可用，不自动切换 provider；用户点击“选用/启用”后才把 `activeAsrProvider` 切到 `funasr-models`。

## 范围

- Provider ID：`funasr-models`
- UI 展示名：`Sherpa FunASR`
- 第一版模型：
  - `paraformer-zh`：`model.int8.onnx`, `tokens.txt`
  - `sense-voice-small-zh`：`model.int8.onnx`, `tokens.txt`
- 复用 sherpa 下载能力：状态、下载、取消、删除、reveal、`.partial`、断点进度、size/sha 校验。
- 复用 sherpa runtime：`OfflineRecognizer` + Paraformer/SenseVoice 配置 + `RawTranscript` 输出，并继续走 polish、insert、history 链路。

## 不做

- 不恢复 FunASR Python / HTTP sidecar。
- 不接 ModelScope PyTorch 下载入口。
- 不新增 Paraformer ONNX 之外的推理链路。
- 不做 VAD、标点、说话人、实时预览、情绪/事件标签 UI。
- 不接 Paraformer Large、Fun-ASR-Nano、Whisper、Qwen3、Zipformer 到该 provider 视图。
- 不改变原有 sherpa、Foundry、Qwen3、云端 ASR 行为。

## 实现要点

1. 后端增加 `funasr_models` 封装：
   - `PROVIDER_ID = "funasr-models"`
   - `DEFAULT_MODEL_ALIAS = "paraformer-zh"`
   - catalog 从现有 sherpa catalog 过滤出 `paraformer-zh`、`sense-voice-small-zh`
   - model dir、download、delete、reveal 直接委托 sherpa 实现
2. 命令层新增 `funasr_models_asr_*` Tauri commands：
   - status/catalog/fetch_remote_info/download/cancel_download/delete/reveal
   - set_model/set_language_hint/prepare/cancel_prepare/release/model_dir
   - 返回结构复用 sherpa 序列化字段，前端不维护第二套 schema
3. 偏好设置增加独立字段：
   - `funasrModelsModel`，默认 `paraformer-zh`
   - `funasrModelsLanguageHint`，默认空
   - `funasrModelsKeepLoadedSecs`，默认沿用 sherpa 当前默认值
4. coordinator 接入：
   - `activeAsrProvider === "funasr-models"` 时读取 `funasrModelsModel`
   - 使用现有 `SherpaOnnxAsr::new_for_model(...)` 和同一个 `SherpaOnnxRuntime`
   - credentials 校验把 `funasr-models` 视为 Windows keyless local ASR
   - provider 切换释放策略与 sherpa 一致，不影响原有 ASR provider
5. 前端 Local ASR 设置页：
   - 新增 `Sherpa FunASR` 卡片或分组
   - 只展示两个 FunASR 模型
   - 下载完成后刷新 catalog，按钮状态变为已下载/可选用
   - 用户点击选用后才调用 set active provider + set model

## 验证计划

- Rust：
  - alias validation 只接受 `paraformer-zh` 和 `sense-voice-small-zh`
  - catalog filtering 不包含 Whisper/Qwen3/Zipformer
  - credentials/keyless：`funasr-models` 不要求 API key
  - coordinator 使用 `funasrModelsModel` 创建 `SherpaOnnxAsr`
  - download delegation 与 sherpa 对同 alias 的状态一致
- Frontend：
  - TypeScript 编译通过
  - UI 只展示两个 FunASR 模型
  - 下载完成不自动修改 `activeAsrProvider`
  - 点击选用后 `activeAsrProvider === "funasr-models"`
- Manual smoke：
  - 下载 Paraformer ONNX，手动选用，中文短句能转文字
  - 下载 SenseVoice Small，手动选用，中英混合短句能转文字
  - 删除模型后状态变为未下载，reveal 能打开对应目录
  - 原有 ASR provider 选择、下载、转写路径不回归

## 风险

- 当前工作树已有大量未提交改动，本任务不得 revert 或整理无关 diff。
- 如果本机仍是 `x86_64-pc-windows-gnu` 且缺少 `dlltool.exe`，`cargo check` / `cargo test` 可能被工具链阻断；需要记录准确阻断信息。
- 本文档是目标模式执行入口；如代码与文档不一致，以最新用户要求和当前代码为准。
