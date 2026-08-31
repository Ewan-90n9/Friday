import { useEffect, useRef } from "react";

interface DiscardChangesDialogProps {
  open: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function DiscardChangesDialog({ open, onConfirm, onCancel }: DiscardChangesDialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  }, [open]);

  return (
    <dialog
      ref={dialogRef}
      aria-label="放弃未保存变更"
      className="z-[60] w-[360px] max-w-[90vw] rounded-xl bg-card border border-border p-0 text-foreground overflow-hidden"
      onClose={onCancel}
    >
      <div className="px-5 py-4">
        <h2 className="text-sm font-medium mb-2">放弃未保存的变更？</h2>
        <p className="text-xs text-muted-foreground leading-relaxed">
          凭证修改尚未保存，关闭后本次变更将丢失。
        </p>
        <div className="flex justify-end gap-2 mt-4">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs hover:bg-surface-3 transition-colors cursor-pointer"
          >
            继续编辑
          </button>
          <button
            onClick={onConfirm}
            className="px-3 py-1.5 rounded-md bg-destructive text-destructive-foreground text-xs hover:bg-destructive/80 transition-colors cursor-pointer"
          >
            放弃变更
          </button>
        </div>
      </div>
    </dialog>
  );
}
