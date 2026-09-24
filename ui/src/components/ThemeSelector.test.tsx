import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ThemeSelector } from "./ThemeSelector";
import { THEMES, STORAGE_KEY } from "../theme";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  localStorage.clear();
  delete document.documentElement.dataset.theme;
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

function select(): HTMLSelectElement {
  return host.querySelector('select[aria-label="Theme"]') as HTMLSelectElement;
}

test("lists every theme with Anvilmar selected by default", () => {
  act(() => root.render(<ThemeSelector />));
  const options = [...select().options].map((o) => o.value);
  expect(options).toEqual(THEMES.map((t) => t.id));
  expect(select().value).toBe("anvilmar");
});

test("reflects the saved theme", () => {
  localStorage.setItem(STORAGE_KEY, "obsidian");
  act(() => root.render(<ThemeSelector />));
  expect(select().value).toBe("obsidian");
});

test("changing the selection applies and persists the theme", () => {
  act(() => root.render(<ThemeSelector />));
  act(() => {
    select().value = "anvilmar-light";
    select().dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(document.documentElement.dataset.theme).toBe("anvilmar-light");
  expect(localStorage.getItem(STORAGE_KEY)).toBe("anvilmar-light");
  expect(select().value).toBe("anvilmar-light");
});
