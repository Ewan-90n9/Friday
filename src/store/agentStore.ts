import { create } from "zustand";
import type { AgentRow } from "@/lib/types";
import { detectAgents, listAgents, addAgent, setActiveAgent, removeAgent } from "@/lib/ipc";

interface AgentStore {
  agents: AgentRow[];
  activeAgent: AgentRow | null;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  load: () => Promise<void>;
  addManual: (provider: string, path: string) => Promise<void>;
  setActive: (id: string) => Promise<void>;
  remove: (id: string) => Promise<void>;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export const useAgentStore = create<AgentStore>((set, get) => ({
  agents: [],
  activeAgent: null,
  loading: false,
  error: null,

  refresh: async () => {
    set({ loading: true, error: null });
    try {
      await detectAgents();
      await get().load();
    } catch (e) {
      set({ error: errMsg(e) });
    } finally {
      set({ loading: false });
    }
  },

  load: async () => {
    set({ error: null });
    try {
      const agents = await listAgents();
      set({ agents, activeAgent: agents.find((a) => a.is_active) ?? null });
    } catch (e) {
      set({ error: errMsg(e) });
    }
  },

  addManual: async (provider, path) => {
    set({ error: null });
    try {
      await addAgent(provider, path);
      await get().load();
    } catch (e) {
      set({ error: errMsg(e) });
    }
  },

  setActive: async (id) => {
    set({ error: null });
    try {
      await setActiveAgent(id);
      await get().load();
    } catch (e) {
      set({ error: errMsg(e) });
    }
  },

  remove: async (id) => {
    set({ error: null });
    try {
      await removeAgent(id);
      await get().load();
    } catch (e) {
      set({ error: errMsg(e) });
    }
  },
}));
