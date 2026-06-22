# 14-打包、测试与发布

## 本次只做一个改动点

只做 Windows 打包、测试矩阵和发布前风险收口。不要新增功能。

## 风险重点

- `sherpa-onnx` native 库是否正确随包。
- ONNX Runtime / sherpa DLL 是否能在安装目录加载。
- 模型不进安装包，只在首次使用时下载。
- 中文路径、空格路径、长路径。
- 低配 CPU 上首句延迟。
- provider 切换后的 runtime 释放。

## 必测矩阵

| 场景 | 必测 |
|---|---|
| 无网络启动 | 是 |
| 无模型启动 | 是 |
| 首次下载模型 | 是 |
| 模型损坏 | 是 |
| 中文路径安装/运行 | 是 |
| 空格路径安装/运行 | 是 |
| Paraformer ONNX 转写 | 是 |
| SenseVoice Small 转写 | 如果已接入 |
| Fun-ASR-Nano ONNX 模型 | 如果已确认并接入 |
| VAD 开/关 | 如果已接入 |
| 标点开/关 | 如果已接入 |
| 说话人识别开/关 | 如果已接入 |
| 实时预览开/关 | 如果已接入 |

## 建议命令

```powershell
cd openless-all\app
corepack.cmd pnpm install
corepack.cmd pnpm tauri build
```

Rust 验证：

```powershell
cd openless-all\app\src-tauri
cargo test
cargo check
```

## 发布前必须确认

```text
git status --short
模型文件未进入 Git
日志文件未进入 Git
__pycache__ 未进入 Git
tauri-dev*.log 未进入 Git
无密钥/Token
```

## 给 AI 的提示词

```text
请只做本地 ASR 升级的打包和测试收口，不新增功能。
重点检查 sherpa-onnx native 库、模型目录、Windows 安装包、中文路径、空格路径、无模型/模型损坏状态。
请运行能承受的测试和构建命令，最后给出发布阻塞项清单。
不要 commit 或 push，除非我明确要求。
```

## 下次继续需要提供

```text
cargo test 结果：
tauri build 结果：
安装包路径：
手测矩阵结果：
发布阻塞项：
```
