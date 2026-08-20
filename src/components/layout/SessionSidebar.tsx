import { ChatCircle, Plus } from "@phosphor-icons/react";

export function SessionSidebar() {
  return (
    <aside className="w-60 shrink-0 border-r border-border bg-surface-1 flex flex-col">
      {/* 会话列表区 */}
      <div className="flex-1 overflow-y-auto flex flex-col">
        <div className="flex items-center justify-between px-4 h-9 shrink-0">
          <span className="text-muted-foreground text-xs font-medium tracking-wide uppercase">
            会话
          </span>
        </div>

        {/* 空状态 */}
        <div className="flex-1 flex flex-col items-center justify-center px-6 py-8 select-none">
          <div className="flex items-center justify-center w-12 h-12 rounded-xl bg-muted/40 border border-border mb-3">
            <ChatCircle
              size={24}
              weight="regular"
              className="text-muted-foreground"
              aria-hidden="true"
            />
          </div>
          <p className="text-muted-foreground text-xs text-center leading-relaxed">
            暂无诊断会话
          </p>
          <p className="text-muted-foreground/60 text-xs text-center mt-1">
            在下方输入框描述问题开始
          </p>
        </div>
      </div>

      {/* 底部：新建会话 */}
      <div className="p-3 border-t border-border">
        <button
          className="w-full flex items-center justify-center gap-2 text-sm text-muted-foreground bg-surface-2 hover:bg-surface-3 hover:text-foreground rounded-lg px-3 py-2 transition-colors cursor-pointer border border-border"
          disabled
        >
          <Plus size={16} weight="regular" aria-hidden="true" />
          新建会话
        </button>
      </div>
    </aside>
  );
}
