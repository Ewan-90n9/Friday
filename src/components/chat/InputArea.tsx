import { useRef } from "react";
import { PaperPlaneTilt, Stop } from "@phosphor-icons/react";
import { useSessionStore } from "@/store/sessionStore";

export function InputArea() {
  const inputText = useSessionStore((s) => s.inputText);
  const setInputText = useSessionStore((s) => s.setInputText);
  const sendMessage = useSessionStore((s) => s.sendMessage);
  const stopAgent = useSessionStore((s) => s.stopAgent);
  const currentSessionId = useSessionStore((s) => s.currentSessionId);
  const agentRunning = useSessionStore((s) => s.agentRunning);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const isRunning = currentSessionId ? agentRunning[currentSessionId] ?? false : false;
  const hasContent = inputText.trim().length > 0;

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (hasContent && !isRunning) {
        sendMessage();
      }
    }
  };

  const placeholder = isRunning
    ? "补充信息...  Enter 发送 · Shift+Enter 换行"
    : "描述环境、服务和症状…  Enter 发送 · Shift+Enter 换行";

  return (
    <div className="shrink-0 px-6 pb-4 pt-2 border-t border-border bg-background">
      <div
        className="rounded-xl border border-border bg-surface-1 transition-colors focus-within:border-accent/40"
        style={{
          boxShadow: "0 1px 3px rgba(0, 0, 0, 0.3)",
        }}
      >
        <textarea
          ref={textareaRef}
          value={inputText}
          onChange={(e) => setInputText(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={placeholder}
          rows={2}
          className="w-full bg-transparent text-foreground text-sm rounded-xl px-4 py-3 resize-none outline-none placeholder:text-muted-foreground/50"
          style={{ fontFamily: "var(--font-sans)" }}
        />
        <div className="flex items-center justify-between px-3 pb-2.5">
          <span className="text-muted-foreground/60 text-xs">
            {isRunning ? "Agent 运行中，输入可补充信息" : "Enter 发送 · Shift+Enter 换行"}
          </span>
          <div className="flex items-center gap-2">
            {isRunning && (
              <button
                onClick={stopAgent}
                className="flex items-center gap-1.5 px-2.5 py-1 bg-destructive/10 border border-destructive/20 rounded-md text-destructive text-xs hover:bg-destructive/20 transition-colors"
                style={{ fontFamily: "var(--font-mono)" }}
              >
                <Stop size={10} weight="fill" aria-hidden="true" />
                停止
              </button>
            )}
            <button
              onClick={() => hasContent && !isRunning && sendMessage()}
              className={`flex items-center justify-center w-7 h-7 rounded-md transition-all ${
                hasContent && !isRunning
                  ? "bg-accent text-white hover:bg-accent/80 cursor-pointer"
                  : "bg-muted text-muted-foreground cursor-not-allowed"
              }`}
              disabled={!hasContent || isRunning}
              aria-label="发送"
            >
              <PaperPlaneTilt size={14} weight="fill" aria-hidden="true" />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
