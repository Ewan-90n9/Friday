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
  toolRunning: boolean;
  agentStreaming: boolean;
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
  return { sessionId, pendingConfirm, toolRunning, agentStreaming, lastAgentStatus };
}

export function useParticleMode(): ParticleMode {
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
  const prevRef = useRef<{ sessionId: string | null; run: LastRunState }>({
    sessionId: null,
    run: { streaming: false, status: null },
  });

  useEffect(() => {
    const run: LastRunState = {
      streaming: signals.agentStreaming,
      status: signals.agentStreaming ? "streaming" : signals.lastAgentStatus,
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
  }, [signals.sessionId, signals.agentStreaming, signals.lastAgentStatus]);

  // 基础模式非 idle 时压倒瞬态（spec §3 优先级）
  if (base !== "idle") return base;
  return transient?.mode ?? "idle";
}
