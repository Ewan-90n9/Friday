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
