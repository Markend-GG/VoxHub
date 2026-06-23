# 文本重写 Phase 5：风格包用途分类

状态：Draft v2（基于用户纠正后的需求）
日期：2026-06-22
前置：Phase 1-4 已完成并提交（commit 40832a6b）

## 1. 需求概述

### 核心要求

1. 风格包明确分为「语音」和「重写」两类，无共享类别
2. 不支持"从语音风格派生重写风格"功能
3. 仅支持四个基本操作：市场下载、编辑、应用、新建
4. 重写风格复用语音风格的基础设施（存储、编辑、市场），但运行完全隔离

### 用户确认的关键决策

| 决策项 | 结论 |
|--------|------|
| 存储方案 | 同一 `style-packs.json`，通过 `scope` 字段区分 |
| 市场下载 | 安装时弹出选择框，让用户选择「语音」或「重写」 |
| 重写激活 | 默认使用内置风格包（支持编辑），可切换为市场/自定义包 |

## 2. 数据模型设计

### 2.1 StylePackScope 枚举

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum StylePackScope {
    /// 语音听写润色风格。旧包无此字段时默认此值。
    #[default]
    Voice,
    /// 文本重写风格。
    Rewrite,
}
```

**设计要点**：
- 仅两个变体，无 Shared——语音和重写完全隔离
- `Default = Voice`：旧 JSON 缺少 `scope` 字段时自动归为语音，零迁移

### 2.2 StylePack 结构体扩展

```rust
pub struct StylePack {
    // ... 现有字段全部保留，不做任何改动 ...

    /// 风格包用途：Voice 或 Rewrite。
    /// 旧包无此字段时 serde default 为 Voice，保证向后兼容。
    #[serde(default)]
    pub scope: StylePackScope,  // 新增唯一字段
}
```

### 2.3 StylePack Default 实现

```rust
impl Default for StylePack {
    fn default() -> Self {
        Self {
            // ... 现有字段不变 ...
            scope: StylePackScope::Voice,  // 新增
        }
    }
}
```

### 2.4 前端 TypeScript 类型

```typescript
export type StylePackScope = 'voice' | 'rewrite';

export interface StylePack {
  // ... 现有字段 ...
  scope?: StylePackScope;  // 可选，缺省视为 'voice'
}
```

### 2.5 UserPreferences 扩展

```rust
pub struct UserPreferences {
    // ... 现有字段 ...

