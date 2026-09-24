// Tray app: one process hosting the Rust core, a tray menu and an on-demand React panel.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anamnesis_core::config::Config;
use anamnesis_core::engine::{DirInfo, Engine};
use anamnesis_core::search::SearchHit;
use anamnesis_core::store::VectorNode;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

const TRAY_ID: &str = "main";
const PANEL: &str = "panel";

struct AppState {
    engine: tokio::sync::RwLock<Option<Arc<Engine>>>,
    boot_error: Mutex<Option<String>>,
    config_path: PathBuf,
    log_path: PathBuf,
    /// Last tray menu shape, so status bursts during indexing don't rebuild the menu each time.
    menu_key: Mutex<String>,
}

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Same location Electron used (`<config dir>/Anamnesis`), so existing installs keep their config.
fn app_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("Anamnesis")
}

async fn engine(state: &State<'_, AppState>) -> Res<Arc<Engine>> {
    state.engine.read().await.clone().ok_or_else(|| "Anamnesis core is still starting".to_string())
}

/// Blocking core calls (SQLite, ORT) off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Res<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(err)?.map_err(err)
}

// ── Boot / restart ──────────────────────────────────────────────────────────

fn starting_payload(state: &AppState) -> Value {
    match state.boot_error.lock().unwrap().clone() {
        Some(e) => json!({ "status": "error", "error": e }),
        None => json!({ "status": "starting" }),
    }
}

fn emit_status(app: &AppHandle, payload: Value) {
    let _ = app.emit("core-status-update", payload.clone());
    update_tray(app, &payload);
}

async fn boot(app: AppHandle) {
    let state = app.state::<AppState>();
    *state.boot_error.lock().unwrap() = None;
    emit_status(&app, starting_payload(&state));
    let config_path = state.config_path.clone();
    let result = async {
        let cfg = Config::load(&config_path);
        tracing::info!("loading embedding model {}", cfg.local_model_name);
        let embedder = blocking(move || Engine::load_embedder(&cfg)).await?;
        let path = config_path.clone();
        let engine = blocking(move || Engine::open(&path, embedder)).await?;
        let handle = app.clone();
        engine.on_status(move |p| emit_status(&handle, serde_json::to_value(p).unwrap_or_default()));
        if engine.config().mcp_enabled {
            if let Err(e) = engine.start_mcp().await {
                tracing::error!("MCP server failed to start: {e:#}");
            }
        }
        Res::Ok(engine)
    }
    .await;
    match result {
        Ok(engine) => {
            let payload = serde_json::to_value(engine.status()).unwrap_or_default();
            *state.engine.write().await = Some(engine);
            emit_status(&app, payload);
            tracing::info!("core ready");
        }
        Err(e) => {
            tracing::error!("core failed to start: {e}");
            *state.boot_error.lock().unwrap() = Some(e);
            emit_status(&app, starting_payload(&state));
        }
    }
}

async fn restart_engine(app: AppHandle) {
    let old = app.state::<AppState>().engine.write().await.take();
    if let Some(e) = old {
        e.shutdown().await;
    }
    boot(app).await;
}

// ── Commands (one per window.anamnesis method, see ui/src/bridge.ts) ───────

#[tauri::command]
fn get_config_path(state: State<'_, AppState>) -> String {
    state.config_path.to_string_lossy().into_owned()
}

#[tauri::command]
fn get_log_path(state: State<'_, AppState>) -> String {
    state.log_path.to_string_lossy().into_owned()
}

#[tauri::command]
fn open_log_file(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    app.opener().open_path(state.log_path.to_string_lossy(), None::<&str>).map_err(err)
}

#[tauri::command]
fn open_file_folder(app: AppHandle, file_path: String) -> Res<()> {
    app.opener().reveal_item_in_dir(file_path).map_err(err)
}

#[tauri::command]
async fn get_status(state: State<'_, AppState>) -> Res<Value> {
    match state.engine.read().await.clone() {
        Some(e) => serde_json::to_value(e.status()).map_err(err),
        None => Ok(starting_payload(&state)),
    }
}

#[tauri::command]
async fn restart(app: AppHandle) -> Res<()> {
    restart_engine(app).await;
    Ok(())
}

#[tauri::command]
async fn reindex(state: State<'_, AppState>) -> Res<()> {
    engine(&state).await?.reindex();
    Ok(())
}

#[tauri::command]
async fn pause(state: State<'_, AppState>) -> Res<()> {
    engine(&state).await?.pause();
    Ok(())
}

#[tauri::command]
async fn resume(state: State<'_, AppState>) -> Res<()> {
    engine(&state).await?.resume();
    Ok(())
}

