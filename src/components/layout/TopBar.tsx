export function TopBar() {
  return (
    <header
      className="flex items-center justify-between h-12 px-4 border-b border-border bg-primary shrink-0"
    >
      <div className="flex items-center gap-2">
        <span className="text-lg font-bold text-foreground" style={{ fontFamily: "'JetBrains Mono', monospace" }}>
          Friday
        </span>
        <span className="text-muted-foreground text-sm">会话标题</span>
      </div>
      <div className="flex items-center gap-2">
        <span className="w-2 h-2 rounded-full bg-muted-foreground" />
        <span className="text-muted-foreground text-xs">已停止</span>
      </div>
    </header>
  );
}