    /// 文本重写当前激活的风格包 ID。
    /// None = 使用内置默认重写风格包 (builtin.rewrite)。
    #[serde(default)]
    pub active_rewrite_style_pack_id: Option<String>,
}
```

**设计要点**：
- `Option<String>`：None 时自动使用 `builtin.rewrite`
- 与 `active_style_pack_id`（语音）完全独立，切换互不影响

## 3. 内置重写风格包

### 3.1 定义

```rust
fn default_rewrite_style_pack() -> StylePack {
    StylePack {
        id: "builtin.rewrite".to_string(),
        name: "智能重写".to_string(),
        description: "改善表达的流畅度和清晰度，修正语法和标点错误，保持原文语气和正式程度。".to_string(),
        author: Some("OpenLess".to_string()),
        version: "1.0.0".to_string(),
        kind: StylePackKind::Builtin,
        base_mode: PolishMode::Light,
        prompt: DEFAULT_REWRITE_STYLE_PROMPT.to_string(),
        examples: vec![],
        tags: vec!["重写".to_string()],
        icon_path: None,
        created_at: None,
        updated_at: None,
        enabled: true,
        active: false,
        recommended_model: None,
        compatible_app_version: None,
        origin_pack_id: None,
        origin_author_login: None,
        scope: StylePackScope::Rewrite,
    }
}
```

### 3.2 自动创建

在 `StylePackStore::new()` 中检查：如果 `style-packs.json` 中不存在 `builtin.rewrite`，自动创建。与现有 builtin 包（raw/light/structured/formal）的迁移逻辑一致。

### 3.3 可编辑

`builtin.rewrite` 与现有 builtin 语音包一样支持编辑 prompt。用户可在编辑器中修改其 prompt 内容。`reset_builtin` 功能同样可用——重置回默认 prompt。

## 4. 风格包隔离机制

### 4.1 列表过滤

```rust
impl StylePackStore {
    /// 列出指定 scope 的风格包。
    /// - Voice 返回 scope == Voice 的包
    /// - Rewrite 返回 scope == Rewrite 的包
    /// 两者互不包含对方的包。
    pub fn list_by_scope(&self, scope: StylePackScope) -> Result<Vec<StylePack>> {
        let all = self.list()?;
        Ok(all.into_iter().filter(|p| p.scope == scope).collect())
    }
}
```

**完全隔离**：语音列表只显示 Voice 包，重写列表只显示 Rewrite 包。没有 Shared 概念。

### 4.2 激活状态隔离

| 维度 | 语音风格 | 重写风格 |
|------|---------|---------|
| 激活 ID | `prefs.active_style_pack_id` (String) | `prefs.active_rewrite_style_pack_id` (Option\<String\>) |
| 默认值 | `"builtin.light"` | `None` → fallback 到 `builtin.rewrite` |
| 激活命令 | `set_active_style_pack(id)` | `set_active_rewrite_style_pack(id)` |
| 列表 active 标记 | `id == active_style_pack_id` | `id == active_rewrite_style_pack_id.unwrap_or("builtin.rewrite")` |

### 4.3 启用/禁用隔离

- 禁用一个 Voice 包不影响 Rewrite 列表
- 禁用一个 Rewrite 包不影响 Voice 列表
- 删除 Rewrite 包时，如果它恰好是 `active_rewrite_style_pack_id`，重置为 None（fallback 到 builtin.rewrite）
- `ensure_at_least_one_style_pack_enabled` 需要按 scope 分别检查

### 4.4 重写编排集成

修改 `rewrite_flow.rs`：

```rust
// 读取重写风格 prompt
let style_prompt = match &prefs.active_rewrite_style_pack_id {
    Some(id) => match inner.style_packs.get(id) {
        Ok(pack) if pack.enabled => pack.prompt.clone(),
        _ => {
            // ID 不存在或包被禁用 → fallback 到 builtin.rewrite
            inner.style_packs.get("builtin.rewrite")
                .map(|p| p.prompt.clone())
                .unwrap_or_else(|_| DEFAULT_REWRITE_STYLE_PROMPT.to_string())
        }
    },
    None => {
        // 未选择 → 使用 builtin.rewrite
        inner.style_packs.get("builtin.rewrite")
            .map(|p| p.prompt.clone())
            .unwrap_or_else(|_| DEFAULT_REWRITE_STYLE_PROMPT.to_string())
    }
};
```

**三级 fallback**：用户选择的包 → builtin.rewrite → 硬编码 DEFAULT_REWRITE_STYLE_PROMPT

### 4.5 历史记录关联

```rust
RewriteHistoryEntry {
    // ...
    style_pack_id: prefs.active_rewrite_style_pack_id.clone()
        .or_else(|| Some("builtin.rewrite".into())),
    style_pack_name: resolve_rewrite_pack_name(inner, &prefs),
    // ...
}
```

## 5. 市场下载流程

### 5.1 安装交互

用户在 Marketplace 页面点击"安装"按钮时，弹出选择对话框：

```
┌──────────────────────────────┐
│  安装风格包                    │
│                              │
│  请选择安装为哪种风格：         │
│                              │
│  ( ) 语音风格（用于听写润色）   │
│  (●) 重写风格（用于文本重写）   │
│                              │
│         [取消]  [确认安装]     │
└──────────────────────────────┘
```

**默认选中逻辑**：根据当前页面来源决定
- 从语音风格页面跳转进入市场 → 默认选中"语音风格"
- 从重写风格页面跳转进入市场 → 默认选中"重写风格"
- 直接打开市场页面 → 默认选中"语音风格"

### 5.2 后端安装命令扩展

```rust
#[tauri::command]
async fn marketplace_install(
    coord: CoordinatorState<'_>,
    app: AppHandle,
    pack_id: String,
    scope: Option<StylePackScope>,  // 新增：安装为哪种用途
) -> Result<StylePack, String>;
```

**安装行为**：
1. 从市场下载 ZIP → 解压 → 读取 StylePack JSON
2. 生成新 ID（`imported-{uuid}`），设置 `kind = Imported`
3. 设置 `scope` 为安装时用户选择的值（`scope.unwrap_or(StylePackScope::Voice)`）
4. 绑定 `origin_pack_id` / `origin_author_login`
5. 写入 `style-packs.json`

### 5.3 前端安装流程

```typescript
// Marketplace.tsx 中修改安装逻辑
const [installScopeDialog, setInstallScopeDialog] = useState<{
  packId: string;
  packName: string;
} | null>(null);
const [installScope, setInstallScope] = useState<StylePackScope>('voice');

