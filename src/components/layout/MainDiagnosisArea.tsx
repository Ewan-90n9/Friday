import { useState, useRef } from "react";
import { Crosshair, PaperPlaneTilt, ArrowRight } from "@phosphor-icons/react";

const EXAMPLE_PROMPT = "10.0.1.23 生产环境 OOMService 频繁 OOM，帮我定位根因";

export function MainDiagnosisArea() {
  const [input, setInput] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      // 骨架阶段不发送
      setInput("");
    }
  };

  const handleExampleClick = () => {
    setInput(EXAMPLE_PROMPT);
    textareaRef.current?.focus();
  };

  const hasContent = input.trim().length > 0;

  return (
    <main className="flex-1 flex flex-col min-w-0 bg-background">
      {/* 诊断区 */}
      <div className="flex-1 overflow-y-auto">
        {hasContent ? null : (
          <EmptyState onExampleClick={handleExampleClick} />
        )}
      </div>

      {/* 输入区 */}
      <div className="shrink-0 px-6 pb-4 pt-2">
        <div
          className="rounded-xl border border-border bg-surface-1 transition-colors focus-within:border-accent/40"
          style={{
            boxShadow: "0 1px 3px rgba(0, 0, 0, 0.3), 0 0 0 0 var(--color-accent-glow)",
          }}
        >
          <textarea
            ref={textareaRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="描述环境、服务和症状…  例如：10.0.1.23 生产环境 OOMService 频繁 OOM"
            rows={2}
            className="w-full bg-transparent text-foreground text-sm rounded-xl px-4 py-3 resize-none outline-none placeholder:text-muted-foreground/50"
            style={{ fontFamily: "var(--font-sans)" }}
          />
          <div className="flex items-center justify-between px-3 pb-2.5">
            <span className="text-muted-foreground/60 text-xs">
              Enter 发送 · Shift+Enter 换行
            </span>
            <button
              className={`flex items-center justify-center w-7 h-7 rounded-md transition-all cursor-pointer ${
                hasContent
                  ? "bg-accent text-white hover:bg-accent/80"
                  : "bg-muted text-muted-foreground cursor-not-allowed"
              }`}
              disabled={!hasContent}
              aria-label="发送"
            >
              <PaperPlaneTilt size={14} weight="fill" aria-hidden="true" />
            </button>
          </div>
        </div>
      </div>
    </main>
  );
}

function EmptyState({ onExampleClick }: { onExampleClick: () => void }) {
  return (
    <div className="h-full flex flex-col items-center justify-center px-8 select-none">
      {/* 图标 */}
      <div className="relative mb-6">
        <div
          className="flex items-center justify-center w-16 h-16 rounded-2xl border border-border bg-surface-1"
          style={{
            backgroundImage:
              "linear-gradient(135deg, var(--color-surface-2) 0%, var(--color-surface-1) 100%)",
          }}
        >
          <Crosshair
            size={30}
            weight="regular"
            className="text-muted-foreground"
            aria-hidden="true"
          />
        </div>
      </div>

      {/* 标题 */}
      <h2
        className="text-foreground text-lg font-medium mb-2"
        style={{ fontFamily: "var(--font-mono)" }}
      >
        开始诊断
      </h2>

      {/* 说明 */}
      <p className="text-muted-foreground text-sm text-center max-w-sm leading-relaxed mb-8">
        描述目标环境、服务和故障症状，Friday 将自动连接环境并定位根因
      </p>

      {/* 示例 */}
      <button
        onClick={onExampleClick}
        className="group flex items-center gap-3 px-4 py-2.5 rounded-lg border border-border bg-surface-1 hover:bg-surface-2 hover:border-border-strong transition-all cursor-pointer max-w-lg w-full"
      >
        <span
          className="text-muted-foreground text-xs shrink-0"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          示例
        </span>
        <span className="text-muted-foreground text-sm text-left flex-1 truncate group-hover:text-foreground transition-colors">
          {EXAMPLE_PROMPT}
        </span>
        <ArrowRight
          size={14}
          weight="regular"
          className="text-muted-foreground/50 group-hover:text-muted-foreground shrink-0 transition-colors"
          aria-hidden="true"
        />
      </button>
    </div>
  );
}
