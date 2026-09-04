# Particle Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在顶栏 Friday logo 旁增加一个 112×36 的 tsParticles 粒子区，用六态粒子（thinking/executing/awaiting/error/done/idle）表现 Agent 生命力，工作结束回归微弱待机呼吸。

**Architecture:** 纯前端改动。纯函数层（模式优先级推导 + 瞬态检测 + 六态预设）可单测；`useParticleMode` hook 把 `sessionStore` 信号接进来；`ParticleCore` 组件持有单个 tsParticles 容器，模式/主题变化时 `loadOptions + refresh`（不销毁重建）。规格见 `docs/superpowers/specs/2026-09-04-particle-core-design.md`。

**Tech Stack:** React 19 + zustand v5、`@tsparticles/engine` + `@tsparticles/slim`（v3，vanilla API，不用 React wrapper）、vitest（本仓库首个前端测试，仅测纯函数，零配置）。

**与 spec 的三处实现细化**（行为不变）：
1. 主题色刷新用 `useThemeStore` 订阅替代 MutationObserver——`themeStore.applyTheme` 是 `dataset.theme` 唯一写入方，store 即事实来源。
2. 粒子颜色在主题切换时整组重读（`getComputedStyle` 读 `--accent` 等），替代逐粒子换色。
3. 模式切换用 `container.reset(preset)` 而非 `loadOptions+refresh`——v3.9.1 Container 无 `loadOptions` 方法；`reset()` 全新重建 options（无深合并残留）、内部自带 `refresh()`、保留 canvas DOM。依赖精确锁定 `3.9.1`（v4 移除了 shadow/paint 重构，与设计不符）。

---

## File Structure

```
src/components/layout/
├─ TopBar.tsx                [modify] 品牌区插入 <ParticleCore />
├─ ParticleCore.tsx          [create] 组件：引擎初始化一次、模式/主题应用、reduced-motion 静态降级
└─ particles/
   ├─ deriveMode.ts          [create] 纯函数：基础模式优先级 + 瞬态检测 + 瞬态时长
   ├─ deriveMode.test.ts     [create] 单测（vitest）
   ├─ presets.ts             [create] 六态 tsParticles 预设 + CSS 变量色读取
   ├─ presets.test.ts        [create] 单测（vitest）
   └─ useParticleMode.ts     [create] hook：sessionStore 信号 → ParticleMode（含瞬态计时与会话切换防误触）
```

设计要点（防止实现走样）：
- **每个预设显式声明全部可变字段**（number/color/size/opacity/move/life/links/shadow），因为 `loadOptions` 是深合并，残留上一模式的生命期/方向会出错。
- **瞬态（error/done）用 `life.count=1` + `outModes:"out"` 实现一次绽放后消散**，不用 emitters——避免引入 `@tsparticles/plugin-emitters` 额外依赖。
- 粒子只反映**当前活跃会话**（selector 只读 `messagesBySession[currentSessionId]`）。

---

### Task 1: 依赖与测试脚手架

**Files:**
- Modify: `package.json`（pnpm add 自动写入 + 手动加 test script）

- [ ] **Step 1: 安装依赖**

```powershell
pnpm add @tsparticles/engine @tsparticles/slim
pnpm add -D vitest
```

- [ ] **Step 2: 加 test script**

`package.json` 的 `scripts` 中加入（保持现有 script 不动）：

```json
"test": "vitest run"
```

- [ ] **Step 3: 验证类型检查通过（此时还没有新代码）**

Run: `pnpm typecheck`
Expected: 无错误退出

- [ ] **Step 4: Commit**

```powershell
git add package.json pnpm-lock.yaml
git commit -m "feat(ui): add tsparticles deps and vitest scaffold"
```

---

### Task 2: deriveMode 纯函数（TDD）

**Files:**
- Test: `src/components/layout/particles/deriveMode.test.ts`
- Create: `src/components/layout/particles/deriveMode.ts`

- [ ] **Step 1: 写失败测试**

`src/components/layout/particles/deriveMode.test.ts`：

