import { create } from "zustand";
import type { Session, AppEvent } from "@/lib/types";

interface SessionStore {
  sessions: Session[];
  currentSessionId: string | null;
  eventsBySession: Record<string, AppEvent[]>;

  selectSession: (id: string) => void;
  appendEvent: (sessionId: string, event: AppEvent) => void;
  clearEvents: (sessionId: string) => void;
}

export const useSessionStore = create<SessionStore>((set) => ({
  sessions: [],
  currentSessionId: null,
  eventsBySession: {},

  selectSession: (id) => set({ currentSessionId: id }),

  appendEvent: (sessionId, event) =>
    set((state) => ({
      eventsBySession: {
        ...state.eventsBySession,
        [sessionId]: [...(state.eventsBySession[sessionId] ?? []), event],
      },
    })),

  clearEvents: (sessionId) =>
    set((state) => {
      const next = { ...state.eventsBySession };
      delete next[sessionId];
      return { eventsBySession: next };
    }),
}));
