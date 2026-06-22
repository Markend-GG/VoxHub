# 02-回滚 FunASR Python 运行时

## 本次只做一个改动点

只回滚之前的 FunASR Python 运行时接入。可以保留下载器里的通用能力，但不能继续保留 FunASR Python provider、sidecar、预检、模型加载或 UI 入口。

## 背景结论

用户已经明确不再需要 FunASR Python。后续本地 ASR 只走：

```text
sherpa-onnx + Paraformer ONNX
```

FunASR Python 相关内容不再作为兼容方案，也不作为 fallback。

## 成功标准

- 移除 `funasr-local` provider 的运行时入口。
- 移除 Python sidecar 文件和调用链。
- 移除 FunASR Python 预检脚本或从主流程中解绑。
- 移除 UI 中的 FunASR provider 选择和状态展示。
- 移除 coordinator / commands / lib.rs 中 FunASR provider 注册。
- 保留或迁移下载器通用能力，但下载目标必须改为 sherpa-onnx ONNX 模型。
- `cargo check` 通过。

## 建议回滚清单

先按实际代码确认，不要盲删：

```text
openless-all/app/src-tauri/python/funasr_server.py
openless-all/app/src-tauri/src/asr/local/funasr_provider.rs
openless-all/app/src-tauri/src/asr/local/funasr_runtime.rs
openless-all/app/src-tauri/src/asr/local/funasr.rs
openless-all/app/src-tauri/src/commands/funasr_asr.rs
scripts/voxhub-funasr-preflight.ps1
openless-all/app/src/i18n/* 中 FunASR 文案
openless-all/app/src/lib/localAsr.ts 中 FunASR 类型/命令
openless-all/app/src/pages/LocalAsr/index.tsx 中 FunASR UI
```

如果某个文件里同时包含通用下载能力，不要整文件删除，先提取或迁移通用逻辑。

## 下载功能保留原则

可以保留：

- 分块下载。
- 断点续传。
- `.partial` / `.partial.idx` 机制。
- 进度事件。
- 取消下载。
- 删除模型。
- reveal 模型目录。
- 镜像源选择。
- SHA-256 或文件完整性校验。

必须修改：

- 下载地址从 FunASR/ModelScope PyTorch 三模型改为 sherpa-onnx ONNX 模型地址。
- 模型目录从 FunASR `modelscope/damo` 语义改为 `%APPDATA%\OpenLess\models\sherpa-onnx\<alias>\`。
- required files 从 `model.pt` / `pytorch_model.bin` 改为 `model.int8.onnx`、`tokens.txt` 等 sherpa 模型文件。
- 状态字段从 `asr/vad/punc 三模型` 改为 `alias + required files + cached/downloadedBytes/expectedBytes`。
- UI 文案从 FunASR 改为 sherpa-onnx / Paraformer ONNX。

## 不要做

- 不接 Paraformer ONNX 推理。
- 不做 VAD / 标点 / 说话人识别。
- 不做实时预览。
- 不新增 SenseVoice / Paraformer Large / Fun-ASR-Nano。
- 不把 FunASR Python 留作 fallback。

## 验证命令

```powershell
rg -n "funasr|FunASR|funasr-local|funasr_server|voxhub-funasr" openless-all scripts docs
cd openless-all\app\src-tauri
cargo check
```

允许 `docs/local-asr-pipeline-upgrade/` 里出现 FunASR 字样，因为这里记录的是回滚任务；功能代码里不应再出现运行时接入。

## 给 AI 的提示词

```text
请按 docs/local-asr-pipeline-upgrade/02-funasr-python-rollback.md 执行。
本次只做一个改动点：回滚 FunASR Python 运行时接入。
可以保留下载器的通用能力，但必须把下载目标、模型目录、required files、状态字段改为 sherpa-onnx ONNX 模型语义。
不要接 Paraformer ONNX 推理，不要做 VAD/标点/说话人/实时预览。
先运行 git status --short，识别 FunASR 相关改动，再给出最小回滚方案。
不要 commit，除非我明确要求。
```

## 下次继续需要提供

```text
已移除的 FunASR 文件/入口：
保留的下载通用能力：
已改成的 sherpa 下载字段：
仍残留的 FunASR 文档或测试引用：
cargo check 结果：
```
