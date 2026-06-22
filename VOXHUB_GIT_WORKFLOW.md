# VoxHub Git 结构与 OpenLess 上游同步规则

## 当前结构

VoxHub 仓库现在直接承载 OpenLess 源码本体，不再使用外层仓库套 `openless/` 子仓库的结构。

```text
C:\Users\25512\Documents\语言输入人
  ├─ .git/                 VoxHub Git 仓库
  ├─ openless-all/         OpenLess 应用源码
  ├─ docs/
  ├─ scripts/
  ├─ VIBE_CODING_RULES.md
  └─ VOXHUB_GIT_WORKFLOW.md
```

`ququ/` 不属于 VoxHub 仓库，已经移出当前项目目录。

## Remote 规则

```text
origin   = https://github.com/Markend-GG/VoxHub.git
upstream = https://github.com/Open-Less/openless.git
```

- `origin` 是你的 VoxHub 仓库，用来保存你的二次开发。
- `upstream` 是 OpenLess 官方仓库，只用于拉取官方更新。
- `upstream` 的 push URL 已禁用，避免误推官方仓库。

## 分支规则

- `beta`：当前来自 OpenLess 官方 `beta` 分支的基础分支。
- `codex/<feature-name>`：每个新功能使用一个独立开发分支。
- 不要直接在稳定分支上做大改。

示例：

```powershell
git checkout -b codex/copy-optimized-prompt
```

## 日常开发规则

每次开发前：

```powershell
git status --short
git branch -vv
git remote -v
```

每次让 AI 开发时，建议这样说：

```text
请按 VoxHub 上游兼容模式开发：
1. 先检查 git 状态、当前分支和 remote。
2. 不要直接改稳定分支，先创建 codex/功能名 分支。
3. 尽量用新增模块或最小改动，避免破坏 OpenLess 上游同步。
4. 改完后运行本地验证。
5. 总结改动文件、验证结果、未来和 upstream 合并时可能冲突的位置。
6. 不要 commit，不要 push，等我确认。
```

## 同步 OpenLess 官方更新

同步前必须保证当前工作区干净，或者先把你的改动提交到功能分支。

标准流程：

```powershell
git fetch upstream
git checkout beta
git merge upstream/beta
```

如果有冲突：

1. 先停止继续合并。
2. 让 AI 列出冲突文件。
3. 逐个解释冲突原因。
4. 只解决冲突，不顺手重构。
5. 合并完成后运行本地验证。

你可以这样对 AI 说：

```text
请帮我同步 OpenLess 官方 upstream 更新：
1. 先检查当前是否有未提交改动。
2. fetch upstream。
3. 比较 upstream/beta 和当前 beta 的差异。
4. 给我同步方案。
5. 合并前不要改代码。
6. 如果需要解决冲突，先告诉我冲突文件和建议。
```

## Commit 与 Push 规则

AI 不允许自动 commit 或 push。

提交前必须先让 AI 输出：

- 当前分支。
- `git status --short`。
- 将提交的文件列表。
- 简短改动总结。
- 建议 commit message。

只有你明确说“提交”后，才能执行：

```powershell
git add <files>
git commit -m "message"
```

只有你明确说“push 到 GitHub”后，才能执行：

```powershell
git push -u origin <branch>
```

## 不要做的事

- 不要把官方 `upstream` 当成自己的推送目标。
- 不要直接 `git add .`，除非已经确认没有缓存、构建产物、密钥或无关目录。
- 不要把下载目录、临时构建目录、日志目录提交进仓库。
- 不要在同步 upstream 时顺手做产品功能改动。
- 不要在功能开发时顺手做大范围格式化。

