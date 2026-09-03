import { useEffect, useRef, useState } from "react";
import { X, CircleNotch, Robot, CaretDown } from "@phosphor-icons/react";
import { useAgentStore } from "@/store/agentStore";
import { useSettingsStore } from "@/store/settingsStore";
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

  const artifactoryBaseUrl = useSettingsStore((s) => s.artifactoryBaseUrl);
  const settingsError = useSettingsStore((s) => s.error);
  const saveBaseUrl = useSettingsStore((s) => s.saveBaseUrl);
  const loadSettings = useSettingsStore((s) => s.load);

  const [urlDraft, setUrlDraft] = useState("");
  const [savingUrl, setSavingUrl] = useState(false);

  const autoApprove = useSettingsStore((s) => s.autoApprove);
  const saveAutoApprove = useSettingsStore((s) => s.saveAutoApprove);

  const [confirmAutoApprove, setConfirmAutoApprove] = useState(false);
  const [savingAutoApprove, setSavingAutoApprove] = useState(false);

  const handleToggleAutoApprove = async (next: boolean) => {
    if (!next) {
      // 关闭直接生效，不确认
      setConfirmAutoApprove(false);
      setSavingAutoApprove(true);
      try {
        await saveAutoApprove(false);
      } finally {
        setSavingAutoApprove(false);
      }
      return;
    }
    // 开启需确认一次
    setConfirmAutoApprove(true);
  };

  const handleConfirmAutoApprove = async () => {
    setSavingAutoApprove(true);
    try {
      const ok = await saveAutoApprove(true);
      if (ok) setConfirmAutoApprove(false);
    } finally {
      setSavingAutoApprove(false);
    }
  };

  useEffect(() => {
    if (open) {
      loadSettings().then(() => {
        setUrlDraft(useSettingsStore.getState().artifactoryBaseUrl);
      });
    }
  }, [open, loadSettings]);

  const handleSaveUrl = async () => {
    const trimmed = urlDraft.trim();
    if (!trimmed || savingUrl) return;
    setSavingUrl(true);
    try {
      await saveBaseUrl(trimmed);
    } finally {
      setSavingUrl(false);
    }
  };

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

        {/* Artifactory base URL (JDK provisioning) */}
        <div className="border-t border-border shrink-0">
          <div className="px-5 py-3 space-y-2">
            <label htmlFor="artifactory-url" className="text-sm text-foreground">
              Artifactory 仓库地址
            </label>
            <p className="text-xs text-muted-foreground">
              用于 ensure_tool 下载 JDK 诊断工具包到目标环境（/tmp/friday-tools）
            </p>
            <div className="flex items-center gap-2">
              <input
                id="artifactory-url"
                type="text"
                value={urlDraft}
                onChange={(e) => setUrlDraft(e.target.value)}
                placeholder="https://…/artifactory/cmc-software-release"
                className="flex-1 bg-muted border border-border rounded-md text-sm text-foreground px-3 py-1.5 placeholder:text-muted-foreground/50 outline-none"
                style={{ fontFamily: "var(--font-mono)" }}
              />
              <button
                onClick={handleSaveUrl}
                disabled={savingUrl || urlDraft.trim() === artifactoryBaseUrl}
                className="px-3 py-1.5 rounded-md bg-accent text-accent-foreground text-xs hover:bg-accent/80 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed shrink-0"
              >
                {savingUrl ? "保存中..." : "保存"}
              </button>
            </div>
            {settingsError && (
              <p className="text-xs text-destructive break-words">{settingsError}</p>
            )}
          </div>
        </div>

        {/* Auto-approve tools */}
        <div className="border-t border-border shrink-0">
          <div className="px-5 py-3 space-y-2">
            <label className="flex items-center gap-2 text-sm text-foreground cursor-pointer">
              <input
                type="checkbox"
                checked={autoApprove || confirmAutoApprove}
                onChange={(e) => handleToggleAutoApprove(e.target.checked)}
                disabled={savingAutoApprove}
              />
              免确认模式
            </label>
            <p className="text-xs text-muted-foreground">
              开启后所有工具调用免确认直接执行（含高风险：任意命令、堆 dump、文件上传），仅建议内网非生产环境开启
            </p>
            {confirmAutoApprove && (
              <div className="rounded-md border border-warning/60 bg-warning/5 px-3 py-2 space-y-2">
                <p className="text-xs text-warning">
                  开启后 agent 执行任何操作都不再需要你确认，包括 run_command、heap_dump、file_upload
                  等高风险操作。确定开启？
                </p>
                <div className="flex items-center gap-2">
                  <button
                    onClick={handleConfirmAutoApprove}
                    disabled={savingAutoApprove}
                    className="px-3 py-1 rounded-md bg-warning text-warning-foreground text-xs hover:bg-warning/80 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {savingAutoApprove ? "开启中..." : "确认开启"}
                  </button>
                  <button
                    onClick={() => setConfirmAutoApprove(false)}
                    disabled={savingAutoApprove}
                    className="px-3 py-1 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    取消
                  </button>
                </div>
              </div>
            )}
            {settingsError && (
              <p className="text-xs text-destructive break-words">{settingsError}</p>
            )}
          </div>
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
