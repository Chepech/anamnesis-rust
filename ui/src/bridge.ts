import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

/**
 * Recreates the `window.anamnesis` API the React components were written against (it used to
 * come from the Electron preload) on top of Tauri commands and events.
 */

export interface AnamnesisApi {
  getConfigPath(): Promise<string>;
  getStatus(): Promise<unknown>;
  restart(): Promise<void>;
  reindex(): Promise<void>;
  pause(): Promise<void>;
  resume(): Promise<void>;
  flush(): Promise<void>;
  getDirs(): Promise<unknown>;
  pauseDir(dir: string): Promise<void>;
  resumeDir(dir: string): Promise<void>;
  reindexDir(dir: string): Promise<void>;
  startMcp(): Promise<void>;
  stopMcp(): Promise<void>;
  getVectors(): Promise<unknown>;
  openDirDialog(): Promise<string | null>;
  getConfig(): Promise<unknown>;
  saveConfig(partial: unknown): Promise<void>;
  search(query: string, limit?: number): Promise<unknown>;
  openFileFolder(filePath: string): Promise<void>;
  platform: string;
  getLogPath(): Promise<string>;
  openLogFile(): Promise<void>;
  onStatusUpdate(cb: (payload: unknown) => void): () => void;
}

declare global {
  interface Window {
    anamnesis: AnamnesisApi;
  }
}

export function detectPlatform(ua: string): string {
  if (ua.includes("Windows")) return "win32";
  if (ua.includes("Mac")) return "darwin";
  return "linux";
}

export function createBridge(): AnamnesisApi {
  return {
    getConfigPath: () => invoke("get_config_path"),
    getStatus: () => invoke("get_status"),
    restart: () => invoke("restart"),
    reindex: () => invoke("reindex"),
    pause: () => invoke("pause"),
    resume: () => invoke("resume"),
    flush: () => invoke("flush"),
    getDirs: () => invoke("get_dirs"),
    pauseDir: (dir) => invoke("pause_dir", { dir }),
    resumeDir: (dir) => invoke("resume_dir", { dir }),
    reindexDir: (dir) => invoke("reindex_dir", { dir }),
    startMcp: () => invoke("start_mcp"),
    stopMcp: () => invoke("stop_mcp"),
    getVectors: () => invoke("get_vectors"),
    openDirDialog: async () => {
      const picked = await open({ directory: true, multiple: false });
      return typeof picked === "string" ? picked : null;
    },
    getConfig: () => invoke("get_config"),
    saveConfig: (partial) => invoke("save_config", { partial }),
    search: (query, limit = 15) => invoke("search", { query, limit }),
    openFileFolder: (filePath) => invoke("open_file_folder", { filePath }),
    platform: detectPlatform(navigator.userAgent),
    getLogPath: () => invoke("get_log_path"),
    openLogFile: () => invoke("open_log_file"),
    onStatusUpdate: (cb) => {
      const off = listen("core-status-update", (e) => cb(e.payload));
      return () => void off.then((f) => f());
    },
  };
}

export function installBridge(): void {
  window.anamnesis = createBridge();
}
