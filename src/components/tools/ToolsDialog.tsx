import { useEffect, useRef, useState } from "react";
import { X, CircleNotch, Wrench } from "@phosphor-icons/react";
import { listTools } from "@/lib/ipc";
import type { ToolInfo } from "@/lib/types";

interface ToolsDialogProps {
  open: boolean;
  onClose: () => void;
}

const RISK_LABELS: Record<string, { label: string; className: string }> = {
  read_only: { label: "只读", className: "bg-success/10 text-success border-success/20" },
  low: { label: "低风险", className: "bg-warning/10 text-warning border-warning/20" },
  high: { label: "高风险", className: "bg-destructive/10 text-destructive border-destructive/20" },
};

export function ToolsDialog({ open, onClose }: ToolsDialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const closeBtnRef = useRef<HTMLButtonElement>(null);
  const [tools, setTools] = useState<ToolInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open) {
      if (!dialog.open) {
        dialog.showModal();
      }
      const raf = requestAnimationFrame(() => closeBtnRef.current?.focus());
      return () => cancelAnimationFrame(raf);
    }
    if (dialog.open) {
      dialog.close();
    }
  }, [open]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const handleClose = () => onClose();
    dialog.addEventListener("close", handleClose);
    return () => dialog.removeEventListener("close", handleClose);
  }, [onClose]);

  useEffect(() => {
    if (!open || tools !== null) return;
    listTools()
      .then(setTools)
      .catch((e) => setError(String(e)));
  }, [open, tools]);

  return (
    <dialog
      ref={dialogRef}
      aria-label="诊断工具"
      className="z-50 w-[480px] max-w-[90vw] rounded-xl bg-card border border-border p-0 text-foreground overflow-hidden"
    >
      <div className="flex flex-col max-h-[85vh] overflow-hidden rounded-xl">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-border shrink-0">
          <div className="flex items-center gap-2">
            <Wrench size={16} weight="regular" className="text-muted-foreground" aria-hidden="true" />
            <h2 className="text-sm font-medium text-foreground">诊断工具</h2>
            {tools && (
              <span className="text-xs text-muted-foreground">{tools.length} 个</span>
            )}
          </div>
          <button
            ref={closeBtnRef}
            onClick={onClose}
            aria-label="关闭"
            className="flex items-center justify-center w-7 h-7 rounded-md text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          >
            <X size={16} weight="regular" aria-hidden="true" />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto px-5 py-4">
          {error && (
            <div className="text-destructive text-xs mb-3">{error}</div>
          )}
          {tools === null && !error && (
            <div className="flex items-center justify-center gap-2 py-8 text-muted-foreground text-sm">
              <CircleNotch size={16} weight="regular" className="animate-spin" aria-hidden="true" />
              加载中…
            </div>
          )}
          {tools !== null && tools.length === 0 && (
            <div className="py-8 text-center text-muted-foreground text-sm">
              暂无已注册工具
            </div>
          )}
          {tools !== null && tools.length > 0 && (
            <ul className="flex flex-col gap-2">
              {tools.map((tool) => {
                const risk = RISK_LABELS[tool.risk_level] ?? {
                  label: tool.risk_level,
                  className: "bg-muted/50 text-muted-foreground border-border",
                };
                return (
                  <li
                    key={tool.name}
                    className="flex items-start gap-3 px-3 py-2.5 rounded-lg border border-border bg-surface-1"
                  >
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2 mb-0.5">
                        <code
                          className="text-xs text-foreground font-medium"
                          style={{ fontFamily: "var(--font-mono)" }}
                        >
                          {tool.name}
                        </code>
                        <span
                          className={`shrink-0 px-1.5 py-px rounded text-[10px] border ${risk.className}`}
                          style={{ fontFamily: "var(--font-mono)" }}
                        >
                          {risk.label}
                        </span>
                      </div>
                      <p className="text-xs text-muted-foreground leading-relaxed">
                        {tool.description}
                      </p>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </div>
    </dialog>
  );
}
