import { create } from "zustand";
import type { CredentialInput, EnvironmentRow, TestConnectionResult } from "@/lib/types";
import {
  listEnvironments as ipcList,
  saveEnvironment as ipcSave,
  deleteEnvironment as ipcDelete,
  testConnection as ipcTest,
} from "@/lib/ipc";

interface EnvStore {
  environments: EnvironmentRow[];
  loading: boolean;
  error: string | null;
  load: () => Promise<void>;
  save: (params: {
    environmentId?: string | null;
    name: string;
    host: string;
    port?: number;
    credentials: CredentialInput[];
  }) => Promise<boolean>;
  remove: (id: string) => Promise<boolean>;
  test: (params: Parameters<typeof ipcTest>[0]) => Promise<TestConnectionResult | null>;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export const useEnvStore = create<EnvStore>((set, get) => ({
  environments: [],
  loading: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const environments = await ipcList();
      set({ environments });
    } catch (e) {
      set({ error: errMsg(e) });
    } finally {
      set({ loading: false });
    }
  },

  save: async (params) => {
    set({ error: null });
    try {
      await ipcSave(params);
      await get().load();
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    }
  },

  remove: async (id) => {
    set({ error: null });
    try {
      await ipcDelete(id);
      await get().load();
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    }
  },

  test: async (params) => {
    try {
      return await ipcTest(params);
    } catch (e) {
      set({ error: errMsg(e) });
      return null;
    }
  },
}));