```ts
import { describe, expect, it } from "vitest";
import { detectTransient, deriveBaseMode, TRANSIENT_MS } from "./deriveMode";

describe("deriveBaseMode（优先级从高到低）", () => {
  it("优先级1：待确认压倒工具执行与思考", () => {
    expect(
      deriveBaseMode({ pendingConfirm: true, toolRunning: true, agentStreaming: true }),
    ).toBe("awaiting");
  });

  it("优先级2：工具执行压倒思考", () => {
    expect(
      deriveBaseMode({ pendingConfirm: false, toolRunning: true, agentStreaming: true }),
    ).toBe("executing");
  });

  it("优先级3：仅思考", () => {
    expect(
      deriveBaseMode({ pendingConfirm: false, toolRunning: false, agentStreaming: true }),
    ).toBe("thinking");
  });

  it("全部静止则沉寂", () => {
    expect(
      deriveBaseMode({ pendingConfirm: false, toolRunning: false, agentStreaming: false }),
    ).toBe("idle");
  });
});

describe("detectTransient（瞬态检测）", () => {
  const streaming = { streaming: true, status: "streaming" as const };

  it("streaming → done 触发 done 瞬态", () => {
    expect(detectTransient(streaming, { streaming: false, status: "done" })).toEqual({
      mode: "done",
      durationMs: TRANSIENT_MS.done,
    });
  });

  it("streaming → error 触发 error 瞬态", () => {
    expect(detectTransient(streaming, { streaming: false, status: "error" })).toEqual({
      mode: "error",
      durationMs: TRANSIENT_MS.error,
    });
  });

  it("streaming → stopped 无瞬态（用户手动停止，直接沉寂）", () => {
    expect(detectTransient(streaming, { streaming: false, status: "stopped" })).toBeNull();
  });

  it("非 streaming 起点不触发", () => {
    expect(
      detectTransient(
        { streaming: false, status: "done" },
        { streaming: false, status: "idle" as never },
      ),
    ).toBeNull();
  });

  it("仍在 streaming 不触发", () => {
    expect(detectTransient(streaming, streaming)).toBeNull();
  });
});
```

- [ ] **Step 2: 运行确认失败**

Run: `pnpm test`
Expected: FAIL —— `Cannot find module './deriveMode'`（或等价的模块解析失败）

- [ ] **Step 3: 实现**

`src/components/layout/particles/deriveMode.ts`：

```ts
/** ParticleCore 六态（spec §3/§4） */
export type ParticleMode =
  | "thinking"
  | "executing"
  | "awaiting"
  | "error"
  | "done"
  | "idle";

/** 基础模式：由 store 信号即时派生（瞬态 error/done 除外） */
export type BaseMode = Exclude<ParticleMode, "error" | "done">;

export interface ModeInput {
  pendingConfirm: boolean;
  toolRunning: boolean;
  agentStreaming: boolean;
}

/** 优先级：awaiting > executing > thinking > idle（spec §3） */
export function deriveBaseMode(input: ModeInput): BaseMode {
  if (input.pendingConfirm) return "awaiting";
  if (input.toolRunning) return "executing";
  if (input.agentStreaming) return "thinking";
  return "idle";
}

/** 最后一条 agent 消息的运行态快照 */
export interface LastRunState {
  streaming: boolean;
  status: "streaming" | "done" | "stopped" | "error" | null;
}

export interface Transient {
  mode: "error" | "done";
  durationMs: number;
}

/** 瞬态停留时长（spec §3：error 3s / done 2.6s） */
export const TRANSIENT_MS: Record<Transient["mode"], number> = {
  error: 3000,
  done: 2600,
};

/**
 * 检测 streaming 边沿上的瞬态：
 * - done → 紫色绽放
 * - error → 红色炸散
 * - stopped（用户手动停止）→ 无瞬态，直接沉寂
 */
export function detectTransient(
  prev: LastRunState,
  next: LastRunState,
): Transient | null {
  if (!prev.streaming || next.streaming) return null;
  if (next.status === "error") return { mode: "error", durationMs: TRANSIENT_MS.error };
  if (next.status === "done") return { mode: "done", durationMs: TRANSIENT_MS.done };
  return null;
}
```

- [ ] **Step 4: 运行确认通过**

Run: `pnpm test`
Expected: PASS（deriveMode 全部用例绿）

- [ ] **Step 5: Commit**

```powershell
git add src/components/layout/particles/deriveMode.ts src/components/layout/particles/deriveMode.test.ts
git commit -m "feat(ui): particle mode state machine (pure) with tests"
```