#[tauri::command]
async fn flush(state: State<'_, AppState>) -> Res<()> {
    engine(&state).await?.flush();
    Ok(())
}

#[tauri::command]
async fn get_dirs(state: State<'_, AppState>) -> Res<Vec<DirInfo>> {
    match state.engine.read().await.clone() {
        Some(e) => blocking(move || e.dirs()).await,
        // Core not up yet: list the configured dirs so the Dashboard still renders.
        None => Ok(Config::load(&state.config_path)
            .watch_dirs
            .into_iter()
            .map(|path| DirInfo { path, paused: false, chunk_count: 0 })
            .collect()),
    }
}

#[tauri::command]
async fn pause_dir(state: State<'_, AppState>, dir: String) -> Res<()> {
    engine(&state).await?.pause_dir(&dir);
    Ok(())
}

#[tauri::command]
async fn resume_dir(state: State<'_, AppState>, dir: String) -> Res<()> {
    engine(&state).await?.resume_dir(&dir);
    Ok(())
}

#[tauri::command]
async fn reindex_dir(state: State<'_, AppState>, dir: String) -> Res<()> {
    engine(&state).await?.reindex_dir(&dir);
    Ok(())
}

#[tauri::command]
async fn start_mcp(state: State<'_, AppState>) -> Res<()> {
    engine(&state).await?.start_mcp().await.map_err(err)
}

#[tauri::command]
async fn stop_mcp(state: State<'_, AppState>) -> Res<()> {
    engine(&state).await?.stop_mcp().await;
    Ok(())
}

#[tauri::command]
async fn get_vectors(state: State<'_, AppState>) -> Res<Vec<VectorNode>> {
    let e = engine(&state).await?;
    blocking(move || e.vectors(2000)).await
}

#[tauri::command]
async fn search(state: State<'_, AppState>, query: String, limit: Option<usize>) -> Res<Vec<SearchHit>> {
    let e = engine(&state).await?;
    blocking(move || e.search(&query, limit)).await
}

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Res<Config> {
    Ok(match state.engine.read().await.clone() {
        Some(e) => e.config(),
        None => Config::load(&state.config_path),
    })
}

#[tauri::command]
async fn save_config(app: AppHandle, state: State<'_, AppState>, partial: Value) -> Res<()> {
    match state.engine.read().await.clone() {
        Some(e) => {
            if e.save_config(&partial).await.map_err(err)? {
                tracing::info!("embedding model changed: restarting core");
                tauri::async_runtime::spawn(restart_engine(app));
            }
        }
        // Core not running (still booting or failed): persist so the next start picks it up.
        None => Config::load(&state.config_path).merged(&partial).map_err(err)?.save(&state.config_path).map_err(err)?,
    }
    Ok(())
}

// ── Window + tray ───────────────────────────────────────────────────────────

fn open_panel(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(PANEL) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, PANEL, WebviewUrl::App("index.html".into()))
        .title("Anamnesis")
        .inner_size(760.0, 680.0)
        .min_inner_size(600.0, 520.0)
        .build();
    if let Err(e) = built {
        tracing::error!("could not open panel: {e}");
    }
}

/// Mirrors the Electron tray menu. Fixes its swapped cancel actions: "Cancel" now cancels.
fn build_menu(app: &AppHandle, payload: &Value) -> tauri::Result<Menu<tauri::Wry>> {
    let state = payload["indexStatus"]["state"].as_str().unwrap_or(if payload["status"] == "error" { "error" } else { "idle" });
    let item = |id: &str, text: &str| MenuItem::with_id(app, id, text, true, None::<&str>);
    let labels = std::cell::Cell::new(0);
    let label = |text: &str| {
        labels.set(labels.get() + 1);
        MenuItem::with_id(app, format!("label-{}", labels.get()), text, false, None::<&str>)
    };
    let sep = || PredefinedMenuItem::separator(app);
    let mut items: Vec<Box<dyn tauri::menu::IsMenuItem<tauri::Wry>>> = vec![];
    match state {
        "indexing" => {
            items.push(Box::new(item("pause", "Pause indexing")?));
            items.push(Box::new(item("cancel", "Cancel indexing")?));
        }
        "paused" => {
            items.push(Box::new(item("resume", "Resume indexing")?));
            items.push(Box::new(item("cancel", "Cancel indexing")?));
        }
        "error" => {
            let msg = payload["indexStatus"]["message"].as_str().or(payload["error"].as_str()).unwrap_or("unknown");
            items.push(Box::new(label(&format!("Error: {}", msg.chars().take(80).collect::<String>()))?));
            items.push(Box::new(sep()?));
            items.push(Box::new(item("reindex", "Re-index vault")?));
        }
        "queued" => {
            let n = payload["indexStatus"]["count"].as_u64().unwrap_or(0);
            items.push(Box::new(label(&format!("{n} file{} queued, indexing soon", if n == 1 { "" } else { "s" }))?));
            items.push(Box::new(sep()?));
            items.push(Box::new(item("reindex", "Re-index vault now")?));
        }
        _ if payload["status"] == "starting" => items.push(Box::new(label("Starting…")?)),
        _ => items.push(Box::new(item("reindex", "Re-index vault")?)),
    }
    items.push(Box::new(sep()?));
    items.push(Box::new(item("open", "Open control panel")?));
    items.push(Box::new(sep()?));
    match payload["mcpStatus"].as_str() {
        Some("running") => {
            items.push(Box::new(label(&format!("MCP: port {}", payload["mcpPort"]))?));
            items.push(Box::new(item("stop_mcp", "Stop MCP server")?));
        }
        Some(_) => items.push(Box::new(item("start_mcp", "Start MCP server")?)),
        None => items.push(Box::new(label("MCP: starting")?)),
    }
    items.push(Box::new(sep()?));
    items.push(Box::new(item("quit", "Quit")?));
    let refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = items.iter().map(|b| b.as_ref()).collect();
    Menu::with_items(app, &refs)
}

