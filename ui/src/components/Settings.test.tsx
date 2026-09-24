import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Settings } from "./Settings";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  window.anamnesis = {
    getConfig: vi.fn(async () => ({ watchDirs: ["/v"], localModelName: "Xenova/all-MiniLM-L6-v2", mcpPort: 8867 })),
    getLogPath: vi.fn(async () => "/logs/anamnesis.log"),
    saveConfig: vi.fn(async () => undefined),
    openLogFile: vi.fn(async () => undefined),
  } as unknown as typeof window.anamnesis;
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

async function render() {
  await act(async () => root.render(<Settings />));
}

test("OpenAI provider settings are gone", async () => {
  await render();
  expect(host.textContent).toContain("Embedding");
  expect(host.textContent).not.toMatch(/openai/i);
  expect(host.querySelector('input[type="password"]')).toBeNull();
});

test("has an Appearance section with the theme selector", async () => {
  await render();
  expect(host.textContent).toContain("Appearance");
  expect(host.querySelector('select[aria-label="Theme"]')).not.toBeNull();
});

test("port hint no longer mentions the removed management API", async () => {
  await render();
  expect(host.textContent).not.toContain("mgmt");
});

test("theme changes do not mark the config dirty", async () => {
  await render();
  const sel = host.querySelector('select[aria-label="Theme"]') as HTMLSelectElement;
  await act(async () => {
    sel.value = "obsidian";
    sel.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(host.textContent).not.toContain("Unsaved changes");
  expect(window.anamnesis.saveConfig).not.toHaveBeenCalled();
});
