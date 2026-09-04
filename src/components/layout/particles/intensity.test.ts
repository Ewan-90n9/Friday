import { describe, expect, it } from "vitest";
import { INTENSITY_FULL_MS, runIntensity } from "./intensity";

describe("runIntensity（颜色跟时间走：平静绿 → 激烈黄绿，5 分钟满强度）", () => {
  it("0ms → 单位滤镜（不改变颜色）", () => {
    expect(runIntensity(0)).toEqual({ saturate: 1, brightness: 1, hueRotate: 0 });
  });

  it("负值按 0 处理 → 单位滤镜", () => {
    expect(runIntensity(-5000)).toEqual({ saturate: 1, brightness: 1, hueRotate: 0 });
  });

  it("60s → 线性插值 (1.09, 1.05, 5)", () => {
    const r = runIntensity(60_000);
    expect(r.saturate).toBeCloseTo(1.09, 10);
    expect(r.brightness).toBeCloseTo(1.05, 10);
    expect(r.hueRotate).toBeCloseTo(5, 10);
  });

  it("150s（半程）→ 精确中点 (1.225, 1.125, 12.5)", () => {
    const r = runIntensity(150_000);
    expect(r.saturate).toBeCloseTo(1.225, 10);
    expect(r.brightness).toBeCloseTo(1.125, 10);
    expect(r.hueRotate).toBeCloseTo(12.5, 10);
  });

  it(`${INTENSITY_FULL_MS}ms → 满强度 (1.45, 1.25, 25)`, () => {
    const r = runIntensity(INTENSITY_FULL_MS);
    expect(r.saturate).toBeCloseTo(1.45, 10);
    expect(r.brightness).toBeCloseTo(1.25, 10);
    expect(r.hueRotate).toBeCloseTo(25, 10);
  });

  it("超过满强度时长 → 钳制在满强度", () => {
    const r = runIntensity(600_000);
    expect(r.saturate).toBeCloseTo(1.45, 10);
    expect(r.brightness).toBeCloseTo(1.25, 10);
    expect(r.hueRotate).toBeCloseTo(25, 10);
  });
});
