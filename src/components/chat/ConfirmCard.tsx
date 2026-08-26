import { useEffect, useState } from "react";
import { WarningCircle, CheckCircle, XCircle, Clock } from "@phosphor-icons/react";
import type { ConfirmRequest } from "@/lib/types";
import { useSessionStore } from "@/store/sessionStore";

const RISK_LABELS: Record<string, string> = {
  read_only: "只读",
  low: "低风险",
  high: "高风险",
};

const CONFIRM_TIMEOUT_SECS = 120;

export function ConfirmCard({ request }: { request: ConfirmRequest }) {
  const confirmToolAction = useSessionStore((s) => s.confirmToolAction);
  const [remaining, setRemaining] = useState(CONFIRM_TIMEOUT_SECS);

  useEffect(() => {
    if (request.resolved !== "pending") return;
    const start = Date.now();
    const timer = setInterval(() => {
      const elapsed = Math.floor((Date.now() - start) / 1000);
      const left = CONFIRM_TIMEOUT_SECS - elapsed;
      setRemaining(left > 0 ? left : 0);
      if (left <= 0) clearInterval(timer);
    }, 1000);
    return () => clearInterval(timer);
  }, [request.resolved]);

  const isPending = request.resolved === "pending";
  const argsObj = request.args as { command?: unknown } | null;
  const command =
    typeof argsObj?.command === "string" ? argsObj.command : JSON.stringify(request.args, null, 2);

  return (
    <div
      className={`rounded-lg border overflow-hidden mb-3 ${
        isPending ? "border-destructive/60 bg-destructive/5" : "border-border bg-card"
      }`}
    >
      <div className="flex items-center gap-2 px-3 py-2">
        <WarningCircle
          size={14}
          weight="fill"
          className={isPending ? "text-destructive shrink-0" : "text-muted-foreground shrink-0"}
          aria-hidden="true"
        />
        <span
          className="text-xs font-semibold text-foreground shrink-0"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {request.tool}
        </span>
        <span className="text-[10px] px-1.5 py-px rounded border bg-destructive/10 text-destructive border-destructive/20 shrink-0">
          {RISK_LABELS[request.risk_level] ?? request.risk_level}
        </span>
        <span className="ml-auto text-xs text-muted-foreground shrink-0 flex items-center gap-1">
          {isPending ? (
            <>
              <Clock size={12} aria-hidden="true" />
              {remaining}s
            </>
          ) : request.resolved === "approved" ? (
            <span className="text-success flex items-center gap-1">
              <CheckCircle size={12} weight="fill" aria-hidden="true" /> 已批准
            </span>
          ) : request.resolved === "rejected" ? (
            <span className="text-destructive flex items-center gap-1">
              <XCircle size={12} weight="fill" aria-hidden="true" /> 已拒绝
            </span>
          ) : (
            "已超时"
          )}
        </span>
      </div>

      <div className="px-3 pb-2">
        <pre
          className="text-xs text-muted-foreground whitespace-pre-wrap break-all bg-background rounded-md px-3 py-2 border border-border max-h-40 overflow-y-auto"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {command}
        </pre>
      </div>

      {isPending && (
        <div className="flex gap-2 px-3 pb-3">
          <button
            onClick={() => confirmToolAction(request.confirm_id, true)}
            className="px-3 py-1.5 rounded-md bg-destructive text-destructive-foreground text-xs hover:bg-destructive/80 transition-colors cursor-pointer"
          >
            批准执行
          </button>
          <button
            onClick={() => confirmToolAction(request.confirm_id, false)}
            className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          >
            拒绝
          </button>
        </div>
      )}
    </div>
  );
}
