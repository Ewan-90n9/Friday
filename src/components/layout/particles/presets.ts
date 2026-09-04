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

/** fallback 值与 globals.css :root 保持同步 */
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
  colorKey: keyof ParticleColors;
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
    count: 36, colorKey: "accent",
    sizeMin: 0.8, sizeMax: 2.2,
    speed: 0.6, direction: "none", straight: false, outMode: "bounce",
    opacityMin: 0.25, opacityMax: 0.85, twinkleSpeed: 0.8, syncTwinkle: false,
    lifeSeconds: null, lifeDelayMax: null, glowBlur: 8,
  },
  executing: {
    count: 40, colorKey: "success",
    sizeMin: 1.0, sizeMax: 2.6,
    speed: 2.2, direction: "none", straight: false, outMode: "bounce",
    opacityMin: 0.35, opacityMax: 1.0, twinkleSpeed: 2.4, syncTwinkle: false,
    lifeSeconds: null, lifeDelayMax: null, glowBlur: 10,
  },
  awaiting: {
    count: 24, colorKey: "warning",
    sizeMin: 0.8, sizeMax: 1.8,
    speed: 0.15, direction: "none", straight: false, outMode: "bounce",
    opacityMin: 0.4, opacityMax: 0.8, twinkleSpeed: 0.4, syncTwinkle: true,
    lifeSeconds: null, lifeDelayMax: null, glowBlur: 6,
  },
  error: {
    count: 40, colorKey: "destructive",
    sizeMin: 1.0, sizeMax: 2.4,
    speed: 3.0, direction: "outside", straight: true, outMode: "out",
    opacityMin: 0.4, opacityMax: 1.0, twinkleSpeed: 1.2, syncTwinkle: false,
    lifeSeconds: 3, lifeDelayMax: 1.2, glowBlur: 10,
  },
  done: {
    count: 40, colorKey: "celebration",
    sizeMin: 1.0, sizeMax: 2.4,
    speed: 1.2, direction: "outside", straight: true, outMode: "out",
    opacityMin: 0.5, opacityMax: 1.0, twinkleSpeed: 1.5, syncTwinkle: false,
    lifeSeconds: 2.6, lifeDelayMax: 1.0, glowBlur: 12,
  },
  idle: {
    count: 14, colorKey: "accent",
    sizeMin: 0.6, sizeMax: 1.4,
    speed: 0.1, direction: "none", straight: false, outMode: "bounce",
    opacityMin: 0.04, opacityMax: 0.12, twinkleSpeed: 0.15, syncTwinkle: false,
    lifeSeconds: null, lifeDelayMax: null, glowBlur: 3,
  },
};

/** 模式 → 颜色（单一事实来源：SPECS.colorKey，StaticCore 与 buildPreset 共用） */
export function modeColor(mode: ParticleMode, colors: ParticleColors): string {
  return colors[SPECS[mode].colorKey];
}

/**
 * 构建指定模式的 tsParticles 选项。
 * 注意：每个预设显式声明全部可变字段——无论上层用整体重建（reset）还是
 * 深合并（options.load）方式应用，都不会残留上一模式的状态。
 */
export function buildPreset(
  mode: ParticleMode,
  ctx: PresetContext,
): RecursivePartial<IOptions> {
  const spec = SPECS[mode];
  const color = ctx.colors[spec.colorKey];
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
