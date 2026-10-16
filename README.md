# Oxdrop

Send files from your Mac or Windows PC to an Android phone over your own Wi-Fi. Nothing gets installed on the phone and nothing leaves your network.

Drop files onto the Oxdrop window and it shows a QR code. Scan it with your phone, and the phone's browser opens a page listing the files. Tap one to download it. Files go straight from the computer to the phone over the local network. There is no Oxdrop server and no Oxdrop app on the phone.

## How it works

- **The Sender** is the desktop app (macOS or Windows). It runs a small web server on your local network.
- **The Receiver** is the phone's web browser. It opens the **Transfer Link** from the QR code and downloads Items with the browser's own download manager.
- **The Transfer Link** contains a 128-bit secret and is the only access control. Anyone without it gets a plain 404, and requests that name any host other than the Sender's own IP address are refused, which blocks DNS-rebinding attacks.
- Files move one way only: computer to phone.

See [`CONTEXT.md`](CONTEXT.md) for the vocabulary (Sender, Receiver, Transfer, Item, …) and [`docs/adr/`](docs/adr/) for the decisions behind the design.

> **Use Oxdrop only on networks you trust.** Transfers use plain HTTP on the local network ([ADR-0004](docs/adr/0004-plain-http-on-local-network.md)). The Transfer Link keeps other devices out, but someone capturing traffic on the same network could read the files.

## Status

Oxdrop is in development and not yet released. The v1 plan is [issue #1](https://github.com/tungdevpro/oxdrop/issues/1).

Working now:

- Several files at once, a phone page that updates live, and a "phone connected" indicator
- Automatic choice of the network interface the phone can reach, with a dropdown to change it
- Downloads that resume, and files marked Changed or Missing if they change on disk after you add them
- Folders, which arrive as one zip that keeps their structure and can be resumed, even with files over 4 GB inside

Still to come: Download all, per-Item progress, ending a Transfer (including after being idle), and signed installers.

## Building and running

You need a recent stable Rust toolchain (1.88 or newer). On Windows, the app also needs the WebView2 runtime, which comes with Windows 11 and recent Windows 10 updates.

```sh
cargo run -p oxdrop-desktop
```

The Sender listens on port **48230**. If that port is busy, it uses one the OS picks and shows it in the Transfer Link. The first time you run it, allow it through the firewall so the phone can connect.

## Development

```sh
cargo test --workspace      # unit and HTTP tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

CI runs the same checks on macOS and Windows.

| Crate | What it holds |
|---|---|
| `crates/oxdrop-core` | The domain: Transfers, Items, Item Names, Transfer Links, network-interface ranking. No networking or UI. |
| `crates/oxdrop-server` | The Sender engine (`SenderEngine`), the HTTP routes and the Receiver page (server-rendered with Dioxus SSR). |
| `crates/oxdrop-desktop` | The Dioxus desktop window. It only displays the engine's state and passes on the user's actions; it holds no transfer logic. |

The HTTP tests in `crates/oxdrop-server/tests/` start a real engine on loopback and use an HTTP client to play the phone.

Issues are tracked in [GitHub Issues](https://github.com/tungdevpro/oxdrop/issues).

## License

Licensed under either of MIT or Apache-2.0, at your option.
