import { create } from "zustand";
import {
  getArtifactoryBaseUrl,
  setArtifactoryBaseUrl,
  getAutoApproveTools,
  setAutoApproveTools,
  getConfirmationTimeout,
  setConfirmationTimeout,
} from "@/lib/ipc";

interface SettingsStore {
  artifactoryBaseUrl: string;
  autoApprove: boolean;
  confirmationTimeout: number;
  loading: boolean;
  saving: boolean;
  error: string | null;
  autoApproveError: string | null;
  confirmationTimeoutError: string | null;
  load: () => Promise<void>;
  saveBaseUrl: (url: string) => Promise<boolean>;
  saveAutoApprove: (enabled: boolean) => Promise<boolean>;
  saveConfirmationTimeout: (secs: string) => Promise<boolean>;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export const useSettingsStore = create<SettingsStore>((set, get) => ({
  artifactoryBaseUrl: "",
  autoApprove: false,
  confirmationTimeout: 120,
  loading: false,
  saving: false,
  error: null,
  autoApproveError: null,
  confirmationTimeoutError: null,

  load: async () => {
    set({ loading: true, error: null, autoApproveError: null, confirmationTimeoutError: null });
    try {
      const [url, autoApprove, confirmationTimeout] = await Promise.all([
        getArtifactoryBaseUrl(),
        getAutoApproveTools(),
        getConfirmationTimeout(),
      ]);
      set({ artifactoryBaseUrl: url, autoApprove, confirmationTimeout });
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
    set({ saving: true, autoApproveError: null });
    try {
      await setAutoApproveTools(enabled);
      set({ autoApprove: enabled });
      return true;
    } catch (e) {
      set({ autoApproveError: errMsg(e) });
      return false;
    } finally {
      set({ saving: false });
    }
  },

  saveConfirmationTimeout: async (secs) => {
    set({ saving: true, confirmationTimeoutError: null });
    try {
      await setConfirmationTimeout(secs);
      const saved = await getConfirmationTimeout();
      set({ confirmationTimeout: saved });
      return true;
    } catch (e) {
      set({ confirmationTimeoutError: errMsg(e) });
      return false;
    } finally {
      set({ saving: false });
    }
  },
}));
