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
  active: boolean;
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
  // 思考与工具执行合并为一个 running 状态（spec 修订：颜色跟时间走）
  const active = agentStreaming || toolRunning;
  return { sessionId, pendingConfirm, active, lastAgentStatus };
}

export interface ParticleState {
  mode: ParticleMode;
  /** 当前 run 的开始时间戳（ms）；无进行中的 run 时为 null。awaiting 中途不打断计时 */
  runStartedAt: number | null;
}

export function useParticleMode(): ParticleState {
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
  // 当前 run 起点：running 且尚无起点时记下，idle 清空；awaiting 保持（同 run 内不打断计时）
  const [runStartedAt, setRunStartedAt] = useState<number | null>(null);
  const prevRef = useRef<{ sessionId: string | null; run: LastRunState }>({
    sessionId: null,
    run: { streaming: false, status: null },
  });

  useEffect(() => {
    const run: LastRunState = {
      streaming: signals.active,
      status: signals.active ? "streaming" : signals.lastAgentStatus,
    };
    const prev = prevRef.current;

    // 会话切换：直接重置快照并清除旧瞬态，防止把旧会话的完成误判成本会话瞬态
    if (prev.sessionId !== signals.sessionId) {
      prevRef.current = { sessionId: signals.sessionId, run };
      setTransient(null);
      return;
    }

    // 新一轮运行开始：清除上一轮可能残留的瞬态（timer 已被 cleanup 清掉，但 state 未必）
    if (run.streaming) {
      setTransient(null);
    }

    const t = detectTransient(prev.run, run);
    prevRef.current = { sessionId: signals.sessionId, run };
    if (t) {
      setTransient({ mode: t.mode });
      const timer = window.setTimeout(() => setTransient(null), t.durationMs);
      return () => window.clearTimeout(timer);
    }
  }, [signals.sessionId, signals.active, signals.lastAgentStatus]);

  // run 计时：running 且尚无起点 → 记下当前时刻；回到 idle → 清空；awaiting 不动
  useEffect(() => {
    if (base === "running") {
      setRunStartedAt((prev) => prev ?? Date.now());
    } else if (base === "idle") {
      setRunStartedAt(null);
    }
  }, [base]);

  // 基础模式非 idle 时压倒瞬态（spec §3 优先级）；瞬态期间 base 已是 idle，计时随之清空
  const mode: ParticleMode = base !== "idle" ? base : (transient?.mode ?? "idle");
  return { mode, runStartedAt };
}
