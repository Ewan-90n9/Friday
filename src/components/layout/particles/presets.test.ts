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
  it("thinking 用 accent、36 粒子、无生命期", () => {
    const p = buildPreset("thinking", ctx);
    expect(p.particles?.number?.value).toBe(36);
    expect(p.particles?.color?.value).toBe(ctx.colors.accent);
    // 字段在 v3 engine 全局类型未声明/为联合类型（life/links 为插件声明，outModes/fullScreen 为非递归联合），叶子级 as any 是最小妥协
    expect((p.particles?.life as any)?.count).toBe(0);
    expect((p.particles?.life as any)?.duration?.value).toBe(0);
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
    expect((p.particles?.life as any)?.count).toBe(1);
    expect(p.particles?.move?.direction).toBe("outside");
    expect((p.particles?.move?.outModes as any)?.default).toBe("out");
  });

  it("done 用庆祝紫 token（--particle-celebration，spec §5）", () => {
    const p = buildPreset("done", ctx);
    expect(p.particles?.color?.value).toBe(ctx.colors.celebration);
    expect((p.particles?.life as any)?.count).toBe(1);
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
    expect((buildPreset("idle", ctx).fullScreen as any)?.enable).toBe(false);
  });

  it("每个预设都显式关闭 links（防 loadOptions 深合并残留）", () => {
    for (const mode of ["thinking", "executing", "awaiting", "error", "done", "idle"] as const) {
      expect((buildPreset(mode, ctx).particles?.links as any)?.enable).toBe(false);
    }
  });
});
