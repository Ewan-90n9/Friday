import { useEffect, useMemo, useRef, useState } from "react";
import { tsParticles, type Container } from "@tsparticles/engine";
import { loadSlim } from "@tsparticles/slim";
import { useThemeStore } from "@/store/themeStore";
import { useParticleMode } from "./particles/useParticleMode";
import {
  buildPreset,
  modeColor,
  readParticleColors,
  type ParticleColors,
} from "./particles/presets";
import { runIntensity } from "./particles/intensity";
import type { ParticleMode } from "./particles/deriveMode";

const CANVAS_ID = "friday-particle-core";
const ZONE_WIDTH = 80;
const ZONE_HEIGHT = 36;

// 引擎只加载一次（StrictMode 双挂载/组件重挂载时复用）
let engineReady: Promise<void> | null = null;
function ensureEngine(): Promise<void> {
  engineReady ??= loadSlim(tsParticles).catch((e) => {
    // 加载失败：重置单例，下次挂载重试（而非永久卡死在 rejected promise）
    engineReady = null;
    throw e;
  });
  return engineReady;
}

export function ParticleCore() {
  const zoneRef = useRef<HTMLDivElement | null>(null);
  const containerRef = useRef<Container | null>(null);
  const [ready, setReady] = useState(false);
  const [colors, setColors] = useState<ParticleColors>(() => readParticleColors());
  const theme = useThemeStore((s) => s.theme);
  const { mode, runStartedAt } = useParticleMode();
  const [now, setNow] = useState(() => Date.now());
  const reducedMotion = useMemo(
    () => window.matchMedia("(prefers-reduced-motion: reduce)").matches,
    [],
  );

  const glow = theme === "dark" ? 1 : 0.4;

  // 运行中每秒刷新当前时刻，驱动激烈度插值（仅重渲染本小组件，可接受）
  useEffect(() => {
    if (mode !== "running") return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [mode]);

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
    container.reset(buildPreset(mode, { colors, glow })).catch(() => {
      // 容器已销毁（卸载/快速模式切换）——静默忽略
    });
  }, [mode, colors, glow, ready]);

  // 运行激烈度滤镜：颜色跟时间走，从平静绿渐变到激烈黄绿（spec 修订）
  let filter: string | undefined;
  if (mode === "running" && runStartedAt !== null) {
    const intensity = runIntensity(now - runStartedAt);
    filter = `saturate(${intensity.saturate}) brightness(${intensity.brightness}) hue-rotate(${intensity.hueRotate}deg)`;
  }

  if (reducedMotion) {
    return <StaticCore mode={mode} colors={colors} />;
  }

  return (
    <div
      ref={zoneRef}
      className="shrink-0"
      style={{ width: ZONE_WIDTH, height: ZONE_HEIGHT, filter }}
      aria-hidden="true"
    />
  );
}

/** reduced-motion 静态形态：当前模式颜色 的 4 个静止色点，无任何动画（spec §6） */
function StaticCore({ mode, colors }: { mode: ParticleMode; colors: ParticleColors }) {
  const color = modeColor(mode, colors);
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
