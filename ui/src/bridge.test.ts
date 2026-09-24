import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { createBridge, detectPlatform, installBridge } from "./bridge";

let statusCb: ((e: { payload: unknown }) => void) | null = null;
const unlisten = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => "result") }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, cb: (e: { payload: unknown }) => void) => { statusCb = cb; return unlisten; }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => "/picked/dir") }));

beforeEach(() => vi.clearAllMocks());

const cases: [string, unknown[], string, Record<string, unknown> | undefined][] = [
  ["getConfigPath", [], "get_config_path", undefined],
  ["getStatus", [], "get_status", undefined],
  ["restart", [], "restart", undefined],
  ["reindex", [], "reindex", undefined],
  ["pause", [], "pause", undefined],
  ["resume", [], "resume", undefined],
  ["flush", [], "flush", undefined],
  ["getDirs", [], "get_dirs", undefined],
  ["pauseDir", ["/v"], "pause_dir", { dir: "/v" }],
  ["resumeDir", ["/v"], "resume_dir", { dir: "/v" }],
  ["reindexDir", ["/v"], "reindex_dir", { dir: "/v" }],
  ["startMcp", [], "start_mcp", undefined],
  ["stopMcp", [], "stop_mcp", undefined],
  ["getVectors", [], "get_vectors", undefined],
  ["getConfig", [], "get_config", undefined],
  ["saveConfig", [{ hybridSearch: false }], "save_config", { partial: { hybridSearch: false } }],
  ["search", ["forge", 7], "search", { query: "forge", limit: 7 }],
  ["getLogPath", [], "get_log_path", undefined],
  ["openLogFile", [], "open_log_file", undefined],
];

test.each(cases)("%s invokes %s", async (method, args, command, payload) => {
  const api = createBridge() as unknown as Record<string, (...a: unknown[]) => Promise<unknown>>;
  await expect(api[method](...args)).resolves.toBe("result");
  if (payload === undefined) expect(invoke).toHaveBeenCalledWith(command);
  else expect(invoke).toHaveBeenCalledWith(command, payload);
});

test("search defaults to 15 results like the Electron preload", async () => {
  await createBridge().search("q");
  expect(invoke).toHaveBeenCalledWith("search", { query: "q", limit: 15 });
});

test("openDirDialog picks one directory and maps cancel to null", async () => {
  const api = createBridge();
  await expect(api.openDirDialog()).resolves.toBe("/picked/dir");
  expect(open).toHaveBeenCalledWith({ directory: true, multiple: false });
  vi.mocked(open).mockResolvedValueOnce(null);
  await expect(api.openDirDialog()).resolves.toBeNull();
});

test("openFileFolder asks the backend to reveal the file in the OS explorer", async () => {
  await createBridge().openFileFolder("/v/a.md");
  expect(invoke).toHaveBeenCalledWith("open_file_folder", { filePath: "/v/a.md" });
});

test("onStatusUpdate forwards event payloads and unsubscribes", async () => {
  const cb = vi.fn();
  const off = createBridge().onStatusUpdate(cb);
  expect(listen).toHaveBeenCalledWith("core-status-update", expect.any(Function));
  await Promise.resolve();
  statusCb!({ payload: { chunkCount: 3 } });
  expect(cb).toHaveBeenCalledWith({ chunkCount: 3 });
  off();
  await Promise.resolve();
  await Promise.resolve();
  expect(unlisten).toHaveBeenCalledTimes(1);
});

test("unsubscribing before listen resolves still unlistens", async () => {
  const off = createBridge().onStatusUpdate(() => {});
  off();
  await new Promise((r) => setTimeout(r, 0));
  expect(unlisten).toHaveBeenCalledTimes(1);
});

test("detectPlatform maps user agents to node-style names", () => {
  expect(detectPlatform("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")).toBe("win32");
  expect(detectPlatform("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)")).toBe("darwin");
  expect(detectPlatform("Mozilla/5.0 (X11; Linux x86_64)")).toBe("linux");
});

test("installBridge exposes the API on window", () => {
  installBridge();
  expect(typeof window.anamnesis.getStatus).toBe("function");
  expect(typeof window.anamnesis.platform).toBe("string");
});