---

### Task 3: presets 六态预设（TDD）

**Files:**
- Test: `src/components/layout/particles/presets.test.ts`
- Create: `src/components/layout/particles/presets.ts`
- Modify: `src/styles/globals.css`（`:root` 语义色区，约 27-38 行）

- [ ] **Step 1: 写失败测试**

`src/components/layout/particles/presets.test.ts`：

```ts
import { describe, expect, it } from "vitest";
import { buildPreset, type PresetContext } from "./presets";

const ctx: PresetContext = {
  colors: {
    accent: "#3B82F6",
    success: "#22C55E",
    warning: "#EAB308",
    destructive: "#EF4444",
    celebration: "#A78BFA",
  },
  glow: 1,
};

describe("buildPreset（六态映射，spec §4）", () => {
  it("thinking 用 accent、36 粒子、无限生命期（显式重置）", () => {
    const p = buildPreset("thinking", ctx);
    expect(p.particles?.number?.value).toBe(36);
    expect(p.particles?.color?.value).toBe(ctx.colors.accent);
    expect((p.particles?.life as any)?.count).toBe(0);
    expect((p.particles?.life as any)?.duration.value).toBe(0);
  });

  it("executing 用 success、40 粒子、高速", () => {
    const p = buildPreset("executing", ctx);
    expect(p.particles?.color?.value).toBe(ctx.colors.success);
    expect(p.particles?.number?.value).toBe(40);
    expect(p.particles?.move?.speed).toBe(2.2);
  });

  it("awaiting 用 warning 且明暗动画同步（屏息）", () => {
    const p = buildPreset("awaiting", ctx);
    expect(p.particles?.color?.value).toBe(ctx.colors.warning);
    expect(p.particles?.opacity?.animation?.sync).toBe(true);
    expect(p.particles?.move?.speed).toBe(0.15);
  });

  it("error 用 destructive、一次性生命期、向外飞散", () => {
    const p = buildPreset("error", ctx);
    expect(p.particles?.color?.value).toBe(ctx.colors.destructive);
    expect(p.particles?.life?.count).toBe(1);
    expect(p.particles?.move?.direction).toBe("outside");
    expect(p.particles?.move?.outModes?.default).toBe("out");
  });

  it("done 用庆祝紫 token（--particle-celebration，spec §5）", () => {
    const p = buildPreset("done", ctx);
    expect(p.particles?.color?.value).toBe(ctx.colors.celebration);
    expect(p.particles?.life?.count).toBe(1);
  });

  it("idle 极低透明度待机呼吸", () => {
    const p = buildPreset("idle", ctx);
    expect(p.particles?.number?.value).toBe(14);
    expect(p.particles?.opacity?.value).toEqual({ min: 0.04, max: 0.12 });
    expect(p.particles?.move?.speed).toBe(0.1);
  });

  it("辉光按 glow 缩放（亮色主题 0.4）", () => {
    const full = buildPreset("thinking", ctx).particles?.shadow?.blur ?? 0;
    const dim = buildPreset("thinking", { ...ctx, glow: 0.4 }).particles?.shadow?.blur ?? 0;
    expect(dim).toBeCloseTo(full * 0.4, 5);
  });

  it("全屏模式必须关闭（顶栏内嵌关键约束）", () => {
    expect(buildPreset("idle", ctx).fullScreen?.enable).toBe(false);
  });

  it("每个预设都显式关闭 links（防 loadOptions 深合并残留）", () => {
    for (const mode of ["thinking", "executing", "awaiting", "error", "done", "idle"] as const) {
      expect(buildPreset(mode, ctx).particles?.links?.enable).toBe(false);
    }
  });
});
```

- [ ] **Step 2: 运行确认失败**

Run: `pnpm test`
Expected: FAIL —— `Cannot find module './presets'`

- [ ] **Step 3: 实现**

先加 token。`src/styles/globals.css` 的 `:root` 语义色区（`--info` 之后、`--ring` 之前）插入：

```css
  /* ── 粒子区专用（spec §5：庆祝紫，无语义对应，三主题通用） ── */
  --particle-celebration: #A78BFA;
```

然后 `src/components/layout/particles/presets.ts`：

