import { THEMES, DEFAULT_THEME, STORAGE_KEY, applyTheme, loadTheme, isTheme, cssVar } from "./theme";

beforeEach(() => {
  localStorage.clear();
  delete document.documentElement.dataset.theme;
  document.documentElement.removeAttribute("style");
  vi.restoreAllMocks();
});

test("anvilmar is the default and listed first", () => {
  expect(DEFAULT_THEME).toBe("anvilmar");
  expect(THEMES[0].id).toBe("anvilmar");
  expect(new Set(THEMES.map((t) => t.id)).size).toBe(THEMES.length);
});

test("loadTheme falls back to the default when nothing or garbage is stored", () => {
  expect(loadTheme()).toBe("anvilmar");
  localStorage.setItem(STORAGE_KEY, "neon-unicorn");
  expect(loadTheme()).toBe("anvilmar");
});

test("loadTheme survives storage that throws (private mode, blocked site data)", () => {
  vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("denied"); });
  expect(loadTheme()).toBe("anvilmar");
});

test("applyTheme sets data-theme and persists the choice", () => {
  expect(applyTheme("obsidian")).toBe("obsidian");
  expect(document.documentElement.dataset.theme).toBe("obsidian");
  expect(localStorage.getItem(STORAGE_KEY)).toBe("obsidian");
  expect(loadTheme()).toBe("obsidian");
});

test("applyTheme rejects unknown ids and still applies when storage throws", () => {
  expect(applyTheme("nope")).toBe("anvilmar");
  expect(document.documentElement.dataset.theme).toBe("anvilmar");
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("full"); });
  expect(applyTheme("anvilmar-light")).toBe("anvilmar-light");
  expect(document.documentElement.dataset.theme).toBe("anvilmar-light");
});

test("isTheme narrows only known ids", () => {
  expect(isTheme("anvilmar-light")).toBe(true);
  expect(isTheme("ANVILMAR")).toBe(false);
  expect(isTheme(undefined)).toBe(false);
});

test("cssVar reads the live token or the fallback", () => {
  document.documentElement.style.setProperty("--fg-1", " #123456 ");
  expect(cssVar("--fg-1", "#fff")).toBe("#123456");
  expect(cssVar("--missing", "#fff")).toBe("#fff");
});
