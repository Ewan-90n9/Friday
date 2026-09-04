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
  // 注：tsParticles v4 的 RecursivePartial<IOptions> 对未在核心接口声明的
  // particles.color/size/opacity/life/links/shadow 仅保留索引签名（类型为空对象），
  // 测试断言需要读取这些字段，故用 any 视图（运行时数据由断言本身校验）。
  it("thinking 用 accent、36 粒子、无生命期", () => {
    const p: any = buildPreset("thinking", ctx);
    expect(p.particles?.number?.value).toBe(36);
    expect(p.particles?.color?.value).toBe(ctx.colors.accent);
    expect(p.particles?.life?.enable).toBe(false);
  });

  it("executing 用 success、40 粒子、高速", () => {
    const p: any = buildPreset("executing", ctx);
    expect(p.particles?.color?.value).toBe(ctx.colors.success);
    expect(p.particles?.number?.value).toBe(40);
    expect(p.particles?.move?.speed).toBe(2.2);
  });

  it("awaiting 用 warning 且明暗动画同步（屏息）", () => {
    const p: any = buildPreset("awaiting", ctx);
    expect(p.particles?.color?.value).toBe(ctx.colors.warning);
    expect(p.particles?.opacity?.animation?.sync).toBe(true);
    expect(p.particles?.move?.speed).toBe(0.15);
  });

  it("error 用 destructive、一次性生命期、向外飞散", () => {
    const p: any = buildPreset("error", ctx);
    expect(p.particles?.color?.value).toBe(ctx.colors.destructive);
    expect(p.particles?.life?.count).toBe(1);
    expect(p.particles?.move?.direction).toBe("outside");
    expect(p.particles?.move?.outModes?.default).toBe("out");
  });

  it("done 用庆祝紫 token（--particle-celebration，spec §5）", () => {
    const p: any = buildPreset("done", ctx);
    expect(p.particles?.color?.value).toBe(ctx.colors.celebration);
    expect(p.particles?.life?.count).toBe(1);
  });

  it("idle 极低透明度待机呼吸", () => {
    const p: any = buildPreset("idle", ctx);
    expect(p.particles?.number?.value).toBe(14);
    expect(p.particles?.opacity?.value).toEqual({ min: 0.04, max: 0.12 });
    expect(p.particles?.move?.speed).toBe(0.1);
  });

  it("辉光按 glow 缩放（亮色主题 0.4）", () => {
    const full = (buildPreset("thinking", ctx) as any).particles?.shadow?.blur ?? 0;
    const dim = (buildPreset("thinking", { ...ctx, glow: 0.4 }) as any).particles?.shadow?.blur ?? 0;
    expect(dim).toBeCloseTo(full * 0.4, 5);
  });

  it("全屏模式必须关闭（顶栏内嵌关键约束）", () => {
    expect((buildPreset("idle", ctx) as any).fullScreen?.enable).toBe(false);
  });

  it("每个预设都显式关闭 links（防 loadOptions 深合并残留）", () => {
    for (const mode of ["thinking", "executing", "awaiting", "error", "done", "idle"] as const) {
      expect((buildPreset(mode, ctx) as any).particles?.links?.enable).toBe(false);
    }
  });
});
