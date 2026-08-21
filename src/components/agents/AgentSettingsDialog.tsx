import { useEffect, useRef, useState } from "react";
import { X, CircleNotch, Robot, CaretDown } from "@phosphor-icons/react";
import { useAgentStore } from "@/store/agentStore";
import { AgentListItem } from "@/components/agents/AgentListItem";

interface AgentSettingsDialogProps {
  open: boolean;
  onClose: () => void;
}

export function AgentSettingsDialog({ open, onClose }: AgentSettingsDialogProps) {
  const agents = useAgentStore((s) => s.agents);
  const loading = useAgentStore((s) => s.loading);
  const error = useAgentStore((s) => s.error);
  const refresh = useAgentStore((s) => s.refresh);
  const addManual = useAgentStore((s) => s.addManual);
  const setActive = useAgentStore((s) => s.setActive);
  const remove = useAgentStore((s) => s.remove);

  const dialogRef = useRef<HTMLDialogElement>(null);
  const refreshBtnRef = useRef<HTMLButtonElement>(null);

  const [showAdd, setShowAdd] = useState(false);
  const [provider, setProvider] = useState("opencode");
  const [path, setPath] = useState("");
  const [adding, setAdding] = useState(false);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open) {
      if (!dialog.open) {
        dialog.showModal();
      }
      const raf = requestAnimationFrame(() => refreshBtnRef.current?.focus());
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

  const handleAdd = async () => {
    const trimmed = path.trim();
    if (!trimmed || adding) return;
    setAdding(true);
    try {
      await addManual(provider, trimmed);
      if (!useAgentStore.getState().error) {
        setPath("");
      }
    } finally {
      setAdding(false);
    }
  };

  return (
    <dialog
      ref={dialogRef}
      aria-label="Agent 设置"
      className="z-50 w-[480px] max-w-[90vw] rounded-xl bg-card border border-border p-0 text-foreground overflow-hidden"
    >
      <div className="flex flex-col max-h-[85vh] overflow-hidden rounded-xl">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-border shrink-0">
          <h2 className="text-sm font-medium text-foreground">Agent 设置</h2>
          <button
            onClick={onClose}
            aria-label="关闭"
            className="flex items-center justify-center w-7 h-7 rounded-md text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          >
            <X size={16} weight="regular" aria-hidden="true" />
          </button>
        </div>

        {/* Toolbar */}
        <div className="px-5 py-3 border-b border-border shrink-0">
          <button
            ref={refreshBtnRef}
            onClick={() => refresh()}
            disabled={loading}
            className="flex items-center gap-2 px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {loading && <CircleNotch size={14} className="animate-spin" aria-hidden="true" />}
            重新检测
          </button>
        </div>

        {/* Agent list */}
        <div className="flex-1 overflow-y-auto px-5 py-4 min-h-0">
          {agents.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-10 text-center">
              <Robot size={32} weight="regular" className="text-muted-foreground mb-3" aria-hidden="true" />
              <p className="text-sm text-muted-foreground">
                未检测到 Agent，点击上方[重新检测]或手动添加路径
              </p>
              {error && (
                <p className="text-xs text-destructive mt-2 max-w-[360px] break-words">{error}</p>
              )}
            </div>
          ) : (
            <div className="space-y-2">
              {agents.map((a) => (
                <AgentListItem
                  key={a.id}
                  agent={a}
                  onSetActive={(id) => setActive(id)}
                  onRemove={(id) => remove(id)}
                />
              ))}
            </div>
          )}
        </div>

        {/* Manual add (collapsible) */}
        <div className="border-t border-border shrink-0">
          <button
            onClick={() => setShowAdd((s) => !s)}
            aria-expanded={showAdd}
            className="w-full flex items-center justify-between px-5 py-3 text-sm text-muted-foreground hover:text-foreground transition-colors cursor-pointer"
          >
            <span>手动添加</span>
            <CaretDown
              size={14}
              weight="regular"
              className={`transition-transform ${showAdd ? "rotate-180" : ""}`}
              aria-hidden="true"
            />
          </button>
          {showAdd && (
            <div className="px-5 pb-4 space-y-3">
              <div className="flex items-center gap-3">
                <select
                  value={provider}
                  onChange={(e) => setProvider(e.target.value)}
                  className="bg-muted border border-border rounded-md text-sm text-foreground px-2 py-1.5 cursor-pointer"
                  aria-label="Provider"
                >
                  <option value="opencode">opencode</option>
                  <option value="codeagentcli">codeagentcli</option>
                </select>
                <input
                  type="text"
                  value={path}
                  onChange={(e) => setPath(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") handleAdd();
                  }}
                  placeholder="可执行文件绝对路径"
                  className="flex-1 bg-muted border border-border rounded-md text-sm text-foreground px-3 py-1.5 placeholder:text-muted-foreground/50 outline-none"
                  style={{ fontFamily: "var(--font-mono)" }}
                  aria-label="可执行文件路径"
                />
              </div>
              {error && (
                <p className="text-xs text-destructive break-words">{error}</p>
              )}
              <button
                onClick={handleAdd}
                disabled={!path.trim() || adding}
                className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-accent text-accent-foreground text-xs hover:bg-accent/80 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {adding && <CircleNotch size={14} className="animate-spin" aria-hidden="true" />}
                添加
              </button>
            </div>
          )}
        </div>
      </div>
    </dialog>
  );
}
