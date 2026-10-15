//! The Receiver page: server-rendered HTML that works in any Android browser.

use dioxus::prelude::*;
use oxdrop_core::{Item, ItemStatus, format_size};

const HEAD: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>Oxdrop</title>
<style>
:root { --bg: #f6f7f9; --card: #ffffff; --text: #16181d; --muted: #646b78; --accent: #2457d6; --line: #e2e5ea; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #111317; --card: #1b1e24; --text: #eceef2; --muted: #9aa1ad; --accent: #7aa2ff; --line: #2b2f37; }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--text); font: 16px/1.4 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }
main { max-width: 560px; margin: 0 auto; padding: 24px 16px 48px; }
h1 { font-size: 22px; margin: 0 0 4px; }
.hint { color: var(--muted); margin: 0 0 20px; }
ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 10px; }
.item { display: flex; align-items: center; gap: 12px; padding: 14px 16px; background: var(--card); border: 1px solid var(--line); border-radius: 12px; color: inherit; text-decoration: none; }
.item:active { border-color: var(--accent); }
.name { flex: 1; min-width: 0; overflow-wrap: anywhere; font-weight: 500; }
.size { color: var(--muted); font-size: 14px; white-space: nowrap; }
.get { color: var(--accent); font-weight: 600; font-size: 14px; }
.unavailable { flex-wrap: wrap; opacity: .6; }
.note { flex-basis: 100%; color: var(--muted); font-size: 14px; }
.empty { color: var(--muted); padding: 32px 0; text-align: center; }
</style>
</head>
"#;

/// Keeps the list in sync with the Sender. Download links work without it.
const SCRIPT: &str = r#"<script>
(() => {
  const list = document.getElementById("items");
  const status = document.getElementById("status");
  const span = (cls, text) => {
    const el = document.createElement("span");
    el.className = cls;
    el.textContent = text;
    return el;
  };
  const events = new EventSource("events");
  events.addEventListener("transfer", (message) => {
    const { items } = JSON.parse(message.data);
    list.replaceChildren(...items.map((item) => {
      let row;
      if (item.note) {
        row = document.createElement("div");
        row.className = "item unavailable";
        row.append(span("name", item.name), span("size", item.size), span("note", item.note));
      } else {
        row = document.createElement("a");
        row.className = "item";
        row.href = "items/" + item.id;
        row.download = item.name;
        row.append(span("name", item.name), span("size", item.size), span("get", "Download"));
      }
      const li = document.createElement("li");
      li.append(row);
      return li;
    }));
    status.className = items.length ? "hint" : "empty";
    status.textContent = items.length ? __HINT__ : __EMPTY__;
  });
})();
</script>"#;

const HINT: &str = "Tap a file to download it.";
const EMPTY: &str = "Nothing here yet. Add files on your computer.";

/// What the Receiver sees of the Transfer; sent as JSON in `transfer` events.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ReceiverView {
    items: Vec<ReceiverItem>,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct ReceiverItem {
    id: String,
    name: String,
    size: String,
    /// `available`, `changed` or `missing`.
    status: &'static str,
    /// Why the Item can't be downloaded right now, if it can't.
    note: Option<&'static str>,
}

fn status_name(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Available => "available",
        ItemStatus::Changed => "changed",
        ItemStatus::Missing => "missing",
    }
}

/// What the Receiver is told about an Item it can't download.
fn unavailable_note(status: ItemStatus) -> Option<&'static str> {
    match status {
        ItemStatus::Available => None,
        ItemStatus::Changed => Some("Changed on the computer. Ask the sender to refresh it."),
        ItemStatus::Missing => Some("No longer on the computer."),
    }
}

impl ReceiverView {
    pub(crate) fn new(items: &[Item]) -> Self {
        Self {
            items: items
                .iter()
                .map(|item| ReceiverItem {
                    id: item.id.to_string(),
                    name: item.name.clone(),
                    size: format_size(item.size),
                    status: status_name(item.status),
                    note: unavailable_note(item.status),
                })
                .collect(),
        }
    }
}

/// Renders the full page for the open Transfer's Items.
pub(crate) fn render(items: &[Item]) -> String {
    let body = dioxus_ssr::render_element(rsx! {
        main {
            h1 { "Oxdrop" }
            if items.is_empty() {
                p { id: "status", class: "empty", "{EMPTY}" }
            } else {
                p { id: "status", class: "hint", "{HINT}" }
            }
            ul { id: "items",
                for item in items {
                    li { key: "{item.id}",
                        if let Some(note) = unavailable_note(item.status) {
                            div { class: "item unavailable",
                                span { class: "name", "{item.name}" }
                                span { class: "size", "{format_size(item.size)}" }
                                span { class: "note", "{note}" }
                            }
                        } else {
                            a { class: "item", href: "items/{item.id}", download: "{item.name}",
                                span { class: "name", "{item.name}" }
                                span { class: "size", "{format_size(item.size)}" }
                                span { class: "get", "Download" }
                            }
                        }
                    }
                }
            }
        }
    });
    let js_string = |text: &str| serde_json::to_string(text).expect("strings serialize");
    let script = SCRIPT
        .replace("__HINT__", &js_string(HINT))
        .replace("__EMPTY__", &js_string(EMPTY));
    format!("{HEAD}<body>{body}{script}</body>\n</html>\n")
}
