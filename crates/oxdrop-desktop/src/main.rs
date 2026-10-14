//! The Sender window: a thin shell that renders the Sender engine's state and
//! forwards the user's actions to it. No transfer logic lives here.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::sync::Arc;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use oxdrop_core::format_size;
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
    let mut problem = use_signal(|| None::<String>);
    let mut dragging = use_signal(|| false);
    let link = use_memo(move || state.read().link.to_string());
    let qr = use_memo(move || qr_svg(&link()));

    let add_dropped = move |event: DragEvent| {
        event.prevent_default();
        dragging.set(false);
        let errors: Vec<String> = event
            .files()
            .iter()
            .filter_map(|file| engine.add_path(&file.path()).err())
            .map(|error| error.to_string())
            .collect();
        problem.set((!errors.is_empty()).then(|| errors.join("\n")));
        state.set(engine.state());
    };

    let items = state.read().items.clone();
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
            }
            section { class: "items",
                if !items.is_empty() {
                    ul {
                        for item in items {
                            li { key: "{item.id}",
                                span { class: "name", title: "{item.path.display()}", "{item.name}" }
                                span { class: "size", "{format_size(item.size)}" }
                            }
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