```ts
import type { IOptions, RecursivePartial } from "@tsparticles/engine";
import type { ParticleMode } from "./deriveMode";

/** 从 CSS 变量读取粒子色板（三主题自动适配，spec §5） */
export interface ParticleColors {
  accent: string;
  success: string;
  warning: string;
  destructive: string;
  celebration: string;
}

export function readParticleColors(): ParticleColors {
  const css = getComputedStyle(document.documentElement);
  const get = (name: string, fallback: string) => {
    const v = css.getPropertyValue(name).trim();
    return v || fallback;
  };
  return {
    accent: get("--accent", "#3B82F6"),
    success: get("--success", "#22C55E"),
    warning: get("--warning", "#EAB308"),
    destructive: get("--destructive", "#EF4444"),
    celebration: get("--particle-celebration", "#A78BFA"),
  };
}

export interface PresetContext {
  colors: ParticleColors;
  /** 辉光强度：暗色 1，浅色/暖白 0.4（spec §5） */
  glow: number;
}

interface ModeSpec {
  count: number;
  color: (c: ParticleColors) => string;
  sizeMin: number;
  sizeMax: number;
  speed: number;
  direction: "none" | "outside";
  straight: boolean;
  outMode: "bounce" | "out";
  opacityMin: number;
  opacityMax: number;
  twinkleSpeed: number;
  syncTwinkle: boolean;
  lifeSeconds: number | null;
  lifeDelayMax: number | null;
  glowBlur: number;
}

/** 六态参数表（spec §4）。瞬态用 life.count=1 + outMode "out" 实现一次绽放后消散。 */
const SPECS: Record<ParticleMode, ModeSpec> = {
  thinking: {
    count: 36, color: (c) => c.accent,
    sizeMin: 0.8, sizeMax: 2.2,
    speed: 0.6, direction: "none", straight: false, outMode: "bounce",
    opacityMin: 0.25, opacityMax: 0.85, twinkleSpeed: 0.8, syncTwinkle: false,
    lifeSeconds: null, lifeDelayMax: null, glowBlur: 8,
  },
  executing: {
    count: 40, color: (c) => c.success,
    sizeMin: 1.0, sizeMax: 2.6,
    speed: 2.2, direction: "none", straight: false, outMode: "bounce",
    opacityMin: 0.35, opacityMax: 1.0, twinkleSpeed: 2.4, syncTwinkle: false,
    lifeSeconds: null, lifeDelayMax: null, glowBlur: 10,
  },
  awaiting: {
    count: 24, color: (c) => c.warning,
    sizeMin: 0.8, sizeMax: 1.8,
    speed: 0.15, direction: "none", straight: false, outMode: "bounce",
    opacityMin: 0.4, opacityMax: 0.8, twinkleSpeed: 0.4, syncTwinkle: true,
    lifeSeconds: null, lifeDelayMax: null, glowBlur: 6,
  },
  error: {
    count: 40, color: (c) => c.destructive,
    sizeMin: 1.0, sizeMax: 2.4,
    speed: 3.0, direction: "outside", straight: true, outMode: "out",
    opacityMin: 0.4, opacityMax: 1.0, twinkleSpeed: 1.2, syncTwinkle: false,
    lifeSeconds: 3, lifeDelayMax: 1.2, glowBlur: 10,
  },
  done: {
    count: 40, color: (c) => c.celebration,
    sizeMin: 1.0, sizeMax: 2.4,
    speed: 1.2, direction: "outside", straight: true, outMode: "out",
    opacityMin: 0.5, opacityMax: 1.0, twinkleSpeed: 1.5, syncTwinkle: false,
    lifeSeconds: 2.6, lifeDelayMax: 1.0, glowBlur: 12,
  },
  idle: {
    count: 14, color: (c) => c.accent,
    sizeMin: 0.6, sizeMax: 1.4,
    speed: 0.1, direction: "none", straight: false, outMode: "bounce",
    opacityMin: 0.04, opacityMax: 0.12, twinkleSpeed: 0.15, syncTwinkle: false,
    lifeSeconds: null, lifeDelayMax: null, glowBlur: 3,
  },
};

/**
 * 构建指定模式的 tsParticles 选项。
 * 注意：每个预设显式声明全部可变字段——无论上层用整体重建（reset）还是
 * 深合并（options.load）方式应用，都不会残留上一模式的生命期/方向/速度。
 * 非瞬态模式的 life 显式置 count:0（v3 ILife 无 enable 字段，count≤0 即无限）。
 */
export function buildPreset(
  mode: ParticleMode,
  ctx: PresetContext,
): RecursivePartial<IOptions> {
  const spec = SPECS[mode];
  const color = spec.color(ctx.colors);
  return {
    fpsLimit: 60,
    detectRetina: true,
    fullScreen: { enable: false },
    background: { color: { value: "transparent" } },
    particles: {
      number: { value: spec.count },
      color: { value: color },
      size: { value: { min: spec.sizeMin, max: spec.sizeMax } },
      opacity: {
        value: { min: spec.opacityMin, max: spec.opacityMax },
        animation: {
          enable: true,
          speed: spec.twinkleSpeed,
          sync: spec.syncTwinkle,
          startValue: "random",
        },
      },
      move: {
        enable: true,
        speed: spec.speed,
        direction: spec.direction,
        straight: spec.straight,
        outModes: { default: spec.outMode },
      },
      life:
        spec.lifeSeconds !== null
          ? {
              count: 1,
              duration: { value: spec.lifeSeconds },
              delay: { value: { min: 0, max: spec.lifeDelayMax ?? 0 } },
            }
          : { count: 0, duration: { value: 0 }, delay: { value: 0 } },
      links: { enable: false },
      shadow: { enable: true, blur: spec.glowBlur * ctx.glow, color: { value: color } },
    },
  };
}
```

