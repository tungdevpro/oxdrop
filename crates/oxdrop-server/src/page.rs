//! The Receiver page: server-rendered HTML that works in any Android browser.

use dioxus::prelude::*;
use oxdrop_core::{Item, format_size};

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
.empty { color: var(--muted); padding: 32px 0; text-align: center; }
</style>
</head>
"#;

/// Renders the full page for the open Transfer's Items.
pub(crate) fn render(items: &[Item]) -> String {
    let body = dioxus_ssr::render_element(rsx! {
        main {
            h1 { "Oxdrop" }
            if items.is_empty() {
                p { class: "empty", "Nothing here yet. Add files on your computer." }
            } else {
                p { class: "hint", "Tap a file to download it." }
                ul {
                    for item in items {
                        li { key: "{item.id}",
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
    format!("{HEAD}<body>{body}</body>\n</html>\n")
}
