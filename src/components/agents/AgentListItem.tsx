import { useState } from "react";
import type { AgentRow } from "@/lib/types";

interface AgentListItemProps {
  agent: AgentRow;
  onSetActive: (id: string) => void;
  onRemove: (id: string) => void;
}

export function AgentListItem({ agent, onSetActive, onRemove }: AgentListItemProps) {
  const [confirmingRemove, setConfirmingRemove] = useState(false);
  const isActive = agent.is_active;

  return (
    <div
      className={`flex items-center gap-3 px-4 py-3 rounded-lg border border-border bg-surface-1 hover:bg-surface-3 transition-colors ${
        isActive ? "border-l-2 border-l-success" : ""
      }`}
    >
      <span
        className={`w-1.5 h-1.5 rounded-full shrink-0 ${isActive ? "bg-success" : "bg-muted-foreground"}`}
        aria-hidden="true"
      />

      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm text-foreground truncate" style={{ fontFamily: "var(--font-sans)" }}>
            {agent.display_name}
          </span>
          <span
            className="text-xs text-muted-foreground shrink-0"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            {agent.version ?? "版本未知"}
          </span>
          <span className="text-xs px-1.5 py-0.5 rounded-sm bg-muted/50 text-muted-foreground shrink-0">
            {agent.source}
          </span>
        </div>
        <div
          className="text-xs text-muted-foreground truncate"
          style={{ fontFamily: "var(--font-mono)" }}
          title={agent.path}
        >
          {agent.path}
        </div>
      </div>

      <div className="flex items-center gap-2 shrink-0">
        {confirmingRemove ? (
          <>
            <span className="text-xs text-muted-foreground">确认移除？</span>
            <button
              onClick={() => onRemove(agent.id)}
              className="text-xs text-destructive hover:text-destructive/80 cursor-pointer"
            >
              确认
            </button>
            <button
              onClick={() => setConfirmingRemove(false)}
              className="text-xs text-muted-foreground hover:text-foreground cursor-pointer"
            >
              取消
            </button>
          </>
        ) : (
          <>
            {!isActive && (
              <button
                onClick={() => onSetActive(agent.id)}
                className="text-xs text-accent hover:text-accent/80 cursor-pointer"
              >
                设为当前
              </button>
            )}
            <button
              onClick={() => setConfirmingRemove(true)}
              className="text-xs text-muted-foreground hover:text-destructive cursor-pointer"
            >
              移除
            </button>
          </>
        )}
      </div>
    </div>
  );
}
