import { GearSix } from "@phosphor-icons/react";
import { FridayMark } from "@/components/FridayMark";

export function TopBar() {
  return (
    <header
      className="flex items-center justify-between h-12 px-4 shrink-0 border-b border-border bg-surface-1"
      style={{ backgroundImage: "linear-gradient(180deg, var(--color-surface-2) 0%, var(--color-surface-1) 100%)" }}
    >
      {/* 左侧：品牌 + 会话标题 */}
      <div className="flex items-center gap-3 min-w-0">
        <div className="flex items-center gap-2">
          <FridayMark size={22} />
          <span
            className="text-foreground text-sm font-semibold tracking-wide"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            Friday
          </span>
        </div>
      </div>

      {/* 右侧：状态 + 设置 */}
      <div className="flex items-center gap-1">
        <div className="flex items-center gap-2 px-2.5 py-1 rounded-md bg-muted/50">
          <span className="relative flex w-1.5 h-1.5">
            <span className="absolute inline-flex w-full h-full rounded-full bg-muted-foreground opacity-60" />
            <span className="relative inline-flex w-1.5 h-1.5 rounded-full bg-muted-foreground" />
          </span>
          <span className="text-muted-foreground text-xs">待机</span>
        </div>
        <button
          className="flex items-center justify-center w-8 h-8 rounded-md text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          aria-label="设置"
        >
          <GearSix size={18} weight="regular" />
        </button>
      </div>
    </header>
  );
}
