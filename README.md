# Anamnesis

![Anamnesis Logo](assets/AnamnesisLogo.png)

**Anamnesis** is a local-first semantic search engine and [MCP](https://modelcontextprotocol.io) server for your notes and documents. It lives in your system tray, indexes the folders you choose, and lets any MCP-compatible AI agent (Claude, Cursor, your own tools) search them in natural language.

No cloud, no API keys, no subscriptions. Your files never leave your machine.

This is the Rust rewrite of [anamnesis-standalone](https://github.com/Chepech/anamnesis-standalone): one small native binary (Tauri 2) instead of Electron + a Node daemon, with the same UI, config file, MCP URL and tools.

---

## Features

- **Watches your folders** with native OS file events (no polling) and re-indexes changes in the background
- **Parses** Markdown (frontmatter tags, wikilinks), PDF, DOCX and HTML into heading-aware chunks
- **Hybrid search**: local ONNX embeddings (`all-MiniLM-L6-v2` by default) + BM25 keyword ranking, fused with Reciprocal Rank Fusion, boosted by backlinks
- **Speaks MCP** over HTTP (`http://127.0.0.1:8867/mcp`) or stdio, with `search_vault`, `read_note` and `list_indexed_files`
- **Cheap re-indexing**: unchanged files are skipped by mtime and content hash, and unchanged chunks reuse their stored vectors, so editing one section of a long note re-embeds only that section
- **Vector graph**: a UMAP map of your notes, colored by folder, searchable
- **Themes**: Anvilmar (default), Anvilmar Light, Obsidian

## Install

Download the latest release for your platform from [Releases](../../releases):

| Platform | File |
|---|---|
| Windows x64 | `Anamnesis_x.y.z_x64-setup.exe` |
| macOS (Apple Silicon) | `Anamnesis_x.y.z_aarch64.dmg` |
| Linux x64 | `Anamnesis_x.y.z_amd64.AppImage` or `.deb` |
| Headless (any) | `anamnesis-core-<os>` (no UI, MCP only) |

The builds are not code-signed yet. On Windows, SmartScreen may show "Windows protected your PC": choose **More info → Run anyway**. On macOS, right-click the app and choose **Open** the first time.

On first launch the embedding model (~90 MB) downloads once into the app data folder.

## Use

1. **Add folders.** Click the tray icon and open the control panel. On the **Dashboard**, add any folders you want indexed: an Obsidian vault, a PDF library, a notes folder.
2. **Connect an agent.** For Claude Code:

   ```json
   { "mcpServers": { "anamnesis": { "type": "http", "url": "http://127.0.0.1:8867/mcp" } } }
   ```

   Or run the headless binary over stdio:

   ```json
   { "mcpServers": { "anamnesis": { "command": "anamnesis-core", "args": ["--stdio"] } } }
   ```

| Tool | What it does |
|---|---|
| `search_vault` | Hybrid search. Returns ranked chunks with path, heading breadcrumb, tags, backlink importance and score |
| `read_note` | Full content of a file inside one of your watched folders |
| `list_indexed_files` | Every indexed file with its chunk count |

## Configuration

Settings live in the control panel and in `config.json`:

| OS | Location |
|---|---|
| Windows | `%APPDATA%\Anamnesis\config.json` |
| macOS | `~/Library/Application Support/Anamnesis/config.json` |
| Linux | `~/.config/Anamnesis/config.json` |

The location and keys are the same as anamnesis-standalone, so an existing install keeps its folders and settings. The first launch builds a fresh index (`data/anamnesis.db`); the old `data/lancedb` folder can be deleted afterwards.

Headless: `anamnesis-core [--config PATH] [--stdio]` (default config: `<config dir>/anamnesis/config.json`).

## Architecture

```
anamnesis (Tauri 2 tray app, one process)
├─ tray + panel window (React UI, created on open)  ── window.anamnesis → Tauri commands/events
└─ anamnesis-core (Rust library, also a headless binary)
   ├─ watcher   notify (inotify / FSEvents / ReadDirectoryChangesW) → debounced, deduplicated queue
   ├─ indexer   parse (rayon) → chunk → embed in cross-file batches → one transaction per file
   ├─ embedder  fastembed-rs over ONNX Runtime (statically linked)
   ├─ store     SQLite: files, chunks, links, FTS5 (BM25) and sqlite-vec (cosine) in one file
   ├─ search    vector top-3N + BM25 top-3N → RRF (k = 60) + importance boost
   └─ mcp       rmcp streamable HTTP on 127.0.0.1 (+ stdio)
```

## Development

Requirements: Rust (stable), Node 22, pnpm 10. On Linux also `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev`.

```bash
pnpm --dir ui install
pnpm --dir ui tauri dev          # run the app
cargo test -p anamnesis-core     # core tests (no UI deps needed)
pnpm --dir ui test               # UI tests
cargo run -p anamnesis-core -- --config ./config.json   # headless
```

`main` accepts PRs from `hotfix/*` and `release/*` branches only. Releases run through the `Release` workflow (see [CLAUDE.md](CLAUDE.md)).

## License

[MIT](LICENSE)
