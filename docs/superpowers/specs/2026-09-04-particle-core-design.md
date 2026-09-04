# Particle Core 设计（顶栏粒子生命力空间）

> 日期：2026-09-04
> 状态：已评审通过（brainstorming 产出）
> 范围：纯前端（React + tsParticles），无 Rust 后端改动

## 1. 背景与目标

Friday 是远程环境故障诊断 Agent。诊断过程中 Agent 思考、执行工具、等待确认——这些过程目前只有文字和状态点表达。本项目在顶栏为 Friday 增加一个**独立的粒子空间**，用动态粒子表现 Agent 的「生命力」：

- **运行时**：粒子随状态变化形态与颜色，界面是「活的」
- **结束时**：粒子绽放后归于沉寂，只保留极微弱的待机呼吸——「呼，结束了」的仪式感
- **设计语言契合**：延续「暗色为本」「状态即视觉」「流式透明」原则；粒子色板完全复用现有语义色 token，不引入新的静态装饰

已通过交互式视觉稿确认的决策：

| 决策点 | 结论 |
|---|---|
| 粒子安放位置 | 顶栏左侧，Friday logo 旁（「品牌 + 生命力」身份簇） |
| 实现技术 | tsParticles（`@tsparticles/engine` + `@tsparticles/slim`） |
| 沉寂形态 | 微弱待机呼吸（约 4% 亮度、6-8s 超慢明灭） |
| 与现有状态指示器关系 | 共存（右侧 agent 检测状态指示器保持不动） |
| 状态映射 | 六态：thinking / executing / awaiting / error / done / idle |

## 2. 架构

### 2.1 组件

新增 `src/components/layout/ParticleCore.tsx`：

- 画布尺寸约 **112×36px**，插入 `TopBar.tsx` 左侧品牌区（`FridayMark` + "Friday" 文字之后，`gap-3`），与 logo 组成身份簇
- 组件持有单个 tsParticles 容器实例（ref）；模式切换时通过 `container.reset()` 应用对应预设（v3.9.1 无 `loadOptions`；`reset` 全新重建 options、无深合并残留，内部自带 refresh，保留 canvas DOM 与容器实例），**不销毁重建**——粒子空间位置连续，过渡自然
- `fpsLimit: 60`，`detectRetina: true`；40 粒子在 112×36 画布上性能开销可忽略

### 2.2 依赖

```
@tsparticles/engine   # 核心
@tsparticles/slim     # emitters 等常用能力
```

tsParticles v3 模块化架构，预计引入约 40-60KB gzip，对桌面 Tauri 应用可接受。

### 2.3 状态信号来源

全部来自现有 `sessionStore` / 消息状态（`ChatMessageStatus = "streaming" | "done" | "stopped" | "error"`、tool part `status === "running"`、pendingConfirm），**无需新增 IPC、无需改 Rust 后端**。

## 3. 状态机

派生 selector（优先级从高到低）：

| 优先级 | 条件 | 模式 |
|---|---|---|
| 1 | 存在待确认卡片（pendingConfirm） | `awaiting` |
| 2 | 任一 tool part `status === "running"` | `executing` |
| 3 | 当前 agent 消息 `status === "streaming"` | `thinking` |
| 4 | 最近一次运行结束（瞬态，本地计时） | `error`（3s）/ `done`（2.6s） |
| 5 | 默认 | `idle` |

瞬态规则：消息 status 离开 `streaming` 且为 `error` / `done` 时触发对应瞬态预设 + 组件本地 timer，到时回落 `idle`。`stopped`（用户手动停止）归入沉寂路径，不做绽放。

## 4. 六态预设

| 模式 | 颜色源 | 粒子数 | 速度 | 关键行为 |
|---|---|---|---|---|
| thinking | `--accent`（暗色 #3B82F6） | ~36 | 慢（0.6） | 小范围漂移 + opacity 呼吸动画 |
| executing | `--success` | ~40 | 快（2.2） | 扩散半径、轻微抖动、高频明灭 |
| awaiting | `--warning` | ~24 | 极慢 | 收紧成环、同步微起伏 |
| error | `--destructive` | ~40 | 中 | 一次性向外 burst 后余烬漂浮 |
| done | `--particle-celebration`（#A78BFA，庆祝专用，无语义对应） | ~40 | 中 | 一次性绽放、2600ms 内 opacity 衰减至 idle 水平 |
| idle | `--accent` 低饱和 | ~14 | 近零 | alpha≈4%，6-8s 超慢明灭 |

瞬态（error / done）用 tsParticles emitter burst 实现。

## 5. 主题处理

- 粒子颜色不硬编码：初始化时从 CSS 变量（`--accent` / `--success` / `--warning` / `--destructive` / `--particle-celebration`）读取；done 态庆祝紫定义在 `globals.css` 的 `:root`（`--particle-celebration: #A78BFA`，无对应语义 token、三主题通用，是颜色走 token 约定的合规实现）
- `MutationObserver` 监听 `<html data-theme>` 变化，主题切换后刷新粒子颜色
- 三主题（暗色 / 浅色 / 暖白）自动适配：浅色系主题的语义色本就是加深过的变体（如 accent #2563EB），亮色顶栏上对比度足够
- 辉光强度：暗色全辉光；浅色 / 暖白降为约 40%（亮底上强辉光发灰）

## 6. 边界情况

- **`prefers-reduced-motion`**：遵循设计语言强制约束——粒子区渲染静态形态（当前模式的 3-5 个静止色点），无任何动画
- **持续动画备案**：idle 待机呼吸是无限循环动画，但其语义为「待机状态指示」（近设备待机灯）而非纯装饰；强度仅约 4%、周期 6-8s，且为全应用唯一环境动画，符合动画克制原则。动画时长 token（`--duration-*`）为 UI 过渡专用，不适用环境循环动画，粒子节奏独立成体系
- **窗口最小化 / 后台**：rAF 自动暂停，无额外处理
- **组件卸载 / 会话切换**：destroy 容器、清理 MutationObserver 与瞬态 timer；粒子只反映当前活跃会话
- **降级**：tsParticles 初始化失败时粒子区静默隐藏，顶栏布局不受影响（容器占位保留，内容为空）

## 7. 验证

- `pnpm typecheck` 通过（纯前端改动，`cargo check` 不受影响）
- `pnpm tauri dev` 手动走查清单：
  1. 发起诊断：观察 thinking ↔ executing 循环切换、粒子位置连续
  2. 触发确认卡片：awaiting 琥珀紧环
  3. 停止 / 完成：done 紫色绽放 → idle 待机呼吸
  4. 制造错误（如断开环境）：error 红色炸散 → idle
  5. 切换三主题：颜色即时刷新、浅色主题辉光减弱
  6. 系统开启 reduced-motion：静态色点、无动画

## 8. 不做的事（YAGNI）

- 不做鼠标交互粒子（hover 追随、点击涟漪）——顶栏区域小，交互意义有限
- 不做粒子内嵌文字读数（当前工具名 / 耗时）——右侧状态指示器与工具卡片已覆盖
- 不替换 / 简化右侧现有状态指示器
- 不引入 WebGL 火焰 / 流体等重型效果
