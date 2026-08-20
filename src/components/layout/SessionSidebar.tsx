export function SessionSidebar() {
  return (
    <aside className="w-60 shrink-0 border-r border-border bg-card flex flex-col">
      <div className="flex-1 overflow-y-auto p-2">
        <p className="text-muted-foreground text-xs px-2 py-4">暂无会话</p>
      </div>
      <div className="p-2 border-t border-border">
        <button
          className="w-full text-sm text-foreground bg-secondary hover:bg-secondary-foreground/20 rounded-md px-3 py-2 transition-colors"
          disabled
        >
          + 新会话
        </button>
      </div>
    </aside>
  );
}
