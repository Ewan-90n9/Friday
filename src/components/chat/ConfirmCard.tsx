import { useEffect, useState } from "react";
import { WarningCircle, CheckCircle, XCircle, Clock } from "@phosphor-icons/react";
import type { ConfirmRequest } from "@/lib/types";
import { useSessionStore } from "@/store/sessionStore";

const RISK_LABELS: Record<string, string> = {
  read_only: "只读",
  low: "低风险",
  high: "高风险",
};

/** 待确认卡片样式按风险级分级（设计语言 §5.4：低=黄边条，高=红边条+警告底色） */
const RISK_STYLES: Record<string, { card: string; icon: string; badge: string; approve: string }> = {
  low: {
    card: "border-warning/60 bg-warning/5",
    icon: "text-warning",
    badge: "bg-warning/10 text-warning border-warning/20",
    approve: "bg-warning text-warning-foreground hover:bg-warning/80",
  },
  high: {
    card: "border-destructive/60 bg-destructive/5",
    icon: "text-destructive",
    badge: "bg-destructive/10 text-destructive border-destructive/20",
    approve: "bg-destructive text-destructive-foreground hover:bg-destructive/80",
  },
};

/** 已知工具的操作后果说明；缺失时按风险级回退 */
const TOOL_CONSEQUENCES: Record<string, string> = {
  run_command: "将在目标环境执行任意 shell 命令，可能改变系统状态",
  jvm_class_histogram: "默认 live 视图会触发一次 Full GC，可能造成服务短暂停顿",
  jvm_heap_dump: "将触发 Full GC 并生成堆转储文件，可能导致服务长时间停顿与磁盘占用",
};

const DEFAULT_CONSEQUENCES: Record<string, string> = {
  low: "该操作可能对目标环境产生轻微影响",
  high: "高风险操作，可能影响目标服务稳定性",
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
  const isHighRisk = request.risk_level === "high";
  // 未知风险级按高风险呈现（保守默认）
  const style = RISK_STYLES[request.risk_level] ?? RISK_STYLES.high;
  const consequence =
    TOOL_CONSEQUENCES[request.tool] ??
    DEFAULT_CONSEQUENCES[request.risk_level] ??
    DEFAULT_CONSEQUENCES.high;

  const argsObj = request.args as { command?: unknown } | null;
  const command =
    typeof argsObj?.command === "string" ? argsObj.command : JSON.stringify(request.args, null, 2);

  return (
    <div
      role={isPending ? "alert" : undefined}
      className={`rounded-lg border overflow-hidden mb-3 ${
        isPending ? style.card : "border-border bg-card"
      }`}
    >
      <div className="flex items-center gap-2 px-3 py-2">
        <WarningCircle
          size={14}
          weight="fill"
          className={`${isPending ? style.icon : "text-muted-foreground"} shrink-0`}
          aria-hidden="true"
        />
        <span
          className="text-xs font-semibold text-foreground shrink-0"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {request.tool}
        </span>
        <span className={`text-[10px] px-1.5 py-px rounded border shrink-0 ${style.badge}`}>
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
        <>
          <p className="px-3 pb-2 text-xs leading-5 text-foreground/80">{consequence}</p>
          <div className="flex gap-2 px-3 pb-3">
            <button
              onClick={() => confirmToolAction(request.confirm_id, true)}
              className={`px-3 py-1.5 rounded-md text-xs transition-colors cursor-pointer ${style.approve}`}
            >
              {isHighRisk ? "我已了解风险，确认执行" : "确认执行"}
            </button>
            <button
              onClick={() => confirmToolAction(request.confirm_id, false)}
              className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
            >
              拒绝
            </button>
          </div>
        </>
      )}
    </div>
  );
}