- [ ] **Step 4: 运行确认通过**

Run: `pnpm test`
Expected: PASS（deriveMode + presets 全绿）

- [ ] **Step 5: Commit**

```powershell
git add src/components/layout/particles/presets.ts src/components/layout/particles/presets.test.ts src/styles/globals.css
git commit -m "feat(ui): six-state particle presets with theme-aware colors"
```

---

### Task 4: useParticleMode hook

**Files:**
- Create: `src/components/layout/particles/useParticleMode.ts`

瞬态逻辑的边沿检测与会话切换防护已全部抽成 Task 2 的纯函数；本 hook 是薄胶水层（zustand selector + 计时器），不单测（需要 jsdom/testing-library，YAGNI），由 Task 6 手动走查覆盖。

- [ ] **Step 1: 实现**

`src/components/layout/particles/useParticleMode.ts`：

```ts
import { useEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useSessionStore } from "@/store/sessionStore";
import type { ChatMessage } from "@/lib/types";
import {
  detectTransient,
  deriveBaseMode,
  type LastRunState,
  type ParticleMode,
} from "./deriveMode";

interface RunSignals {
  sessionId: string | null;
  pendingConfirm: boolean;
  toolRunning: boolean;
  agentStreaming: boolean;
  lastAgentStatus: LastRunState["status"];
}

/** 从当前会话消息派生信号（只反映当前活跃会话，spec §6） */
function readSignals(
  messages: ChatMessage[] | undefined,
  running: boolean,
  sessionId: string | null,
): RunSignals {
  const msgs = messages ?? [];
  let pendingConfirm = false;
  let toolRunning = false;
  for (const m of msgs) {
    for (const p of m.parts) {
      if (p.type === "confirm" && p.confirm?.resolved === "pending") pendingConfirm = true;
      if (p.type === "tool" && p.tool?.status === "running") toolRunning = true;
    }
  }
  let lastAgentStatus: RunSignals["lastAgentStatus"] = null;
  for (let i = msgs.length - 1; i >= 0; i--) {
    if (msgs[i].role === "agent") {
      lastAgentStatus = msgs[i].status;
      break;
    }
  }
  const agentStreaming = running || lastAgentStatus === "streaming";
  return { sessionId, pendingConfirm, toolRunning, agentStreaming, lastAgentStatus };
}

export function useParticleMode(): ParticleMode {
  const signals = useSessionStore(
    useShallow((s) =>
      readSignals(
        s.currentSessionId ? s.messagesBySession[s.currentSessionId] : undefined,
        !!(s.currentSessionId && s.agentRunning[s.currentSessionId]),
        s.currentSessionId,
      ),
    ),
  );

  const base = deriveBaseMode(signals);

  const [transient, setTransient] = useState<{ mode: "error" | "done" } | null>(null);
  const prevRef = useRef<{ sessionId: string | null; run: LastRunState }>({
    sessionId: null,
    run: { streaming: false, status: null },
  });

  useEffect(() => {
    const run: LastRunState = {
      streaming: signals.agentStreaming,
      status: signals.agentStreaming ? "streaming" : signals.lastAgentStatus,
    };
    const prev = prevRef.current;

    // 会话切换：直接重置快照并清除旧瞬态，防止把旧会话的完成误判成本会话瞬态
    if (prev.sessionId !== signals.sessionId) {
      prevRef.current = { sessionId: signals.sessionId, run };
      setTransient(null);
      return;
    }

    const t = detectTransient(prev.run, run);
    prevRef.current = { sessionId: signals.sessionId, run };

    // 新一轮运行开始：清除上一轮可能残留的瞬态（timer 已被 cleanup 清掉，但 state 未必）
    if (run.streaming) {
      setTransient(null);
    }

    if (t) {
      setTransient({ mode: t.mode });
      const timer = window.setTimeout(() => setTransient(null), t.durationMs);
      return () => window.clearTimeout(timer);
    }
  }, [signals.sessionId, signals.agentStreaming, signals.lastAgentStatus]);

  // 基础模式非 idle 时压倒瞬态（spec §3 优先级）
  if (base !== "idle") return base;
  return transient?.mode ?? "idle";
}
```

