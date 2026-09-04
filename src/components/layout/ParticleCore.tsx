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
    // glow 仅初值使用，故不列入依赖
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
