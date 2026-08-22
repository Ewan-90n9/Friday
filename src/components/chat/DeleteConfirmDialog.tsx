import { useEffect } from "react";
import { Warning } from "@phosphor-icons/react";

interface DeleteConfirmDialogProps {
  open: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}

export function DeleteConfirmDialog({ open, onCancel, onConfirm }: DeleteConfirmDialogProps) {
  useEffect(() => {
    if (!open) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onCancel();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [open, onCancel]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      style={{ backgroundColor: "rgba(0, 0, 0, 0.6)" }}
      onClick={onCancel}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="delete-dialog-title"
        className="bg-card border border-border rounded-xl p-6 max-w-sm w-full mx-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start gap-3 mb-4">
          <div className="flex items-center justify-center w-8 h-8 rounded-lg bg-destructive/10 shrink-0">
            <Warning size={18} weight="regular" className="text-destructive" aria-hidden="true" />
          </div>
          <div>
            <h3
              id="delete-dialog-title"
              className="text-foreground text-sm font-medium mb-1"
              style={{ fontFamily: "var(--font-sans)" }}
            >
              删除会话
            </h3>
            <p className="text-muted-foreground text-xs leading-relaxed">
              确定删除该会话？删除后不可恢复。
            </p>
          </div>
        </div>
        <div className="flex items-center justify-end gap-2">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-xs text-muted-foreground hover:text-foreground bg-surface-2 hover:bg-surface-3 rounded-md transition-colors border border-border"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            取消
          </button>
          <button
            onClick={onConfirm}
            className="px-3 py-1.5 text-xs text-destructive-foreground bg-destructive hover:bg-destructive/80 rounded-md transition-colors"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            确认删除
          </button>
        </div>
      </div>
    </div>
  );
}
