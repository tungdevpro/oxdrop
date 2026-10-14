//! The Sender window: a thin shell that renders the Sender engine's state and
//! forwards the user's actions to it. No transfer logic lives here.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Arc;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use oxdrop_core::{Item, format_size};
use oxdrop_server::{EngineConfig, SenderEngine};
use qrcode::QrCode;
use qrcode::render::svg;

const STYLE: &str = r#"<style>
:root { --bg: #f6f7f9; --card: #ffffff; --text: #16181d; --muted: #646b78; --accent: #2457d6; --line: #e2e5ea; --problem: #b42318; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #111317; --card: #1b1e24; --text: #eceef2; --muted: #9aa1ad; --accent: #7aa2ff; --line: #2b2f37; --problem: #ff8a80; }
}
* { box-sizing: border-box; }
html, body { margin: 0; height: 100%; background: var(--bg); color: var(--text); font: 14px/1.4 -apple-system, "Segoe UI", system-ui, sans-serif; user-select: none; -webkit-user-select: none; }
.app { display: flex; height: 100vh; gap: 20px; padding: 20px; }
.share { width: 260px; flex: none; display: flex; flex-direction: column; align-items: center; gap: 10px; padding: 20px; background: var(--card); border: 1px solid var(--line); border-radius: 14px; }
.qr { width: 220px; height: 220px; padding: 0; background: #fff; border-radius: 8px; overflow: hidden; }
.qr svg { width: 100%; height: 100%; display: block; }
.scan { margin: 4px 0 0; font-weight: 600; text-align: center; }
.link { margin: 0; color: var(--muted); font: 12px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; text-align: center; overflow-wrap: anywhere; user-select: text; -webkit-user-select: text; cursor: text; }
.items { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 10px; }
.drop { flex: 1; display: flex; align-items: center; justify-content: center; border: 2px dashed var(--line); border-radius: 14px; color: var(--muted); font-size: 16px; text-align: center; padding: 20px; }
.app.dragging .drop { border-color: var(--accent); color: var(--accent); }
ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; overflow-y: auto; }
li { display: flex; gap: 12px; padding: 12px 14px; background: var(--card); border: 1px solid var(--line); border-radius: 10px; }
.name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 500; }
.size { color: var(--muted); white-space: nowrap; }
.problem { margin: 0; color: var(--problem); white-space: pre-line; }
.toolbar { display: flex; justify-content: flex-end; }
.button { display: inline-block; padding: 7px 14px; border-radius: 8px; background: var(--accent); color: #fff; font-weight: 600; cursor: default; }
.button:active { opacity: .85; }
.remove { flex: none; width: 24px; height: 24px; padding: 0; border: none; border-radius: 6px; background: transparent; color: var(--muted); font-size: 18px; line-height: 24px; }
.remove:hover { background: var(--line); color: var(--text); }
li { align-items: center; }
.status { display: flex; align-items: center; gap: 8px; margin: 6px 0 0; color: var(--muted); }
.dot { width: 8px; height: 8px; border-radius: 50%; background: var(--line); }
.status.connected { color: var(--text); }
.status.connected .dot { background: #12b76a; }
.failed { margin: auto; max-width: 420px; text-align: center; }
</style>"#;

#[derive(Clone)]
struct Engine(Arc<SenderEngine>);

#[derive(Clone)]
struct StartupError(String);

fn main() {
    let launcher = LaunchBuilder::desktop().with_cfg(
        Config::new()
            .with_window(
                WindowBuilder::new()
                    .with_title("Oxdrop")
                    .with_inner_size(LogicalSize::new(780.0, 520.0))
                    .with_min_inner_size(LogicalSize::new(600.0, 400.0)),
            )
            .with_custom_head(STYLE.to_owned()),
    );
    match SenderEngine::start(EngineConfig::default()) {
        Ok(engine) => launcher.with_context(Engine(Arc::new(engine))).launch(App),
        Err(error) => launcher
            .with_context(StartupError(error.to_string()))
            .launch(StartupFailed),
    }
}

#[component]
fn App() -> Element {
    let Engine(engine) = use_context::<Engine>();
    let mut state = use_signal({
        let engine = engine.clone();
        move || engine.state()
    });
    use_future({
        let engine = engine.clone();
        move || {
            let mut updates = engine.subscribe();
            async move {
                // Catch anything that changed before we subscribed, then follow every change.
                state.set(updates.borrow_and_update().clone());
                while updates.changed().await.is_ok() {
                    state.set(updates.borrow_and_update().clone());
                }
            }
        }
    });
    let mut problem = use_signal(|| None::<String>);
    let mut dragging = use_signal(|| false);
    let link = use_memo(move || state.read().link.to_string());
    let qr = use_memo(move || qr_svg(&link()));

    let mut add = move |paths: Vec<PathBuf>| {
        let errors: Vec<String> = engine
            .add_paths(&paths)
            .into_iter()
            .filter_map(Result::err)
            .map(|error| error.to_string())
            .collect();
        problem.set((!errors.is_empty()).then(|| errors.join("\n")));
    };
    let add_dropped = {
        let mut add = add.clone();
        move |event: DragEvent| {
            event.prevent_default();
            dragging.set(false);
            add(event.files().iter().map(|file| file.path()).collect());
        }
    };
    let add_picked =
        move |event: FormEvent| add(event.files().iter().map(|file| file.path()).collect());

    let items = state.read().items.clone();
    let connected = state.read().receiver_connected;
    rsx! {
        div {
            class: if dragging() { "app dragging" } else { "app" },
            ondragover: move |event| {
                event.prevent_default();
                dragging.set(true);
            },
            ondragleave: move |_| dragging.set(false),
            ondrop: add_dropped,
            section { class: "share",
                div { class: "qr", dangerous_inner_html: "{qr}" }
                p { class: "scan", "Scan with your Android phone" }
                p { class: "link", "{link}" }
                p { class: if connected { "status connected" } else { "status" },
                    span { class: "dot" }
                    if connected { "Phone connected" } else { "Waiting for phone…" }
                }
            }
            section { class: "items",
                div { class: "toolbar",
                    label { class: "button",
                        "Add files…"
                        input {
                            r#type: "file",
                            multiple: true,
                            hidden: true,
                            onchange: add_picked,
                        }
                    }
                }
                if !items.is_empty() {
                    ul {
                        for item in items {
                            ItemRow { key: "{item.id}", item }
                        }
                    }
                }
                div { class: "drop",
                    if state.read().items.is_empty() { "Drop files here to send them to your phone" } else { "Drop more files here" }
                }
                if let Some(problem) = problem() {
                    p { class: "problem", "{problem}" }
                }
            }
        }
    }
}

#[component]
fn ItemRow(item: Item) -> Element {
    let Engine(engine) = use_context::<Engine>();
    let id = item.id;
    rsx! {
        li {
            span { class: "name", title: "{item.path.display()}", "{item.name}" }
            span { class: "size", "{format_size(item.size)}" }
            button {
                class: "remove",
                title: "Remove",
                onclick: move |_| {
                    engine.remove_item(id);
                },
                "×"
            }
        }
    }
}

#[component]
fn StartupFailed() -> Element {
    let StartupError(message) = use_context::<StartupError>();
    rsx! {
        div { class: "app",
            div { class: "failed",
                h2 { "Oxdrop can't start" }
                p { "{message}" }
            }
        }
    }
}

/// The Transfer Link as an SVG QR code: dark on white, so it scans in dark mode too.
fn qr_svg(link: &str) -> String {
    QrCode::new(link.as_bytes())
        .map(|code| {
            code.render::<svg::Color<'_>>()
                .min_dimensions(220, 220)
                .dark_color(svg::Color("#000000"))
                .light_color(svg::Color("#ffffff"))
                .build()
        })
        .unwrap_or_default()
}
