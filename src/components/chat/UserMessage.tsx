interface UserMessageProps {
  content: string;
}

export function UserMessage({ content }: UserMessageProps) {
  return (
    <div className="flex justify-end mb-5">
      <div
        className="max-w-[70%] bg-surface-2 border border-border rounded-xl rounded-br-sm px-3.5 py-2.5 text-sm leading-5 text-foreground"
        style={{ fontFamily: "var(--font-sans)" }}
      >
        {content}
      </div>
    </div>
  );
}
