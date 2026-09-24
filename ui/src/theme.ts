/** Theme registry. Anvilmar is the default and lives in `:root`; others override its tokens. */

export const THEMES = [
  { id: "anvilmar", label: "Anvilmar (default)" },
  { id: "anvilmar-light", label: "Anvilmar Light" },
  { id: "obsidian", label: "Obsidian" },
] as const;

export type ThemeId = (typeof THEMES)[number]["id"];

export const DEFAULT_THEME: ThemeId = "anvilmar";
export const STORAGE_KEY = "anamnesis.theme";

export function isTheme(id: unknown): id is ThemeId {
  return THEMES.some((t) => t.id === id);
}

/** Saved theme, or the default when nothing valid is stored or storage is unavailable. */
export function loadTheme(): ThemeId {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    return isTheme(saved) ? saved : DEFAULT_THEME;
  } catch {
    return DEFAULT_THEME;
  }
}

/** Sets `data-theme` on <html> and persists it. Unknown ids fall back to the default. */
export function applyTheme(id: string): ThemeId {
  const theme = isTheme(id) ? id : DEFAULT_THEME;
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    // Storage unavailable: the theme still applies for this session.
  }
  return theme;
}

/** Reads a CSS custom property from the active theme (canvas drawing can't use var()). */
export function cssVar(name: string, fallback: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}
