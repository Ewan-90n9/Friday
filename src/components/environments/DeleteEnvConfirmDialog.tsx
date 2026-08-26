import { useEffect, useRef } from "react";
import type { EnvironmentRow } from "@/lib/types";

interface DeleteEnvConfirmDialogProps {
  env: EnvironmentRow | null;
  onConfirm: () => void;
  onCancel: () => void;
}

export function DeleteEnvConfirmDialog({ env, onConfirm, onCancel }: DeleteEnvConfirmDialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (env && !dialog.open) dialog.showModal();
    if (!env && dialog.open) dialog.close();
  }, [env]);

  return (
    <dialog
      ref={dialogRef}
      aria-label="确认删除环境"
      className="z-50 w-[360px] max-w-[90vw] rounded-xl bg-card border border-border p-0 text-foreground overflow-hidden"
      onClose={onCancel}
    >
      <div className="px-5 py-4">
        <h2 className="text-sm font-medium mb-2">删除环境</h2>
        <p className="text-xs text-muted-foreground leading-relaxed">
          确定删除环境 <span className="text-foreground font-medium">{env?.name}</span>（{env?.host}）？
          同时删除密钥链中保存的凭证，不影响正在进行的诊断会话。
        </p>
        <div className="flex justify-end gap-2 mt-4">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs hover:bg-surface-3 transition-colors cursor-pointer"
          >
            取消
          </button>
          <button
            onClick={onConfirm}
            className="px-3 py-1.5 rounded-md bg-destructive text-destructive-foreground text-xs hover:bg-destructive/80 transition-colors cursor-pointer"
          >
            删除
          </button>
        </div>
      </div>
    </dialog>
  );
}
