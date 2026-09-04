# Particle Core 设计（顶栏粒子生命力空间）

> 日期：2026-09-04
> 状态：已评审通过（brainstorming 产出）
> 范围：纯前端（React + tsParticles），无 Rust 后端改动
> **修订 2026-09-04（v0.15.0 发布后）**：v1 的 thinking（蓝）/executing（绿）按状态切换在实际使用中出现高频蓝绿抖动、切换生硬。修订为**颜色跟时间走**：两态合并为单一 `running` 态（绿色），随运行时间经 CSS filter 渐进增强激烈度（saturate/brightness/hue-rotate，5 分钟到满强度）；awaiting/error/done/idle 不变。§3/§4 已按修订后口径更新。

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

- 画布尺寸约 **80×36px**（v0.15.1 修订：原 112px 收窄），紧跟 `FridayMark`（gap 1.5；顶栏 "Friday" 文字已移除，logo 与粒子直接相连组成身份簇）
- 组件持有单个 tsParticles 容器实例（ref）；模式切换时通过 `container.reset()` 应用对应预设（v3.9.1 无 `loadOptions`；`reset` 全新重建 options、无深合并残留，内部自带 refresh，保留 canvas DOM 与容器实例），**不销毁重建**——粒子空间位置连续，过渡自然
- `fpsLimit: 60`，`detectRetina: true`；40 粒子在 80×36 画布上性能开销可忽略

### 2.2 依赖

```
@tsparticles/engine   # 核心
@tsparticles/slim     # emitters 等常用能力
```

tsParticles v3 模块化架构，预计引入约 40-60KB gzip，对桌面 Tauri 应用可接受。

### 2.3 状态信号来源

全部来自现有 `sessionStore` / 消息状态（`ChatMessageStatus = "streaming" | "done" | "stopped" | "error"`、tool part `status === "running"`、pendingConfirm），**无需新增 IPC、无需改 Rust 后端**。

## 3. 状态机

派生 selector（优先级从高到低，修订后）：

| 优先级 | 条件 | 模式 |
|---|---|---|
| 1 | 存在待确认卡片（pendingConfirm） | `awaiting` |
| 2 | 运行中（agent streaming **或** 任一工具执行中，合并判断） | `running` |
| 3 | 最近一次运行结束（瞬态，本地计时） | `error`（3s）/ `done`（2.6s） |
| 4 | 默认 | `idle` |

瞬态规则：运行结束（streaming 与工具全部结束）且消息 status 为 `error` / `done` 时触发对应瞬态预设 + 组件本地 timer，到时回落 `idle`。`stopped`（用户手动停止）归入沉寂路径，不做绽放。

`running` 态携带 `runStartedAt` 时间戳：首次进入 running 时记录；awaiting 打断期间保留（恢复后激烈度从原进度继续）；回到 idle 清除；会话切换清除并重新播种。

## 4. 五态预设 + 时间激烈度

| 模式 | 颜色源 | 粒子数 | 速度 | 关键行为 |
|---|---|---|---|---|
| running | `--success`（绿） | ~36 | 1.4 | 中等能量漂移；叠加时间激烈度 filter（见下） |
| awaiting | `--warning` | ~24 | 极慢 | 收紧集群、同步微起伏 |
| error | `--destructive` | ~40 | 快 | 一次性向外 burst 后余烬漂浮 |
| done | `--particle-celebration`（#A78BFA，庆祝专用，无语义对应） | ~40 | 中 | 一次性绽放、2600ms 内 opacity 衰减至 idle 水平 |
| idle | `--accent` 低饱和 | ~14 | 近零 | alpha≈12%→30%、size 0.8-1.8、微辉光，6-8s 超慢明灭（纯黑底上可辨认的待机微光；v0.15.0 后修正：原 4-12% 在暗色下不可见） |

瞬态（error / done）用 life.count=1 + outMode "out" 实现一次绽放后消散。

**时间激烈度（running 态专属）**：粒子区 wrapper 上应用 CSS filter，随 `now - runStartedAt` 线性增强，**5 分钟（300s）到满强度后保持**：

| 时刻 | filter |
|---|---|
| 0s | `saturate(1) brightness(1) hue-rotate(0deg)` |
| 300s | `saturate(1.45) brightness(1.25) hue-rotate(25deg)`（向黄绿偏移） |

- filter 作用于 canvas 整体合成像素（粒子 + 辉光一起增强），GPU 合成零粒子重建，全程平滑
- 激烈度由纯函数 `runIntensity(elapsedMs)` 计算（intensity.ts），组件以 1s tick 更新
- 语义：运行越久 = 诊断进入越深的阶段，颜色越"热"

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