// 点击安装按钮 → 弹出选择对话框（不再直接安装）
const onInstallClick = (packId: string, packName: string) => {
  setInstallScopeDialog({ packId, packName });
  setInstallScope(entryFromVoiceTab ? 'voice' : 'rewrite');
};

// 确认安装
const onConfirmInstall = async () => {
  if (!installScopeDialog) return;
  await marketplaceInstall(installScopeDialog.packId, installScope);
  setInstallScopeDialog(null);
};
```

## 6. 前端风格页面分段

### 6.1 Style.tsx 页面结构

```
┌──────────────────────────────────────────────┐
│  PageHeader: 风格管理                         │
│  [风格市场] [刷新] [导入 ZIP]                  │
├──────────────────────────────────────────────┤
│  ┌──────────────┐ ┌──────────────┐           │
│  │ ● 语音风格    │ │   重写风格    │           │  ← 分段 Tab
│  └──────────────┘ └──────────────┘           │
├──────────────────────────────────────────────┤
│                                              │
│  === 语音风格 Tab ===                         │
│  保持现有布局完全不变                          │
│  raw (pill) + builtin 卡片 + imported 卡片    │
│                                              │
│  === 重写风格 Tab ===                         │
│  当前激活风格提示                              │
│  builtin.rewrite 卡片 + imported 卡片         │
│  [新建重写风格] 按钮                           │
│                                              │
└──────────────────────────────────────────────┘
```

### 6.2 Tab 状态管理

```typescript
type StyleTab = 'voice' | 'rewrite';
const [styleTab, setStyleTab] = useState<StyleTab>('voice');
```

### 6.3 列表过滤

```typescript
// 语音 Tab：只显示 Voice 包
const voicePacks = packs.filter(p => (p.scope ?? 'voice') === 'voice');

// 重写 Tab：只显示 Rewrite 包
const rewritePacks = packs.filter(p => (p.scope ?? 'voice') === 'rewrite');
```

### 6.4 重写 Tab 布局

```tsx
{styleTab === 'rewrite' && (
  <>
    {/* 当前激活提示 */}
    {activeRewritePack && (
      <div style={{ fontSize: 12, color: 'var(--ol-ink-4)', marginBottom: 8 }}>
        当前重写风格：{activeRewritePack.name}
      </div>
    )}

    {/* builtin.rewrite 卡片（始终在最前面） */}
    {builtinRewritePack && (
      <PackCard
        pack={builtinRewritePack}
        isActive={activeRewriteId === builtinRewritePack.id}
        onActivate={() => setActiveRewriteStylePack(builtinRewritePack.id)}
        onEdit={() => openEditor(builtinRewritePack)}
        scope="rewrite"
      />
    )}

    {/* imported Rewrite 包 */}
    {importedRewritePacks.map(pack => (
      <PackCard
        key={pack.id}
        pack={pack}
        isActive={activeRewriteId === pack.id}
        onActivate={() => setActiveRewriteStylePack(pack.id)}
        onEdit={() => openEditor(pack)}
        onDelete={() => deleteStylePack(pack.id)}
        scope="rewrite"
      />
    ))}

    {/* 新建按钮 */}
    <button onClick={() => createNewRewritePack()}>
      + 新建重写风格
    </button>
  </>
)}
```

### 6.5 激活管理

```typescript
// 语音风格：复用现有逻辑，不变
const setActiveVoiceStyle = async (id: string) => {
  await setActiveStylePack(id);  // 现有命令
};