- [ ] **Step 2: 类型检查**

Run: `pnpm typecheck`
Expected: 无错误（组件尚未引用，hook 自身类型完整）

- [ ] **Step 3: Commit**

```powershell
git add src/components/layout/particles/useParticleMode.ts
git commit -m "feat(ui): useParticleMode hook wiring store signals to particle mode"
```

---

### Task 5: ParticleCore 组件 + TopBar 集成

**Files:**
- Create: `src/components/layout/ParticleCore.tsx`
- Modify: `src/components/layout/TopBar.tsx`（品牌区，当前 59-70 行附近）

- [ ] **Step 1: 实现组件**

`src/components/layout/ParticleCore.tsx`：

```tsx
import { useEffect, useMemo, useRef, useState } from "react";
import { tsParticles, type Container } from "@tsparticles/engine";
import { loadSlim } from "@tsparticles/slim";
import { useThemeStore } from "@/store/themeStore";
import { useParticleMode } from "./particles/useParticleMode";
import {
  buildPreset,
  readParticleColors,
  type ParticleColors,
} from "./particles/presets";
import type { ParticleMode } from "./particles/deriveMode";

const CANVAS_ID = "friday-particle-core";
const ZONE_WIDTH = 112;
const ZONE_HEIGHT = 36;

// 引擎只加载一次（StrictMode 双挂载/组件重挂载时复用）
let engineReady: Promise<void> | null = null;
function ensureEngine(): Promise<void> {
  engineReady ??= loadSlim(tsParticles);
  return engineReady;
}

export function ParticleCore() {
  const zoneRef = useRef<HTMLDivElement | null>(null);
  const containerRef = useRef<Container | null>(null);
  const [ready, setReady] = useState(false);
  const [colors, setColors] = useState<ParticleColors>(() => readParticleColors());
  const theme = useThemeStore((s) => s.theme);
  const mode = useParticleMode();
  const reducedMotion = useMemo(
    () => window.matchMedia("(prefers-reduced-motion: reduce)").matches,
    [],
  );

  const glow = theme === "dark" ? 1 : 0.4;

  // 主题切换：整组重读 CSS 变量色板（spec §5）
  useEffect(() => {
    setColors(readParticleColors());
  }, [theme]);

  // 初始化一次：引擎加载 + 容器创建；失败静默降级为空白区域（spec §6）
  useEffect(() => {
    if (reducedMotion) return;
    let cancelled = false;
    (async () => {
      try {
        await ensureEngine();
        if (cancelled || !zoneRef.current) return;
        const container = await tsParticles.load({
          id: CANVAS_ID,
          element: zoneRef.current,
          options: buildPreset("idle", { colors: readParticleColors(), glow }),
        });
        if (!container) return;
        if (cancelled) {
          container.destroy();
          return;
        }
        containerRef.current = container;
        setReady(true);
      } catch (e) {
        console.error("ParticleCore init failed:", e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      cancelled = true;
      containerRef.current?.destroy();
      containerRef.current = null;
    };
    // eslint 未配置；glow 仅初值使用，故不列入依赖
  }, [reducedMotion]);

  // 模式/主题/颜色变化：应用对应预设，不销毁容器（spec §2.1）
  // v3.9.1 无 container.loadOptions；reset() 全新重建 options（无深合并残留）、
  // 内部自带 refresh()、保留 canvas DOM 与容器实例
  useEffect(() => {
    const container = containerRef.current;
    if (!container || !ready) return;
    void container.reset(buildPreset(mode, { colors, glow }));
  }, [mode, colors, glow, ready]);

  if (reducedMotion) {
    return <StaticCore mode={mode} colors={colors} />;
  }

  return (
    <div
      ref={zoneRef}
      className="shrink-0"
      style={{ width: ZONE_WIDTH, height: ZONE_HEIGHT }}
      aria-hidden="true"
    />
  );
}

/** reduced-motion 静态形态：当前模式颜色 的 4 个静止色点，无任何动画（spec §6） */
function StaticCore({ mode, colors }: { mode: ParticleMode; colors: ParticleColors }) {
  const color =
    mode === "executing"
      ? colors.success
      : mode === "awaiting"
        ? colors.warning
        : mode === "error"
          ? colors.destructive
          : mode === "done"
            ? colors.celebration
            : colors.accent;
  return (
    <div
      className="flex items-center gap-1.5 shrink-0"
      style={{ width: ZONE_WIDTH, height: ZONE_HEIGHT }}
      aria-hidden="true"
    >
      {[0.2, 0.4, 0.6, 0.8].map((o) => (
        <span
          key={o}
          className="w-1 h-1 rounded-full"
          style={{ background: color, opacity: o }}
        />
      ))}
    </div>
  );
}
```

