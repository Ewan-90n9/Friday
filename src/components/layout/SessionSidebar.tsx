import { ChatCircle, Plus } from "@phosphor-icons/react";
import { useSessionStore } from "@/store/sessionStore";

export function SessionSidebar() {
  const sessions = useSessionStore((s) => s.sessions);
  const currentSessionId = useSessionStore((s) => s.currentSessionId);
  const agentRunning = useSessionStore((s) => s.agentRunning);
  const selectSession = useSessionStore((s) => s.selectSession);
  const newSession = useSessionStore((s) => s.newSession);

  return (
    <aside className="w-60 shrink-0 border-r border-border bg-surface-1 flex flex-col">
      <div className="flex-1 overflow-y-auto flex flex-col">
        <div className="flex items-center justify-between px-4 h-9 shrink-0">
          <span className="text-muted-foreground text-xs font-medium tracking-wide uppercase">
            会话
          </span>
        </div>

        {sessions.length === 0 ? (
          <div className="flex-1 flex flex-col items-center justify-center px-6 py-8 select-none">
            <div className="flex items-center justify-center w-12 h-12 rounded-xl bg-muted/40 border border-border mb-3">
              <ChatCircle size={24} weight="regular" className="text-muted-foreground" aria-hidden="true" />
            </div>
            <p className="text-muted-foreground text-xs text-center leading-relaxed">
              暂无诊断会话
            </p>
            <p className="text-muted-foreground/60 text-xs text-center mt-1">
              在下方输入框描述问题开始
            </p>
          </div>
        ) : (
          <div className="px-2">
            {sessions.map((s) => {
              const isActive = s.id === currentSessionId;
              const isRunning = agentRunning[s.id] ?? false;
              return (
                <button
                  key={s.id}
                  onClick={() => selectSession(s.id)}
                  className={`w-full text-left px-3 py-2 rounded-lg mb-0.5 transition-colors cursor-pointer ${
                    isActive
                      ? "bg-surface-2 border-l-2 border-success pl-[10px]"
                      : "hover:bg-surface-2"
                  }`}
                >
                  <div className="flex items-center gap-1.5 mb-0.5">
                    <span
                      className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                        isRunning ? "bg-success animate-pulse" : "bg-muted-foreground"
                      }`}
                      aria-hidden="true"
                    />
                    <span className="text-sm font-medium text-foreground truncate flex-1">
                      {s.title || "无标题会话"}
                    </span>
                  </div>
                  <span
                    className="text-xs text-muted-foreground"
                    style={{ fontFamily: "var(--font-mono)" }}
                  >
                    {s.created_at.slice(0, 10)}
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </div>

      <div className="p-3 border-t border-border">
        <button
          onClick={newSession}
          className="w-full flex items-center justify-center gap-2 text-sm text-muted-foreground bg-surface-2 hover:bg-surface-3 hover:text-foreground rounded-lg px-3 py-2 transition-colors cursor-pointer border border-border"
        >
          <Plus size={16} weight="regular" aria-hidden="true" />
          新建会话
        </button>
      </div>
    </aside>
  );
}
