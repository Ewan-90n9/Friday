import { Crosshair, ArrowRight } from "@phosphor-icons/react";
import { useSessionStore } from "@/store/sessionStore";
import { MessageList } from "@/components/chat/MessageList";
import { InputArea } from "@/components/chat/InputArea";

const EXAMPLE_PROMPT = "10.0.1.23 生产环境 OOMService 频繁 OOM，帮我定位根因";

export function MainDiagnosisArea() {
  const currentSessionId = useSessionStore((s) => s.currentSessionId);
  const messagesBySession = useSessionStore((s) => s.messagesBySession);
  const setInputText = useSessionStore((s) => s.setInputText);

  const messages = currentSessionId ? messagesBySession[currentSessionId] ?? [] : [];

  const handleExampleClick = () => {
    setInputText(EXAMPLE_PROMPT);
  };

  return (
    <main className="flex-1 flex flex-col min-w-0 bg-background">
      {messages.length > 0 ? (
        <MessageList messages={messages} />
      ) : (
        <div className="flex-1 overflow-y-auto">
          <EmptyState onExampleClick={handleExampleClick} />
        </div>
      )}
      <InputArea />
    </main>
  );
}

function EmptyState({ onExampleClick }: { onExampleClick: () => void }) {
  return (
    <div className="h-full flex flex-col items-center justify-center px-8 select-none">
      <div className="relative mb-6">
        <div
          className="flex items-center justify-center w-16 h-16 rounded-2xl border border-border bg-surface-1"
          style={{
            backgroundImage:
              "linear-gradient(135deg, var(--color-surface-2) 0%, var(--color-surface-1) 100%)",
          }}
        >
          <Crosshair size={30} weight="regular" className="text-muted-foreground" aria-hidden="true" />
        </div>
      </div>

      <h2
        className="text-foreground text-lg font-medium mb-2"
        style={{ fontFamily: "var(--font-mono)" }}
      >
        开始诊断
      </h2>

      <p className="text-muted-foreground text-sm text-center max-w-sm leading-relaxed mb-8">
        描述目标环境、服务和故障症状，Friday 将自动连接环境并定位根因
      </p>

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
