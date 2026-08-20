import { useState } from "react";
import { CaretRight, CaretDown, CheckCircle, XCircle, Spinner } from "@phosphor-icons/react";
import type { ToolCallInfo } from "@/lib/types";

interface ToolCallCardProps {
  tool: ToolCallInfo;
}

export function ToolCallCard({ tool }: ToolCallCardProps) {
  const [expanded, setExpanded] = useState(false);

  const argsStr =
    typeof tool.args === "string"
      ? tool.args
      : JSON.stringify(tool.args, null, 2);

  const isRunning = tool.status === "running";
  const isError = tool.status === "error";

  return (
    <div className="bg-card border border-border rounded-lg overflow-hidden mb-3">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-2 px-3 py-2 w-full hover:bg-surface-2 transition-colors text-left"
      >
        {expanded ? (
          <CaretDown size={12} weight="bold" className="text-muted-foreground shrink-0" aria-hidden="true" />
        ) : (
          <CaretRight size={12} weight="bold" className="text-muted-foreground shrink-0" aria-hidden="true" />
        )}
        <span
          className="text-xs font-semibold text-success bg-success/10 px-1.5 py-0.5 rounded shrink-0"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {tool.name}
        </span>
        <span
          className="text-xs text-foreground truncate flex-1"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {argsStr}
        </span>
        <span
          className="text-xs shrink-0 flex items-center gap-1"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {isRunning ? (
            <span className="text-accent flex items-center gap-1">
              <Spinner size={12} className="animate-spin" aria-hidden="true" />
              执行中...
            </span>
          ) : isError ? (
            <span className="text-destructive flex items-center gap-1">
              <XCircle size={12} weight="fill" aria-hidden="true" />
              失败
            </span>
          ) : (
            <span className="text-success flex items-center gap-1">
              <CheckCircle size={12} weight="fill" aria-hidden="true" />
              {tool.elapsedMs ? `${(tool.elapsedMs / 1000).toFixed(1)}s` : ""}
            </span>
          )}
        </span>
      </button>
      {expanded && tool.output && (
        <div className="border-t border-border px-3 py-2 bg-background max-h-40 overflow-y-auto">
          <pre
            className="text-xs text-muted-foreground whitespace-pre-wrap break-all"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            {tool.output}
          </pre>
        </div>
      )}
    </div>
  );
}
