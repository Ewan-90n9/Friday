import { create } from "zustand";
import {
  getArtifactoryBaseUrl,
  setArtifactoryBaseUrl,
  getAutoApproveTools,
  setAutoApproveTools,
} from "@/lib/ipc";

interface SettingsStore {
  artifactoryBaseUrl: string;
  autoApprove: boolean;
  loading: boolean;
  saving: boolean;
  error: string | null;
  load: () => Promise<void>;
  saveBaseUrl: (url: string) => Promise<boolean>;
  saveAutoApprove: (enabled: boolean) => Promise<boolean>;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export const useSettingsStore = create<SettingsStore>((set, get) => ({
  artifactoryBaseUrl: "",
  autoApprove: false,
  loading: false,
  saving: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const [url, autoApprove] = await Promise.all([
        getArtifactoryBaseUrl(),
        getAutoApproveTools(),
      ]);
      set({ artifactoryBaseUrl: url, autoApprove });
    } catch (e) {
      set({ error: errMsg(e) });
    } finally {
      set({ loading: false });
    }
  },

  saveBaseUrl: async (url) => {
    set({ saving: true, error: null });
    try {
      await setArtifactoryBaseUrl(url);
      await get().load();
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    } finally {
      set({ saving: false });
    }
  },

  saveAutoApprove: async (enabled) => {
    set({ saving: true, error: null });
    try {
      await setAutoApproveTools(enabled);
      set({ autoApprove: enabled });
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    } finally {
      set({ saving: false });
    }
  },
}));