- [ ] **Step 2: 接入 TopBar**

`src/components/layout/TopBar.tsx`：

import 区（`FridayMark` 导入之后）加：

```tsx
import { ParticleCore } from "@/components/layout/ParticleCore";
```

品牌区（`<div className="flex items-center gap-2">` 内，`Friday` span 之后、`</div>` 之前）插入：

```tsx
          <ParticleCore />
```

即品牌区变为：

```tsx
        <div className="flex items-center gap-2">
          <FridayMark size={22} />
          <span
            className="text-foreground text-sm font-semibold tracking-wide"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            Friday
          </span>
          <ParticleCore />
        </div>
```

- [ ] **Step 3: 类型检查 + 测试**

Run: `pnpm typecheck`
Expected: 无错误

Run: `pnpm test`
Expected: 全绿（纯函数测试不受组件影响）

- [ ] **Step 4: 快速目检**

Run: `pnpm tauri dev`
Expected: 顶栏 logo 右侧出现 112×36 粒子区，无诊断时呈 idle 微光呼吸；顶栏布局无错位，右侧状态指示器不受影响。

- [ ] **Step 5: Commit**

```powershell
git add src/components/layout/ParticleCore.tsx src/components/layout/TopBar.tsx
git commit -m "feat(ui): particle core vitality zone in topbar"
```

---

### Task 6: 手动走查与收尾

**Files:** 无新增（如有微调则修改 Task 4/5 的文件）

- [ ] **Step 1: 全量自动检查**

Run: `pnpm typecheck`
Expected: 无错误

Run: `pnpm test`
Expected: 全绿

- [ ] **Step 2: 手动走查（spec §7 清单）**

Run: `pnpm tauri dev`

逐项验证：