// 重写风格：新命令
const setActiveRewriteStyle = async (id: string) => {
  await setActiveRewriteStylePack(id);  // 新命令
};

// 恢复默认（builtin.rewrite）
const resetRewriteToDefault = async () => {
  await setActiveRewriteStylePack(null);  // null = 使用 builtin.rewrite
};
```

### 6.6 编辑器扩展

侧滑编辑器中，对 Imported 包显示 scope 选择器：

```tsx
{/* 仅 Imported 包可修改 scope，Builtin 包锁定 */}
{editing.kind === 'imported' && (
  <label>
    用途
    <select
      value={editing.scope ?? 'voice'}
      onChange={e => setEditing({...editing, scope: e.target.value as StylePackScope})}
    >
      <option value="voice">语音风格</option>
      <option value="rewrite">重写风格</option>
    </select>
  </label>
)}
```

### 6.7 新建重写风格

```typescript
const createNewRewritePack = async () => {
  const template: StylePack = {
    id: '',  // 后端生成
    name: '新建重写风格',
    description: '',
    kind: 'imported',
    baseMode: 'light',
    prompt: '改善表达的流畅度和清晰度，修正语法和标点错误，保持原文语气和正式程度。',
    examples: [],
    tags: [],
    enabled: true,
    scope: 'rewrite',  // 关键：标记为重写
    // ...
  };
  const created = await createStylePackFromTemplate(template);
  // 切换到重写 Tab 并选中
  setStyleTab('rewrite');
  setSelectedId(created.id);
  openEditor(created);
};
```

### 6.8 市场入口区分

从 Style.tsx 跳转市场时携带来源信息：

```typescript
// 语音 Tab 中的"风格市场"按钮
const openMarketFromVoice = () => {
  setMarketSource('voice');
  setMarketplaceOpen(true);
};

