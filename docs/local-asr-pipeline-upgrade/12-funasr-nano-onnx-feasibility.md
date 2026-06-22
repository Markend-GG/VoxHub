# 12-Fun-ASR-Nano ONNX 可行性评估

## 本次只做一个改动点

只评估 Fun-ASR-Nano 是否存在稳定 ONNX/sherpa-onnx 路线。没有稳定 ONNX 时不接入。

## 推荐判断

Fun-ASR-Nano 不再通过 FunASR Python/HTTP 服务接入。只有满足下面条件才进入实现：

```text
有官方或可信 ONNX 模型
能被 sherpa-onnx 或现有 Rust ONNX runtime 稳定加载
许可允许随产品下载/使用
required files、体积、sha256 可固定
```

如果不满足，结论就是暂缓。

## 成功标准

- 找到或排除 Fun-ASR-Nano 的 ONNX/sherpa-onnx 可行路线。
- 如果可行，输出 alias、下载地址、required files、sha256、capabilities。
- 如果不可行，文档明确写“暂缓”，并说明缺少什么。
- 不新增 Python runtime。

## 不要做

- 不通过 FunASR Python/HTTP sidecar 接入。
- 不把 Fun-ASR-Nano 塞进 `sherpa-onnx-local`，除非已有官方 ONNX + sherpa 支持。
- 不改变 Paraformer/SenseVoice 行为。
- 不把复杂 metadata 直接插入正文。

## 验证样例

```text
中文普通话
中英混合
长句
带热词短句
多人音频（如果启用说话人）
```

## 给 AI 的提示词

```text
请只评估 Fun-ASR-Nano 的 ONNX/sherpa-onnx 可行性。
不要使用 FunASR Python/HTTP 服务或 sidecar。
如果没有稳定 ONNX 模型，请输出暂缓结论，不要写实现代码。
如果可行，再输出 alias、下载地址、required files、sha256、capabilities。
不要改 Paraformer 和 SenseVoice。
不要 commit。
```

## 下次继续需要提供

```text
结论：可行 / 暂缓
模型来源链接：
模型目录：
required files：
sha256：
capabilities：
暂缓原因（如有）：
```
