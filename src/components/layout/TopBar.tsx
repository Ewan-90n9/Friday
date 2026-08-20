import { useState, useRef } from "react";
import { GearSix } from "@phosphor-icons/react";
import { FridayMark } from "@/components/FridayMark";
import { useAgentStore } from "@/store/agentStore";
import { AgentSettingsDialog } from "@/components/agents/AgentSettingsDialog";
import type { AgentRow } from "@/lib/types";

function computeStatus(
  loading: boolean,
  activeAgent: AgentRow | null,
  error: string | null,
): { label: string; dotClass: string; pulse: boolean } {
  if (loading) return { label: "检测中…", dotClass: "bg-accent", pulse: true };
  if (activeAgent) {
    if (activeAgent.version) {
      return {
        label: `${activeAgent.display_name} v${activeAgent.version}`,
        dotClass: "bg-success",
        pulse: false,
      };
    }
    return {
      label: `${activeAgent.display_name} 版本未知`,
      dotClass: "bg-success",
      pulse: false,
    };
  }
  if (error) return { label: "检测失败", dotClass: "bg-warning", pulse: false };
  return { label: "未检测到 Agent", dotClass: "bg-destructive", pulse: false };
}

export function TopBar() {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const activeAgent = useAgentStore((s) => s.activeAgent);
  const loading = useAgentStore((s) => s.loading);
  const error = useAgentStore((s) => s.error);

  const { label, dotClass, pulse } = computeStatus(loading, activeAgent, error);

  const handleClose = () => {
    setSettingsOpen(false);
    triggerRef.current?.focus();
  };

  return (
    <header
      className="flex items-center justify-between h-12 px-4 shrink-0 border-b border-border bg-surface-1"
      style={{ backgroundImage: "linear-gradient(180deg, var(--color-surface-2) 0%, var(--color-surface-1) 100%)" }}
    >
      {/* 左侧：品牌 */}
      <div className="flex items-center gap-3 min-w-0">
        <div className="flex items-center gap-2">
          <FridayMark size={22} />
          <span
            className="text-foreground text-sm font-semibold tracking-wide"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            Friday
          </span>
        </div>
      </div>

      {/* 右侧：状态 + 设置 */}
      <div className="flex items-center gap-1">
        <button
          ref={triggerRef}
          onClick={() => setSettingsOpen(true)}
          className="flex items-center gap-2 px-2.5 py-1 rounded-md bg-muted/50 hover:bg-muted transition-colors cursor-pointer"
          aria-label={`${label}，点击打开设置`}
        >
          <span className="relative flex w-1.5 h-1.5">
            {pulse ? (
              <>
                <span className={`absolute inline-flex w-full h-full rounded-full ${dotClass} opacity-60 animate-ping`} />
                <span className={`relative inline-flex w-1.5 h-1.5 rounded-full ${dotClass}`} />
              </>
            ) : (
              <span className={`relative inline-flex w-1.5 h-1.5 rounded-full ${dotClass}`} />
            )}
          </span>
          <span className="text-muted-foreground text-xs">{label}</span>
        </button>
        <button
          onClick={() => setSettingsOpen(true)}
          className="flex items-center justify-center w-8 h-8 rounded-md text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          aria-label="Agent 设置"
        >
          <GearSix size={18} weight="regular" />
        </button>
      </div>

      <AgentSettingsDialog open={settingsOpen} onClose={handleClose} />
    </header>
  );
}
