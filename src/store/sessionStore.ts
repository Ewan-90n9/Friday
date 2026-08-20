import { create } from "zustand";
import type { SessionRow, ChatMessage, ChatPart, AppEvent } from "@/lib/types";
import { sendMessage as ipcSendMessage, stopAgent, listSessions, onAppEvent } from "@/lib/ipc";

interface SessionStore {
  sessions: SessionRow[];
  currentSessionId: string | null;
  messagesBySession: Record<string, ChatMessage[]>;
  agentRunning: Record<string, boolean>;
  inputText: string;
  eventUnlisten: (() => void) | null;

  loadSessions: () => Promise<void>;
  selectSession: (id: string) => void;
  newSession: () => void;
  setInputText: (text: string) => void;
  sendMessage: () => Promise<void>;
  stopAgent: () => Promise<void>;
  initEventListener: () => Promise<void>;
  handleEvent: (payload: { session_id: string; event: AppEvent }) => void;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

let agentMessageCounter = 0;

export const useSessionStore = create<SessionStore>((set, get) => ({
  sessions: [],
  currentSessionId: null,
  messagesBySession: {},
  agentRunning: {},
  inputText: "",
  eventUnlisten: null,

  loadSessions: async () => {
    try {
      const sessions = await listSessions();
      set({ sessions });
    } catch (e) {
      console.error("Failed to load sessions:", errMsg(e));
    }
  },

  selectSession: (id) => set({ currentSessionId: id }),

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
        const messages =
          state.currentSessionId === null ? [userMsg] : [...existing, userMsg];
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

  initEventListener: async () => {
    const { eventUnlisten } = get();
    if (eventUnlisten) return;

    const unlisten = await onAppEvent((payload) => {
      get().handleEvent(payload);
    });
    set({ eventUnlisten: unlisten });
  },

  handleEvent: (payload) => {
    const { session_id, event } = payload;
    const state = get();

    if (event.type === "agent_started") {
      set({
        agentRunning: { ...state.agentRunning, [session_id]: true },
      });
      const agentMsg: ChatMessage = {
        id: `agent-${agentMessageCounter++}`,
        role: "agent",
        content: "",
        parts: [],
        status: "streaming",
      };
      const existing = state.messagesBySession[session_id] ?? [];
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: [...existing, agentMsg],
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

      const updatedMessages = [...messages];
      updatedMessages[lastIdx] = {
        ...lastMsg,
        parts: [...lastMsg.parts, toolPart],
      };
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
      set({ agentRunning: newRunning });

      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length > 0) {
        const lastIdx = messages.length - 1;
        const lastMsg = messages[lastIdx];
        if (lastMsg.role === "agent") {
          const status =
            event.type === "diagnosis_done"
              ? "done"
              : event.type === "agent_stopped"
                ? "stopped"
                : "error";
          const updatedMessages = [...messages];
          updatedMessages[lastIdx] = { ...lastMsg, status };
          set({
            messagesBySession: {
              ...state.messagesBySession,
              [session_id]: updatedMessages,
            },
          });
        }
      }
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
  },
}));
