import { create } from "zustand";

export type Theme = "dark" | "light" | "warm";

export const THEMES: { id: Theme; label: string }[] = [
  { id: "dark", label: "暗色" },
  { id: "light", label: "浅色" },
  { id: "warm", label: "暖白" },
];

const STORAGE_KEY = "friday.theme";

export function isTheme(value: string | null): value is Theme {
  return value === "dark" || value === "light" || value === "warm";
}

export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
}

export function loadStoredTheme(): Theme {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    return isTheme(stored) ? stored : "dark";
  } catch {
    return "dark";
  }
}

interface ThemeStore {
  theme: Theme;
  setTheme: (theme: Theme) => void;
}

export const useThemeStore = create<ThemeStore>((set) => ({
  theme: "dark",
  setTheme: (theme) => {
    applyTheme(theme);
    try {
      localStorage.setItem(STORAGE_KEY, theme);
    } catch {
      // localStorage 不可用时仅切换内存态，下次启动回落暗色
    }
    set({ theme });
  },
}));
