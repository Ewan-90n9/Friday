import { create } from "zustand";
import { sendMessage as ipcSendMessage, stopAgent, listSessions, onAppEvent, getSessionMessages, archiveSession as ipcArchiveSession, unarchiveSession as ipcUnarchiveSession, deleteSession as ipcDeleteSession, confirmTool } from "@/lib/ipc";
import type { SessionRow, ChatMessage, ChatPart, AppEvent, MessageRow, TransferInfo } from "@/lib/types";

interface SessionStore {
  sessions: SessionRow[];
  archivedSessions: SessionRow[];
  currentSessionId: string | null;
  messagesBySession: Record<string, ChatMessage[]>;
  agentRunning: Record<string, boolean>;
  inputText: string;
  sidebarView: "sessions" | "archived";
  eventUnlisten: (() => void) | null | string;

  loadSessions: () => Promise<void>;
  loadArchivedSessions: () => Promise<void>;
  selectSession: (id: string) => Promise<void>;
  newSession: () => void;
  setInputText: (text: string) => void;
  sendMessage: () => Promise<void>;
  stopAgent: () => Promise<void>;
  initEventListener: () => Promise<void>;
  handleEvent: (payload: { session_id: string; event: AppEvent }) => void;
  setSidebarView: (view: "sessions" | "archived") => void;
  archiveSession: (id: string) => Promise<void>;
  unarchiveSession: (id: string) => Promise<void>;
  deleteSession: (id: string) => Promise<void>;
  confirmToolAction: (confirmId: string, approved: boolean) => Promise<void>;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

let agentMessageCounter = 0;

function convertMessages(rows: MessageRow[]): ChatMessage[] {
  return rows.map((row) => {
    const parts: ChatPart[] = row.parts.map((p) => {
      if (p.part_type === "text") {
        return { type: "text", text: p.text ?? "" };
      } else {
        let args: unknown;
        try {
          args = p.tool_args ? JSON.parse(p.tool_args) : null;
        } catch {
          args = p.tool_args;
        }
        return {
          type: "tool",
          tool: {
            name: p.tool_name ?? "unknown",
            args,
            status: (p.tool_status as "running" | "completed" | "error") ?? "completed",
            output: p.tool_output ?? undefined,
            elapsedMs: p.tool_elapsed_ms ?? undefined,
          },
        };
      }
    });

    return {
      id: row.id,
      role: row.role as "user" | "agent",
      content: row.content ?? "",
      parts,
      status: (row.status as ChatMessage["status"]) ?? "done",
    };
  });
}

export const useSessionStore = create<SessionStore>((set, get) => ({
  sessions: [],
  archivedSessions: [],
  currentSessionId: null,
  messagesBySession: {},
  agentRunning: {},
  inputText: "",
  sidebarView: "sessions",
  eventUnlisten: null,

  loadSessions: async () => {
    try {
      const sessions = await listSessions(false);
      set({ sessions });
    } catch (e) {
      console.error("Failed to load sessions:", errMsg(e));
    }
  },

  loadArchivedSessions: async () => {
    try {
      const archivedSessions = await listSessions(true);
      set({ archivedSessions });
    } catch (e) {
      console.error("Failed to load archived sessions:", errMsg(e));
    }
  },

  setSidebarView: (view) => {
    if (view === "archived") {
      get().loadArchivedSessions();
    }
    set({ sidebarView: view });
  },

  selectSession: async (id) => {
    set({ currentSessionId: id });
    const { messagesBySession } = get();
    if (!messagesBySession[id]) {
      try {
        const rows = await getSessionMessages(id);
        const messages = convertMessages(rows);
        set((state) => ({
          messagesBySession: { ...state.messagesBySession, [id]: messages },
        }));
      } catch (e) {
        console.error("Failed to load session messages:", errMsg(e));
      }
    }
  },

  newSession: () => set({ currentSessionId: null, inputText: "" }),

  setInputText: (text) => set({ inputText: text }),

  sendMessage: async () => {
    const { inputText, currentSessionId, agentRunning } = get();
    const trimmed = inputText.trim();
    if (!trimmed) return;
    if (currentSessionId && agentRunning[currentSessionId]) return;

    set({ inputText: "" });

    try {
      const sessionId = await ipcSendMessage(currentSessionId, trimmed);

      const userMsg: ChatMessage = {
        id: `user-${Date.now()}`,
        role: "user",
        content: trimmed,
        parts: [],
        status: "done",
      };

      set((state) => {
        const existing = state.messagesBySession[sessionId] ?? [];
        // Check if this specific user message is already in the list
        // (race condition: events may arrive before invoke resolves)
        const hasThisUserMsg = existing.some(
          (m) => m.role === "user" && m.content === trimmed,
        );
        if (hasThisUserMsg) {
          return { currentSessionId: sessionId };
        }
        // Insert user message before any streaming agent message
        let insertIdx = existing.length;
        for (let i = existing.length - 1; i >= 0; i--) {
          if (existing[i].role === "agent" && existing[i].status === "streaming") {
            insertIdx = i;
          } else if (existing[i].role === "agent") {
            insertIdx = i + 1;
            break;
          }
        }
        const messages = [
          ...existing.slice(0, insertIdx),
          userMsg,
          ...existing.slice(insertIdx),
        ];
        return {
          currentSessionId: sessionId,
          messagesBySession: { ...state.messagesBySession, [sessionId]: messages },
        };
      });

      await get().loadSessions();
    } catch (e) {
      console.error("Failed to send message:", errMsg(e));
      set({ inputText: trimmed });
    }
  },

  stopAgent: async () => {
    const { currentSessionId } = get();
    if (!currentSessionId) return;
    try {
      await stopAgent(currentSessionId);
    } catch (e) {
      console.error("Failed to stop agent:", errMsg(e));
    }
  },

  archiveSession: async (id) => {
    try {
      await ipcArchiveSession(id);
      const { sessions } = get();
      const archived = sessions.find((s) => s.id === id);
      if (archived) {
        set((state) => ({
          sessions: state.sessions.filter((s) => s.id !== id),
          archivedSessions: [...state.archivedSessions, { ...archived, status: "archived" as const, archived_at: new Date().toISOString() }],
        }));
      }
    } catch (e) {
      console.error("Failed to archive session:", errMsg(e));
    }
  },

  unarchiveSession: async (id) => {
    try {
      await ipcUnarchiveSession(id);
      const { archivedSessions } = get();
      const restored = archivedSessions.find((s) => s.id === id);
      set((state) => ({
        archivedSessions: state.archivedSessions.filter((s) => s.id !== id),
        sessions: restored
          ? [...state.sessions, { ...restored, status: "closed" as const, archived_at: null }]
          : state.sessions,
      }));
    } catch (e) {
      console.error("Failed to unarchive session:", errMsg(e));
    }
  },

  deleteSession: async (id) => {
    try {
      await ipcDeleteSession(id);
      const { messagesBySession, currentSessionId } = get();
      const newMessages = { ...messagesBySession };
      delete newMessages[id];
      set((state) => ({
        sessions: state.sessions.filter((s) => s.id !== id),
        archivedSessions: state.archivedSessions.filter((s) => s.id !== id),
        messagesBySession: newMessages,
        currentSessionId: currentSessionId === id ? null : currentSessionId,
      }));
    } catch (e) {
      console.error("Failed to delete session:", errMsg(e));
    }
  },

  confirmToolAction: async (confirmId, approved) => {
    set((state) => {
      const updatedMessages: Record<string, ChatMessage[]> = {};
      for (const [sid, msgs] of Object.entries(state.messagesBySession)) {
        updatedMessages[sid] = msgs.map((m) => {
          if (m.role !== "agent") return m;
          const hasMatch = m.parts.some(
            (p) => p.type === "confirm" && p.confirm?.confirm_id === confirmId,
          );
          if (!hasMatch) return m;
          return {
            ...m,
            parts: m.parts.map((p) =>
              p.type === "confirm" && p.confirm?.confirm_id === confirmId
                ? {
                    ...p,
                    confirm: {
                      ...p.confirm!,
                      resolved: approved ? ("approved" as const) : ("rejected" as const),
                    },
                  }
                : p,
            ),
          };
        });
      }
      return { messagesBySession: updatedMessages };
    });
    try {
      await confirmTool(confirmId, approved);
    } catch (e) {
      console.error("Failed to confirm tool:", errMsg(e));
    }
  },

  initEventListener: async () => {
    const { eventUnlisten } = get();
    if (eventUnlisten) return;

    // Mark as in-progress immediately to prevent duplicate registration
    // (React StrictMode runs useEffect twice in dev)
    set({ eventUnlisten: "pending" });

    try {
      const unlisten = await onAppEvent((payload) => {
        get().handleEvent(payload);
      });
      set({ eventUnlisten: unlisten });
    } catch (e) {
      set({ eventUnlisten: null });
      console.error("Failed to init event listener:", errMsg(e));
    }
  },

  handleEvent: (payload) => {
    const { session_id, event } = payload;
    const state = get();

    if (event.type === "agent_started") {
      const agentMsg: ChatMessage = {
        id: `agent-${agentMessageCounter++}`,
        role: "agent",
        content: "",
        parts: [],
        status: "streaming",
      };
      const existing = state.messagesBySession[session_id] ?? [];
      // Only add agent message if there isn't already a streaming agent
      // message for this session (prevents duplicates from race conditions)
      const hasStreamingAgent = existing.some(
        (m) => m.role === "agent" && m.status === "streaming",
      );
      const messages = hasStreamingAgent
        ? existing
        : [...existing, agentMsg];

      set({
        agentRunning: { ...state.agentRunning, [session_id]: true },
        currentSessionId: state.currentSessionId ?? session_id,
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: messages,
        },
      });
      return;
    }

    if (event.type === "llm_thinking") {
      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length === 0) return;
      const lastIdx = messages.length - 1;
      const lastMsg = messages[lastIdx];
      if (lastMsg.role !== "agent") return;

      const updatedParts = [...lastMsg.parts];
      const lastPart = updatedParts[updatedParts.length - 1];

      if (lastPart && lastPart.type === "text" && lastPart.text) {
        updatedParts[updatedParts.length - 1] = {
          ...lastPart,
          text: lastPart.text + event.token,
        };
      } else {
        updatedParts.push({ type: "text", text: event.token });
      }

      const updatedMessages = [...messages];
      updatedMessages[lastIdx] = { ...lastMsg, parts: updatedParts };
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: updatedMessages,
        },
      });
      return;
    }

    if (event.type === "confirm_required") {
      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length === 0) return;
      const lastIdx = messages.length - 1;
      const lastMsg = messages[lastIdx];
      if (lastMsg.role !== "agent") return;

      const confirmPart: ChatPart = {
        type: "confirm",
        confirm: {
          confirm_id: event.confirm_id,
          session_id: session_id,
          tool: event.tool,
          args: event.args,
          risk_level: event.risk_level,
          resolved: "pending",
        },
      };

      const updatedMessages = [...messages];
      updatedMessages[lastIdx] = {
        ...lastMsg,
        parts: [...lastMsg.parts, confirmPart],
      };
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: updatedMessages,
        },
      });
      return;
    }

    if (event.type === "tool_executing") {
      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length === 0) return;
      const lastIdx = messages.length - 1;
      const lastMsg = messages[lastIdx];
      if (lastMsg.role !== "agent") return;

      const toolPart: ChatPart = {
        type: "tool",
        tool: {
          name: event.tool,
          args: event.args,
          status: "running",
        },
      };

      // 已批准的确认卡片在其位置被执行卡片接管（按 tool + args 匹配）
      const argsStr = JSON.stringify(event.args);
      let replaceAt = -1;
      for (let i = lastMsg.parts.length - 1; i >= 0; i--) {
        const p = lastMsg.parts[i];
        if (
          p.type === "confirm" &&
          p.confirm &&
          p.confirm.resolved === "approved" &&
          p.confirm.tool === event.tool &&
          JSON.stringify(p.confirm.args) === argsStr
        ) {
          replaceAt = i;
          break;
        }
      }

      const updatedParts = [...lastMsg.parts];
      if (replaceAt >= 0) {
        updatedParts[replaceAt] = toolPart;
      } else {
        updatedParts.push(toolPart);
      }

      const updatedMessages = [...messages];
      updatedMessages[lastIdx] = {
        ...lastMsg,
        parts: updatedParts,
      };
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: updatedMessages,
        },
      });
      return;
    }

    if (event.type === "provision_progress") {
      // 装备进度：附加到最近一个 running 的同名工具卡片（状态行文本）
      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length === 0) return;
      const lastIdx = messages.length - 1;
      const lastMsg = messages[lastIdx];
      if (lastMsg.role !== "agent") return;

      const updatedParts = [...lastMsg.parts];
      for (let i = updatedParts.length - 1; i >= 0; i--) {
        const part = updatedParts[i];
        if (part.type === "tool" && part.tool && part.tool.name === event.tool && part.tool.status === "running") {
          updatedParts[i] = {
            ...part,
            tool: {
              ...part.tool,
              // 复用 output 字段展示当前阶段（tool_result 到达后会被覆盖）
              output: `${event.stage}: ${event.detail}`,
            },
          };
          break;
        }
      }

      const updatedMessages = [...messages];
      updatedMessages[lastIdx] = { ...lastMsg, parts: updatedParts };
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: updatedMessages,
        },
      });
      return;
    }

    if (event.type === "transfer_progress" || event.type === "transfer_finished") {
      const messages = state.messagesBySession[session_id] ?? [];
      const speed = event.type === "transfer_progress" ? event.speed_bps : 0;
      const attempt = event.type === "transfer_progress" ? event.attempt : 0;
      const info: TransferInfo = {
        transfer_id: event.transfer_id,
        direction: event.direction,
        status: event.status,
        transferred_bytes: event.transferred_bytes,
        total_bytes: event.total_bytes,
        speed_bps: speed,
        attempt,
        error: "error" in event ? event.error : null,
        file_name: "remote_path" in event ? event.remote_path.split("/").pop() ?? event.remote_path : "",
      };

      let messages2 = messages;
      // 无 agent 消息时兜底新建一条承载（heap_dump 场景 Agent 可能已结束本轮回复）
      if (messages2.length === 0 || messages2[messages2.length - 1].role !== "agent") {
        messages2 = [
          ...messages2,
          {
            id: `agent-${agentMessageCounter++}`,
            role: "agent" as const,
            content: "",
            parts: [],
            status: "done" as const,
          },
        ];
      }

      const lastIdx = messages2.length - 1;
      const lastMsg = messages2[lastIdx];
      const updatedParts = [...lastMsg.parts];
      const existingIdx = updatedParts.findIndex(
        (p) => p.type === "transfer" && p.transfer?.transfer_id === event.transfer_id,
      );
      if (existingIdx >= 0) {
        updatedParts[existingIdx] = { ...updatedParts[existingIdx], transfer: info };
      } else {
        updatedParts.push({ type: "transfer", transfer: info });
      }

      const updatedMessages = [...messages2];
      updatedMessages[lastIdx] = { ...lastMsg, parts: updatedParts };
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: updatedMessages,
        },
      });
      return;
    }

    if (event.type === "tool_result") {
      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length === 0) return;
      const lastIdx = messages.length - 1;
      const lastMsg = messages[lastIdx];
      if (lastMsg.role !== "agent") return;

      const updatedParts = [...lastMsg.parts];
      for (let i = updatedParts.length - 1; i >= 0; i--) {
        const part = updatedParts[i];
        if (
          part.type === "tool" &&
          part.tool &&
          part.tool.name === event.tool &&
          part.tool.status === "running"
        ) {
          const output =
            typeof event.output === "string"
              ? event.output
              : JSON.stringify(event.output, null, 2);
          updatedParts[i] = {
            ...part,
            tool: {
              ...part.tool,
              status: "completed",
              output,
              elapsedMs: event.elapsed_ms,
            },
          };
          break;
        }
      }

      const updatedMessages = [...messages];
      updatedMessages[lastIdx] = { ...lastMsg, parts: updatedParts };
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: updatedMessages,
        },
      });
      return;
    }

    if (
      event.type === "diagnosis_done" ||
      event.type === "agent_stopped" ||
      event.type === "agent_crashed"
    ) {
      const newRunning = { ...state.agentRunning };
      delete newRunning[session_id];

      // 未决确认卡片翻转为超时（后端 120s 超时是真正兜底），同时终结消息状态——一次 set 避免快照覆盖
      let messages = state.messagesBySession[session_id] ?? [];
      if (messages.length > 0) {
        const finalStatus =
          event.type === "diagnosis_done"
            ? "done"
            : event.type === "agent_stopped"
              ? "stopped"
              : "error";
        messages = messages.map((m, idx) => {
          let parts = m.parts;
          if (parts.some((p) => p.type === "confirm" && p.confirm?.resolved === "pending")) {
            parts = parts.map((p) =>
              p.type === "confirm" && p.confirm?.resolved === "pending"
                ? { ...p, confirm: { ...p.confirm!, resolved: "timeout" as const } }
                : p,
            );
          }
          if (idx === messages.length - 1 && m.role === "agent") {
            return { ...m, parts, status: finalStatus as ChatMessage["status"] };
          }
          return { ...m, parts };
        });
      }

      set({
        agentRunning: newRunning,
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: messages,
        },
      });
      return;
    }

    if (event.type === "session_closed") {
      set({
        sessions: get().sessions.map((s) =>
          s.id === session_id ? { ...s, status: "closed" as const } : s,
        ),
      });
      return;
    }

    if (event.type === "session_deleted") {
      const { messagesBySession, currentSessionId } = get();
      const newMessages = { ...messagesBySession };
      delete newMessages[session_id];
      set({
        sessions: get().sessions.filter((s) => s.id !== session_id),
        archivedSessions: get().archivedSessions.filter((s) => s.id !== session_id),
        messagesBySession: newMessages,
        currentSessionId: currentSessionId === session_id ? null : currentSessionId,
      });
      return;
    }
  },
}));
