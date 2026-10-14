# The Receiver is the phone's web browser, not an Android app

The Sender serves a web page over the local network and the phone downloads Items with its own browser; there is no Oxdrop Android app. We chose zero-install on the phone over what a native app (e.g. Dioxus mobile) would give us.

## Consequences

- No pairing: a browser has no stable identity and the Sender's local IP changes, so access is granted per Transfer by the Transfer Link (QR code).
- Folders cannot be recreated by a browser, so folder Items download as uncompressed zip archives; "Download all" bundles the whole Transfer the same way.
- No device discovery, no background service on the phone; the browser's download manager owns saving and resuming.
- The page is server-rendered HTML (Dioxus SSR) with minimal JavaScript, not a WASM app, so it loads instantly in any Android browser.
