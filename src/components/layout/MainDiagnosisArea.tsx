import { useState, useRef } from "react";

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

  return (
    <main className="flex-1 flex flex-col min-w-0">
      {/* 诊断区 */}
      <div className="flex-1 overflow-y-auto p-4">
        <p className="text-muted-foreground text-sm">输入问题开始诊断...</p>
      </div>

      {/* 输入框 */}
      <div className="border-t border-border p-4 bg-card">
        <textarea
          ref={textareaRef}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="描述环境、服务和症状..."
          rows={2}
          className="w-full bg-muted text-foreground text-sm rounded-md px-3 py-2 resize-none border border-border focus:outline-none focus:ring-1 focus:ring-ring placeholder:text-muted-foreground"
          style={{ fontFamily: "'IBM Plex Sans', sans-serif" }}
        />
        <p className="text-muted-foreground text-xs mt-1">
          Shift+Enter 换行，Enter 发送
        </p>
      </div>
    </main>
  );
}