// 重写 Tab 中的"风格市场"按钮
const openMarketFromRewrite = () => {
  setMarketSource('rewrite');
  setMarketplaceOpen(true);
};
```

Marketplace 组件接收 `defaultScope` 参数，决定安装对话框默认选中项。

## 7. 新增 Tauri Commands

| Command | 签名 | 说明 |
|---------|------|------|
| `set_active_rewrite_style_pack` | `(id: Option<String>) -> Result<(), String>` | 设置重写激活包，None = 恢复默认 builtin.rewrite |

**不新增** `derive_style_pack_for_rewrite`——已移除派生功能。

## 8. 修改文件清单

### 后端（Rust）

| 文件 | 修改内容 | 影响评估 |
|------|---------|---------|
| `types.rs` | 新增 `StylePackScope` 枚举；`StylePack` 新增 `scope` 字段；`StylePack::default()` 新增 `scope: Voice`；`UserPreferences` 新增 `active_rewrite_style_pack_id` | 仅新增字段，不改动现有字段 |
| `persistence/style_pack.rs` | `new()` 中检查并创建 `builtin.rewrite`；新增 `list_by_scope` 方法；`remove_imported` 中检查是否为当前 rewrite active 并重置；`sync_style_pack_preferences` 不受影响（只管语音侧） | 新增逻辑，不修改现有逻辑 |
| `commands/style_packs.rs` | 新增 `set_active_rewrite_style_pack` 命令；`save_style_pack` 中支持 scope 字段保存 | 新增命令 |
| `commands/marketplace.rs` | `marketplace_install` 增加 `scope` 参数 | 扩展参数，不改现有逻辑 |
| `coordinator/rewrite_flow.rs` | 从 `active_rewrite_style_pack_id` 读取 prompt，三级 fallback | 替换硬编码 prompt |

### 前端（TypeScript）

| 文件 | 修改内容 | 影响评估 |
|------|---------|---------|
| `lib/types.ts` | `StylePack` 接口新增 `scope` 可选字段 | 仅新增，不改动现有字段 |
| `lib/ipc/style-packs.ts` | 新增 `setActiveRewriteStylePack` 函数 | 新增 |
| `lib/ipc/index.ts` | 导出新函数 | 新增 |
| `pages/Style.tsx` | 新增分段 Tab；重写 Tab 布局；scope 过滤；编辑器 scope 选择；新建重写风格按钮 | 新增 Tab 和重写布局，语音 Tab 完全不动 |
| `pages/Marketplace.tsx` | 安装按钮改为弹出 scope 选择对话框；接收 `defaultScope` 参数 | 修改安装交互 |
| `i18n/*.ts` | 新增 scope/重写相关文案 | 新增 |

## 9. 兼容性保障

### 9.1 向后兼容（旧数据 → 新版本）

| 数据 | 旧版内容 | 新版处理 |
|------|---------|---------|
| `style-packs.json` | 无 `scope` 字段 | `#[serde(default)]` → 自动填充 `Voice` |
| `preferences.json` | 无 `active_rewrite_style_pack_id` | `#[serde(default)]` → 自动填充 `None` |
| 前端 StylePack 接口 | 无 `scope` 属性 | `scope?: StylePackScope`，代码中 `p.scope ?? 'voice'` |

### 9.2 语音功能零影响验证清单

| 验证项 | 预期 |
|--------|------|
| `list_style_packs` 返回的语音包 | 与改动前完全一致 |
| `set_active_style_pack` 行为 | 不受影响 |
| `save_style_pack` 编辑语音包 | scope 默认为 Voice，不影响编辑内容 |
| `marketplace_install`（不传 scope） | 默认 Voice，行为与改动前一致 |
| `set_style_pack_enabled` 禁用语音包 | 不影响重写列表 |
| `delete_style_pack` 删除语音包 | 不影响重写激活状态 |
| 语音听写润色 | prompt 来源不变 |
| 语音历史 | 不受影响 |
| 胶囊窗口 | 不受影响 |

### 9.3 降级兼容（新版本 → 旧版本）

如果用户降级到无 scope 的版本：
- `scope` 字段被旧版 serde 忽略（不识别的字段跳过）
- `active_rewrite_style_pack_id` 被旧版忽略
- 所有 Rewrite 包会显示在语音风格列表中（因为旧版无过滤逻辑）
- 重写功能整体不可用，但不会崩溃

## 10. 实施顺序

| 步骤 | 内容 | 依赖 | 预估改动量 |
|------|------|------|-----------|
| 1 | `StylePackScope` 枚举 + `StylePack.scope` 字段 | 无 | ~20 行 |
| 2 | `UserPreferences.active_rewrite_style_pack_id` | 无 | ~5 行 |
| 3 | `builtin.rewrite` 内置包 + 迁移逻辑 | 步骤 1 | ~30 行 |
| 4 | `StylePackStore::list_by_scope` 方法 | 步骤 1 | ~10 行 |
| 5 | `set_active_rewrite_style_pack` 命令 | 步骤 2 | ~30 行 |
| 6 | `rewrite_flow.rs` 集成风格 prompt | 步骤 2, 3 | ~20 行 |
| 7 | `marketplace_install` scope 参数 | 步骤 1 | ~10 行 |
| 8 | `remove_imported` 重置 rewrite active | 步骤 2 | ~5 行 |
| 9 | 前端 types.ts + IPC 封装 | 步骤 5 | ~15 行 |
| 10 | Style.tsx 分段 Tab + 重写布局 | 步骤 9 | ~150 行 |
| 11 | Marketplace.tsx 安装 scope 选择 | 步骤 9 | ~60 行 |
| 12 | i18n 文案 | 步骤 10, 11 | ~30 行 |

总计：后端 ~130 行新增，前端 ~255 行新增。
