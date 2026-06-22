# 01-确认现状与冻结边界

## 本次只做一个改动点

只确认当前 ASR 代码状态、依赖状态、模型缓存状态，并写出本机可复现的基线。不改功能代码。

## 假设

- 当前仓库可能已有未提交改动。
- `funasr-local` 相关文件可能已经存在，但后续目标是回滚；`sherpa-onnx-local` 是唯一保留的本地 ONNX 路线。
- 这一步的目标是避免后续开发在未知状态上继续叠改。

## 成功标准

- 能列出当前 ASR provider、Rust 依赖、FunASR Python 残留入口、sherpa 模型目录状态。
- 能判断 `sherpa-onnx-local` 是否已经能编译。
- 能判断哪些 `funasr-local` 代码需要回滚，哪些下载能力可以迁移复用。
- 生成一份简短现状记录，供下一步使用。

## 建议检查命令

```powershell
git status --short
rg -n "funasr|sherpa|ActiveAsr|AudioConsumer|RawTranscript" openless-all\app\src-tauri\src
Get-Content -Encoding UTF8 openless-all\app\src-tauri\Cargo.toml | Select-String -Pattern "sherpa|funasr|onnx"
cd openless-all\app\src-tauri
cargo check
```

如果 `cargo check` 太慢，至少先运行：

```powershell
cd openless-all\app\src-tauri
cargo check -q
```

## 不要改动

- 不改 provider 默认值。
- 不改 UI。
- 不新增模型。
- 不重构 ASR trait 或 coordinator。
- 不 commit，除非用户明确同意。

## 输出文档模板

建议把结果写到一次性记录里，例如：

```text
docs/local-asr-pipeline-upgrade/status-YYYYMMDD.md
```

内容：

```text
当前分支：
工作树状态：
sherpa-onnx 依赖版本：
FunASR Python 运行时残留入口：
可复用下载能力：
sherpa 模型目录是否存在：
cargo check 结果：
阻塞项：
下一步可执行子文档：02-funasr-python-rollback.md
```

## 给 AI 的提示词

```text
请只做现状确认，不要修改功能代码。
目标：确认当前仓库里的 FunASR Python 残留入口、可复用下载能力、sherpa-onnx provider、Cargo 依赖和模型缓存状态。
请先运行 git status --short，再搜索 funasr/sherpa 相关文件，最后运行能承受的最小验证命令。
输出一份简短状态记录，说明下一步是否可以执行 02-funasr-python-rollback.md。
不要 commit。
```

## 下次继续需要提供

```text
当前分支：
是否有未提交改动：
cargo check 是否通过：
FunASR Python 残留入口清单：
下载功能哪些可以迁移复用：
sherpa-onnx 当前是否能加载任一模型：
```
