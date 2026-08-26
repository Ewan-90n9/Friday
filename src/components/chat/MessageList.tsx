import { useEffect, useRef } from "react";
import type { ChatMessage } from "@/lib/types";
import { useSessionStore } from "@/store/sessionStore";
import { UserMessage } from "./UserMessage";
import { AgentMessage } from "./AgentMessage";
import { ConfirmCard } from "./ConfirmCard";

interface MessageListProps {
  messages: ChatMessage[];
}

export function MessageList({ messages }: MessageListProps) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const isAtBottomRef = useRef(true);
  const currentSessionId = useSessionStore((s) => s.currentSessionId);
  const pendingConfirms = useSessionStore((s) => s.pendingConfirms);
  const confirms = currentSessionId ? (pendingConfirms[currentSessionId] ?? []) : [];

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const handleScroll = () => {
      const { scrollTop, scrollHeight, clientHeight } = container;
      isAtBottomRef.current = scrollHeight - scrollTop - clientHeight < 50;
    };

    container.addEventListener("scroll", handleScroll);
    return () => container.removeEventListener("scroll", handleScroll);
  }, []);

  useEffect(() => {
    if (isAtBottomRef.current) {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
  }, [messages]);

  if (messages.length === 0) {
    return null;
  }

  return (
    <div ref={containerRef} className="flex-1 overflow-y-auto px-6 py-4">
      {messages.map((msg) =>
        msg.role === "user" ? (
          <UserMessage key={msg.id} content={msg.content} />
        ) : (
          <AgentMessage key={msg.id} message={msg} />
        ),
      )}
      {confirms.map((c) => (
        <ConfirmCard key={c.confirm_id} request={c} />
      ))}
      <div ref={bottomRef} />
    </div>
  );
}
