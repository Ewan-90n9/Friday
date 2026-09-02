import { useEffect, useRef, useState } from "react";
import { Palette, MoonStars, Sun, SunHorizon, Check } from "@phosphor-icons/react";
import { THEMES, useThemeStore, type Theme } from "@/store/themeStore";

const THEME_ICONS: Record<Theme, typeof Sun> = {
  dark: MoonStars,
  light: Sun,
  warm: SunHorizon,
};

export function ThemeMenu() {
  const [open, setOpen] = useState(false);
  const theme = useThemeStore((s) => s.theme);
  const setTheme = useThemeStore((s) => s.setTheme);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const raf = requestAnimationFrame(() => menuRef.current?.focus());
    const handlePointerDown = (e: PointerEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        triggerRef.current?.focus();
      }
    };
    document.addEventListener("pointerdown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      cancelAnimationFrame(raf);
      document.removeEventListener("pointerdown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [open]);

  const handleSelect = (id: Theme) => {
    setTheme(id);
    setOpen(false);
    triggerRef.current?.focus();
  };

  return (
    <div ref={rootRef} className="relative">
      <button
        ref={triggerRef}
        onClick={() => setOpen((s) => !s)}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="切换主题"
        className="flex items-center justify-center w-8 h-8 rounded-md text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
      >
        <Palette size={18} weight="regular" aria-hidden="true" />
      </button>
      {open && (
        <div
          ref={menuRef}
          role="menu"
          aria-label="主题"
          tabIndex={-1}
          className="absolute right-0 top-full mt-2 w-36 py-1 rounded-lg bg-card border border-border shadow-lg z-40 outline-none"
        >
          {THEMES.map(({ id, label }) => {
            const Icon = THEME_ICONS[id];
            const active = theme === id;
            return (
              <button
                key={id}
                role="menuitemradio"
                aria-checked={active}
                onClick={() => handleSelect(id)}
                className="flex items-center gap-2 w-full px-3 py-1.5 text-sm text-foreground hover:bg-surface-2 transition-colors cursor-pointer text-left"
              >
                <Icon
                  size={16}
                  weight="regular"
                  aria-hidden="true"
                  className="text-muted-foreground shrink-0"
                />
                <span className="flex-1">{label}</span>
                {active && (
                  <Check size={14} weight="bold" aria-hidden="true" className="text-accent shrink-0" />
                )}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
