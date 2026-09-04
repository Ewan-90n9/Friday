/** ParticleCore 五态（spec §3/§4 修订：思考与工具执行合并为 running，颜色跟时间走） */
export type ParticleMode =
  | "running"
  | "awaiting"
  | "error"
  | "done"
  | "idle";

/** 基础模式：由 store 信号即时派生（瞬态 error/done 除外） */
export type BaseMode = Exclude<ParticleMode, "error" | "done">;

export interface ModeInput {
  pendingConfirm: boolean;
  /** 运行中：LLM 流式输出或任一工具执行中（思考与工具执行合并为一个状态） */
  active: boolean;
}

/** 优先级：awaiting > running > idle（spec §3 修订） */
export function deriveBaseMode(input: ModeInput): BaseMode {
  if (input.pendingConfirm) return "awaiting";
  if (input.active) return "running";
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
