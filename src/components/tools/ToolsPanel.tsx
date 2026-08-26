import { useEffect, useState } from "react";
import { Wrench, CircleNotch } from "@phosphor-icons/react";
import { listTools } from "@/lib/ipc";
import type { ToolInfo } from "@/lib/types";

const RISK_LABELS: Record<string, { label: string; className: string }> = {
  read_only: { label: "只读", className: "bg-success/10 text-success border-success/20" },
  low: { label: "低风险", className: "bg-warning/10 text-warning border-warning/20" },
  high: { label: "高风险", className: "bg-destructive/10 text-destructive border-destructive/20" },
};

export function ToolsPanel() {
  const [tools, setTools] = useState<ToolInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    listTools()
      .then(setTools)
      .catch((e) => setError(String(e)));
  }, []);

  return (
    <section className="flex-1 flex flex-col min-h-0">
      {/* Header */}
      <div className="flex items-center gap-2 h-10 px-4 border-b border-border shrink-0">
        <Wrench size={14} weight="regular" className="text-muted-foreground" aria-hidden="true" />
        <span
          className="text-xs font-medium text-muted-foreground uppercase tracking-wide"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          诊断工具
        </span>
        {tools && (
          <span className="text-xs text-muted-foreground/60 ml-auto">{tools.length}</span>
        )}
      </div>

      {/* Tool list */}
      <div className="flex-1 overflow-y-auto px-3 py-3">
        {error && (
          <div className="text-destructive text-xs px-1 py-2">{error}</div>
        )}
        {tools === null && !error && (
          <div className="flex items-center justify-center gap-2 py-8 text-muted-foreground text-xs">
            <CircleNotch size={14} weight="regular" className="animate-spin" aria-hidden="true" />
            加载中…
          </div>
        )}
        {tools !== null && tools.length === 0 && (
          <div className="py-8 text-center text-muted-foreground text-xs leading-relaxed">
            暂无已注册工具
          </div>
        )}
        {tools !== null && tools.length > 0 && (
          <ul className="flex flex-col gap-1.5">
            {tools.map((tool) => {
              const risk = RISK_LABELS[tool.risk_level] ?? {
                label: tool.risk_level,
                className: "bg-muted/50 text-muted-foreground border-border",
              };
              return (
                <li
                  key={tool.name}
                  className="px-2.5 py-2 rounded-lg border border-border bg-surface-2/50"
                >
                  <div className="flex items-center gap-1.5 mb-1">
                    <span
                      className="w-1 h-1 rounded-full shrink-0 bg-muted-foreground/60"
                      aria-hidden="true"
                    />
                    <code
                      className="text-xs text-foreground font-medium truncate"
                      style={{ fontFamily: "var(--font-mono)" }}
                      title={tool.name}
                    >
                      {tool.name}
                    </code>
                    <span
                      className={`shrink-0 ml-auto px-1.5 py-px rounded text-[10px] border ${risk.className}`}
                      style={{ fontFamily: "var(--font-mono)" }}
                    >
                      {risk.label}
                    </span>
                  </div>
                  <p className="text-xs text-muted-foreground leading-relaxed line-clamp-3">
                    {tool.description}
                  </p>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </section>
  );
}
