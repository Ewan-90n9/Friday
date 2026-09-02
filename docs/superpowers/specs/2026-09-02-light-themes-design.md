# Friday 浅色主题设计

日期：2026-09-02
状态：已实现
关联 issue：MRLA-3（给 Friday 增加浅色主题）

## 背景

Friday 此前仅有暗色主题（纯黑基底）。用户习惯浅色界面，要求增加 2 个浅色主题，并保持现有暗色为默认（设计原则"暗色为本"）。

## 目标

- 新增 2 个浅色主题：**浅色**（冷调中性）与**暖白**（纸感暖调）
- 暗色保持默认且视觉零变化
- 顶栏一键切换、选择持久化，重启不闪暗色
- 全部组件无硬编码颜色，浅色下满足设计语言的对比度要求（正文 4.5:1、图标 3:1）

## 方案选型

| 方案 | 结论 |
|------|------|
| **A. CSS 变量多主题**（`:root` 暗色 + `[data-theme]` 覆盖 + `@theme inline` 映射 `--color-*: var(--*)`） | ✅ 采用。shadcn/Tailwind v4 标准模式；组件层零改动（现有代码已全走语义 token）；暗色值原样保留 |
| B. 后端 SQLite 存主题偏好（IPC command） | ❌ 主题是纯 UI 偏好，localStorage（WebView 持久化）足够，避免 Rust 侧改动 |
| C. Tailwind `dark:` 变体逐组件适配 | ❌ 侵入全部组件，工作量大且易漏 |

## 实现

### 1. 主题机制（`src/styles/globals.css`）

- 原暗色 token 值从 `@theme inline` 挪到 `:root`（值逐字节不变），`@theme inline` 改为 `--color-*: var(--*)` 引用——工具类内联 `var(--background)` 等，随 `<html data-theme>` 级联切换
- `[data-theme="light"]` / `[data-theme="warm"]` 覆盖全部语义变量
- 基础样式主题化：滚动条（`--scrollbar-thumb[-hover]`）、文本选中（`--selection-*`）、弹窗遮罩（`--dialog-backdrop`）、`color-scheme`（dark/light，保证原生控件一致）
- 原 `* { color-scheme: dark }` 收敛到主题块内声明

### 2. 色板设计

浅色语义色整体加深一档（Blue-600 / Green-700 / Red-600 / Amber-700），保证浅底上正文级对比度（4.5:1~5.2:1）；暗色的 500 系 + 黑前景（success/warning）保持不变。暖白与浅色共用语义色，仅表面层用 Stone 暖调（背景 `#FAF7F1`、前景 `#292524`）。品牌蓝 `#2563EB` 两个浅色主题统一，保持品牌一致性。完整色板见 [设计语言 · 多主题](../../design/design-language.md)。

### 3. 切换与持久化（前端）

- `src/store/themeStore.ts`：`dark | light | warm` 三态 zustand store；`setTheme` 写 `<html data-theme>` + localStorage（`friday.theme`，非法值回落暗色）
- `src/main.tsx`：`createRoot` 前应用持久化主题，浅色用户启动无暗色闪烁
- `src/components/layout/ThemeMenu.tsx`：顶栏调色板按钮 + 弹出菜单（menuitemradio、Check 标记当前主题、点击外部/Esc 关闭、焦点归还触发器）
- `InputArea.tsx` 发送按钮 `text-white` → `text-accent-foreground`（消除最后一处组件硬编码颜色；暗色下值相同 `#FFFFFF`）

## 验证

- `pnpm typecheck` / `pnpm build` 通过
- 构建产物 CSS 含 `[data-theme="light"]`/`[data-theme="warm"]` 覆盖块，工具类（如 `bg-background`）编译为 `var(--background)` 引用
- 组件无硬编码颜色（FridayMark 为品牌徽标，黑底白 F 与页面主题无关，不改）

## 后续演进

- 跟随系统 `prefers-color-scheme`（未存储偏好时）
- 主题预览色卡（菜单项内嵌色板示意）
