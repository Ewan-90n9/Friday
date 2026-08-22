import { useState } from "react";
import { ChatCircle, Plus, Archive, Trash, ArrowUUpLeft } from "@phosphor-icons/react";
import { useSessionStore } from "@/store/sessionStore";
import { DeleteConfirmDialog } from "@/components/chat/DeleteConfirmDialog";

export function SessionSidebar() {
  const sessions = useSessionStore((s) => s.sessions);
  const archivedSessions = useSessionStore((s) => s.archivedSessions);
  const currentSessionId = useSessionStore((s) => s.currentSessionId);
  const agentRunning = useSessionStore((s) => s.agentRunning);
  const sidebarView = useSessionStore((s) => s.sidebarView);
  const selectSession = useSessionStore((s) => s.selectSession);
  const newSession = useSessionStore((s) => s.newSession);
  const setSidebarView = useSessionStore((s) => s.setSidebarView);
  const archiveSession = useSessionStore((s) => s.archiveSession);
  const unarchiveSession = useSessionStore((s) => s.unarchiveSession);
  const deleteSession = useSessionStore((s) => s.deleteSession);

  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);

  const handleDeleteConfirm = async () => {
    if (!deleteTarget) return;
    await deleteSession(deleteTarget);
    setDeleteTarget(null);
  };

  const isArchiveView = sidebarView === "archived";
  const displaySessions = isArchiveView ? archivedSessions : sessions;

  const renderSessionItem = (s: { id: string; title: string | null; status: string; created_at: string; archived_at?: string | null }) => {
    const isActive = s.id === currentSessionId;
    const isRunning = agentRunning[s.id] ?? false;
    const isClosed = s.status === "closed";
    const isArchived = s.status === "archived";
    const dimmed = isClosed || isArchived;

    return (
      <div
        key={s.id}
        className={`group relative w-full text-left px-3 py-2 rounded-lg mb-0.5 transition-colors ${
          isActive
            ? "bg-surface-2 border-l-2 border-success pl-[10px]"
            : "hover:bg-surface-2"
        } ${dimmed ? "opacity-60" : ""}`}
      >
        <button
          type="button"
          onClick={() => selectSession(s.id)}
          className="flex items-center gap-1.5 mb-0.5 w-full text-left"
        >
          <span
            className={`w-1.5 h-1.5 rounded-full shrink-0 ${
              isRunning ? "bg-success animate-pulse" : "bg-muted-foreground"
            }`}
            aria-hidden="true"
          />
          <span className="text-sm font-medium text-foreground truncate flex-1">
            {s.title || "无标题会话"}
          </span>
        </button>

        <div className="flex items-center justify-between">
          <span
            className="text-xs text-muted-foreground"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            {isArchived && s.archived_at
              ? `归档于 ${s.archived_at.slice(0, 10)}`
              : s.created_at.slice(0, 10)}
          </span>

          <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
            {isArchiveView ? (
              <button
                type="button"
                onClick={(e) => { e.stopPropagation(); unarchiveSession(s.id); }}
                className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors"
                aria-label="取消归档"
              >
                <ArrowUUpLeft size={14} weight="regular" aria-hidden="true" />
              </button>
            ) : (
              <button
                type="button"
                onClick={(e) => { e.stopPropagation(); archiveSession(s.id); }}
                className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors"
                aria-label="归档会话"
              >
                <Archive size={14} weight="regular" aria-hidden="true" />
              </button>
            )}
            <button
              type="button"
              onClick={(e) => { e.stopPropagation(); setDeleteTarget(s.id); }}
              className="p-1 rounded text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-colors"
              aria-label="删除会话"
            >
              <Trash size={14} weight="regular" aria-hidden="true" />
            </button>
          </div>
        </div>
      </div>
    );
  };

  return (
    <aside className="w-60 shrink-0 border-r border-border bg-surface-1 flex flex-col">
      {/* Toggle bar */}
      <div className="flex border-b border-border h-10 shrink-0">
        <button
          onClick={() => setSidebarView("sessions")}
          className={`flex-1 flex items-center justify-center text-xs font-medium transition-colors ${
            !isArchiveView
              ? "text-foreground border-b-2 border-success"
              : "text-muted-foreground hover:text-foreground"
          }`}
        >
          会话
        </button>
        <button
          onClick={() => setSidebarView("archived")}
          className={`flex-1 flex items-center justify-center text-xs font-medium transition-colors ${
            isArchiveView
              ? "text-foreground border-b-2 border-success"
              : "text-muted-foreground hover:text-foreground"
          }`}
        >
          归档
        </button>
      </div>

      {/* Session list */}
      <div className="flex-1 overflow-y-auto flex flex-col">
        {isArchiveView && displaySessions.length > 0 && (
          <div className="text-xs text-muted-foreground uppercase tracking-wide px-4 py-2">
            {displaySessions.length} 个已归档会话
          </div>
        )}

        {displaySessions.length === 0 ? (
          <div className="flex-1 flex flex-col items-center justify-center px-6 py-8 select-none">
            <div className="flex items-center justify-center w-12 h-12 rounded-xl bg-muted/40 border border-border mb-3">
              {isArchiveView ? (
                <Archive size={24} weight="regular" className="text-muted-foreground" aria-hidden="true" />
              ) : (
                <ChatCircle size={24} weight="regular" className="text-muted-foreground" aria-hidden="true" />
              )}
            </div>
            <p className="text-muted-foreground text-xs text-center leading-relaxed">
              {isArchiveView ? "暂无归档会话" : "暂无诊断会话"}
            </p>
            {!isArchiveView && (
              <p className="text-muted-foreground/60 text-xs text-center mt-1">
                在下方输入框描述问题开始
              </p>
            )}
          </div>
        ) : (
          <div className="px-2">
            {displaySessions.map(renderSessionItem)}
          </div>
        )}
      </div>

      {/* New session button — only in main view */}
      {!isArchiveView && (
        <div className="p-3 border-t border-border">
          <button
            onClick={newSession}
            className="w-full flex items-center justify-center gap-2 text-sm text-muted-foreground bg-surface-2 hover:bg-surface-3 hover:text-foreground rounded-lg px-3 py-2 transition-colors cursor-pointer border border-border"
          >
            <Plus size={16} weight="regular" aria-hidden="true" />
            新建会话
          </button>
        </div>
      )}

      {/* Delete confirmation dialog */}
      <DeleteConfirmDialog
        open={deleteTarget !== null}
        onCancel={() => setDeleteTarget(null)}
        onConfirm={handleDeleteConfirm}
      />
    </aside>
  );
}