1. **思考↔执行循环**：发起一次诊断（输入环境+服务+症状）。观察粒子在 thinking（蓝色漂移）与 executing（绿色高能）间切换，模式切换时粒子区不闪烁黑屏、无布局抖动。
2. **等待确认**：触发一个需确认的工具（或观察确认卡片出现时）。粒子变为琥珀色收紧集群、明暗同步。
3. **完成绽放**：诊断正常结束。粒子紫色绽放约 2.6s 后消散，落入 idle 微光呼吸。
4. **错误炸散**：制造一次失败（如诊断中直接杀掉目标连接/agent）。粒子红色炸散约 3s 后落入 idle。
5. **手动停止**：诊断中点停止。粒子直接沉寂（无绽放）。
6. **会话切换防护**：在 A 会话运行中切到历史 B 会话再切回。粒子不应出现假"完成绽放"。
7. **三主题**：顶栏切换 暗色/浅色/暖白。粒子颜色随主题即时刷新；浅色/暖白下辉光明显减弱；idle 态在浅色顶栏上仍可辨认。
8. **reduced-motion**：系统开启"减少动画"（Windows: 设置→辅助功能→视觉效果→动画效果 关闭）后重启应用。粒子区为 4 个静止色点，无动画。
9. **降级**：断网/正常路径无法直接模拟 tsParticles 失败——代码走查确认 try/catch 分支存在即可（`ParticleCore.tsx` init 的 catch）。

- [ ] **Step 3: 修复走查中发现的问题（如有）**

每处修复后重跑 Step 1 自动检查。

- [ ] **Step 4: Commit（如有修复）**

```powershell
git add -A src/components/layout/
git commit -m "fix(ui): particle core walkthrough fixes"
```

无修复则跳过此步。

---

## Self-Review

**Spec coverage（spec → task）：**
- §2 架构（组件/依赖/loadOptions 不销毁重建）→ Task 1、Task 5 ✓
- §3 状态机（优先级 + 瞬态 + stopped 直接沉寂）→ Task 2、Task 4 ✓
- §4 六态预设参数表 → Task 3（SPECS 与表逐项对应）✓
- §5 主题处理（CSS 变量色 + 辉光降档 + done 紫例外）→ Task 3、Task 5 ✓
- §6 边界（reduced-motion / 卸载清理 / 会话切换 / 降级）→ Task 4（会话防护）、Task 5（StaticCore/清理/catch）、Task 6 走查 ✓
- §7 验证 → Task 6 ✓
- §8 不做的事 → 未引入鼠标交互/文字读数/WebGL ✓

**Placeholder scan：** 无 TBD/TODO；所有代码步骤含完整代码；走查步骤含具体操作与预期。✓

**Type consistency：** `ParticleMode` 定义于 deriveMode.ts，presets/useParticleMode/ParticleCore 均从该处导入；`buildPreset(mode, ctx)` 签名与所有调用一致；`Transient`/`TRANSIENT_MS` 名称前后一致。✓

## 执行过程中的实现偏差记录（以提交代码为准）

执行中发现/修正的偏差，最终代码为事实来源：

1. **tsParticles 版本**：安装时最新为 v4.4.0，但 v4 移除 `particles.shadow`（辉光）并将 `particles.color` 迁移到 `paint`，与已批准视觉设计不符 → 降级精确锁定 **3.9.1**（package.json 无 caret）。
2. **运行时重载 API**：v3.9.1 Container 无 `loadOptions` 方法；正确 API 为 `container.reset(preset)`（全新重建 options、无深合并残留、内部自带 refresh、保留 canvas DOM）。
3. **life 重置**：v3 `ILife` 无 `enable` 字段，非瞬态预设显式 `life: { count: 0, duration: { value: 0 }, delay: { value: 0 } }`（count≤0 = 无限）。
4. **stale transient 修复**（Task 4 代码评审产出）：新一轮运行开始（`run.streaming`）时 `setTransient(null)`，防止上轮瞬态残留。
5. **单一色彩映射源**（Task 5 代码评审产出）：SPECS 用 `colorKey: keyof ParticleColors` 取代 color 闭包；新增 `modeColor(mode, colors)` 导出供 StaticCore 与 buildPreset 共用。
6. **引擎重试与 reset 兜底**（Task 5 代码评审产出）：`ensureEngine` 失败时重置单例允许重试；`container.reset(...).catch(()=>{})` 吞掉卸载竞态的 rejection。
7. **测试类型窄化**：presets.test.ts 有 6 处叶子级 `as any`（life/links 为插件声明字段、outModes/fullScreen 为非递归联合），已加注释说明。
8. **pnpm-workspace.yaml**：pnpm 11 为 @tsparticles/engine 的 advisory install script 写入 `allowBuilds: false`（脚本纯提示性，跳过零影响）。
