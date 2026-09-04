import { describe, expect, it } from "vitest";
import { detectTransient, deriveBaseMode, TRANSIENT_MS } from "./deriveMode";

describe("deriveBaseMode（优先级从高到低）", () => {
  it("优先级1：待确认压倒运行", () => {
    expect(
      deriveBaseMode({ pendingConfirm: true, active: true }),
    ).toBe("awaiting");
  });

  it("优先级2：运行中（思考与工具执行合并为一个状态）", () => {
    expect(
      deriveBaseMode({ pendingConfirm: false, active: true }),
    ).toBe("running");
  });

  it("全部静止则沉寂", () => {
    expect(
      deriveBaseMode({ pendingConfirm: false, active: false }),
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

  it("streaming → 无状态（异常断流）无瞬态", () => {
    expect(detectTransient(streaming, { streaming: false, status: null })).toBeNull();
  });

  it("非 streaming 起点不触发", () => {
    expect(
      detectTransient(
        { streaming: false, status: "done" },
        { streaming: false, status: null },
      ),
    ).toBeNull();
  });

  it("仍在 streaming 不触发", () => {
    expect(detectTransient(streaming, streaming)).toBeNull();
  });

  it("瞬态时长钉住 spec 值（error 3s / done 2.6s）", () => {
    expect(TRANSIENT_MS).toEqual({ error: 3000, done: 2600 });
  });
});
