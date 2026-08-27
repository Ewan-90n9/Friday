import { useState } from "react";
import { CaretRight, CaretDown } from "@phosphor-icons/react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ChatMessage } from "@/lib/types";
import { ToolCallCard } from "./ToolCallCard";
import { ConfirmCard } from "./ConfirmCard";

interface AgentMessageProps {
  message: ChatMessage;
}

export function AgentMessage({ message }: AgentMessageProps) {
  const [reasoningExpanded, setReasoningExpanded] = useState(true);

  const reasoningParts = message.parts.filter((p) => p.type === "reasoning");
  const textParts = message.parts.filter((p) => p.type === "text");
  const toolParts = message.parts.filter((p) => p.type === "tool");
  const confirmParts = message.parts.filter((p) => p.type === "confirm");

  const isStreaming = message.status === "streaming";

  return (
    <div className="mb-5 max-w-[85%]">
      <div className="flex items-center gap-1.5 mb-2">
        <span
          className="text-xs text-muted-foreground"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          ● Friday
        </span>
      </div>

      {reasoningParts.length > 0 && (
        <div className="bg-surface-1 border border-border rounded-lg mb-3 overflow-hidden">
          <button
            onClick={() => setReasoningExpanded(!reasoningExpanded)}
            className="flex items-center gap-1 px-2.5 py-1.5 text-xs text-muted-foreground hover:bg-surface-2 transition-colors w-full text-left"
          >
            {reasoningExpanded ? (
              <CaretDown size={10} weight="bold" aria-hidden="true" />
            ) : (
              <CaretRight size={10} weight="bold" aria-hidden="true" />
            )}
            推理过程
          </button>
          {reasoningExpanded && (
            <div
              className="px-3 py-2 text-xs leading-5 text-muted-foreground border-t border-border"
              style={{ fontFamily: "var(--font-mono)" }}
            >
              {reasoningParts.map((p, i) => (
                <span key={i}>{p.text}</span>
              ))}
            </div>
          )}
        </div>
      )}

      {message.parts.map((part, i) => {
        if (part.type === "tool" && part.tool) {
          return <ToolCallCard key={i} tool={part.tool} />;
        }
        if (part.type === "confirm" && part.confirm) {
          return <ConfirmCard key={i} request={part.confirm} />;
        }
        return null;
      })}

      {textParts.length > 0 ? (
        <div className="markdown-body mb-3">
          <ReactMarkdown remarkPlugins={[remarkGfm]}>
            {textParts.map((p) => p.text ?? "").join("")}
          </ReactMarkdown>
          {isStreaming && (
            <span
              className="inline-block w-[7px] h-[15px] bg-accent ml-0.5 align-text-bottom animate-pulse"
              aria-hidden="true"
            />
          )}
        </div>
      ) : isStreaming && toolParts.length === 0 && confirmParts.length === 0 ? (
        <div
          className="text-sm text-muted-foreground mb-3"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          思考中
          <span
            className="inline-block w-[7px] h-[15px] bg-accent ml-0.5 align-text-bottom animate-pulse"
            aria-hidden="true"
          />
        </div>
      ) : null}

      {!isStreaming && (
        <div
          className="text-xs text-muted-foreground"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {message.status === "done" && "✓ 完成"}
          {message.status === "stopped" && "■ 已停止"}
          {message.status === "error" && "✕ 出错"}
        </div>
      )}
    </div>
  );
}