fn update_tray(app: &AppHandle, payload: &Value) {
    let key = format!(
        "{}|{}|{}|{}|{}",
        payload["status"], payload["indexStatus"]["state"], payload["indexStatus"]["count"], payload["mcpStatus"], payload["mcpPort"]
    );
    {
        let state = app.state::<AppState>();
        let mut last = state.menu_key.lock().unwrap();
        if *last == key {
            return;
        }
        *last = key;
    }
    let (app2, payload) = (app.clone(), payload.clone());
    // Menus must be built on the main thread (macOS); status arrives from worker threads.
    let _ = app.run_on_main_thread(move || match build_menu(&app2, &payload) {
        Ok(menu) => {
            if let Some(tray) = app2.tray_by_id(TRAY_ID) {
                let _ = tray.set_menu(Some(menu));
            }
        }
        Err(e) => tracing::error!("tray menu: {e}"),
    });
}

fn on_menu(app: &AppHandle, id: &str) {
    let app = app.clone();
    let id = id.to_string();
    tauri::async_runtime::spawn(async move {
        if id == "open" {
            return open_panel(&app);
        }
        if id == "quit" {
            if let Some(e) = app.state::<AppState>().engine.write().await.take() {
                e.shutdown().await;
            }
            return app.exit(0);
        }
        let Some(e) = app.state::<AppState>().engine.read().await.clone() else { return };
        match id.as_str() {
            "reindex" => e.reindex(),
            "pause" => e.pause(),
            "resume" => e.resume(),
            "cancel" => e.cancel(),
            "start_mcp" => {
                let _ = e.start_mcp().await;
            }
            "stop_mcp" => e.stop_mcp().await,
            _ => {}
        }
    });
}

fn init_logging(log_path: &std::path::Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let dir = log_path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    let file = tracing_appender::rolling::never(dir, log_path.file_name()?);
    let (writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::fmt().with_writer(writer).with_ansi(false).with_env_filter("info").init();
    Some(guard)
}

fn main() {
    let dir = app_dir();
    let log_path = dir.join("logs").join("anamnesis.log");
    let _log_guard = init_logging(&log_path);
    tracing::info!("--- session started, v{} ---", env!("CARGO_PKG_VERSION"));

    let state = AppState {
        engine: tokio::sync::RwLock::new(None),
        boot_error: Mutex::new(None),
        config_path: dir.join("config.json"),
        log_path,
        menu_key: Mutex::new(String::new()),
    };

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| open_panel(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config_path,
            get_log_path,
            open_log_file,
            open_file_folder,
            get_status,
            restart,
            reindex,
            pause,
            resume,
            flush,
            get_dirs,
            pause_dir,
            resume_dir,
            reindex_dir,
            start_mcp,
            stop_mcp,
            get_vectors,
            search,
            get_config,
            save_config
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle().clone();
            let menu = build_menu(&handle, &json!({ "status": "starting" }))?;
            let mut tray = TrayIconBuilder::with_id(TRAY_ID)
                .tooltip("Anamnesis")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        open_panel(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            tauri::async_runtime::spawn(boot(handle));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Anamnesis");

    app.run(|_app, event| {
        // Closing the panel must not quit: the tray keeps running. Explicit Quit passes a code.
        if let RunEvent::ExitRequested { api, code: None, .. } = event {
            api.prevent_exit();
        }
    });
}
