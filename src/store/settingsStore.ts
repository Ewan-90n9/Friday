import { create } from "zustand";
import { getArtifactoryBaseUrl, setArtifactoryBaseUrl } from "@/lib/ipc";

interface SettingsStore {
  artifactoryBaseUrl: string;
  loading: boolean;
  saving: boolean;
  error: string | null;
  load: () => Promise<void>;
  saveBaseUrl: (url: string) => Promise<boolean>;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export const useSettingsStore = create<SettingsStore>((set, get) => ({
  artifactoryBaseUrl: "",
  loading: false,
  saving: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const url = await getArtifactoryBaseUrl();
      set({ artifactoryBaseUrl: url });
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
}));
