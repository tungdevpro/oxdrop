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

**Oxdrop 0.1.0** is out: [download it from Releases](https://github.com/tungdevpro/oxdrop/releases/latest). It covers the whole v1 plan from [issue #1](https://github.com/tungdevpro/oxdrop/issues/1):

- Several files at once, a phone page that updates live, and a "phone connected" indicator
- Automatic choice of the network interface the phone can reach, with a dropdown to change it
- Downloads that resume, and files marked Changed or Missing if they change on disk after you add them
- Folders, which arrive as one zip that keeps their structure and can be resumed, even with files over 4 GB inside
- "Download all", which fetches the whole Transfer as one resumable zip, so the browser doesn't ask to allow many downloads
- Live progress for each Item on the computer, marked Sent when the phone has all of it or Stopped if a download breaks off, including during "Download all" and resumed downloads
- Ending a Transfer with End Transfer, by closing the window, or after 30 minutes with no activity (with a 2-minute countdown and Keep open). The old QR code then shows "This Transfer has ended", and a fresh one is ready straight away
- A note to use Oxdrop only on networks you trust, a local log for bug reports, and a notice when a newer version is out

Builds are for macOS 12 or later (universal) and Windows 10 21H2 or later (x64 and ARM64). They aren't signed yet, so the first time you open Oxdrop your computer will warn you; see [Installing](#installing).

## Installing

Download the installer for your computer from [Releases](https://github.com/tungdevpro/oxdrop/releases):

- **macOS 12 or later** (Apple Silicon or Intel): open `Oxdrop-<version>-macos-universal.dmg` and drag Oxdrop to Applications.
- **Windows 10 21H2 or later**: run `Oxdrop-<version>-windows-x64-setup.exe`, or the `arm64` one on an ARM PC. It installs for your user account, with no administrator rights needed, and installs Microsoft's WebView2 runtime first if the PC doesn't have it.

Until the builds are signed, your computer will warn you the first time you open Oxdrop:

- **macOS:** right-click Oxdrop in Applications, choose **Open**, then **Open** again. On macOS 15 and later, open **System Settings → Privacy & Security** instead and click **Open Anyway**.
- **Windows:** when SmartScreen says "Windows protected your PC", click **More info**, then **Run anyway**.

## Building and running

You need a recent stable Rust toolchain (1.88 or newer). On Windows, the app also needs the WebView2 runtime, which comes with Windows 11 and recent Windows 10 updates.

```sh
cargo run -p oxdrop-desktop
```

The Sender listens on port **48230**. If that port is busy, it uses one the OS picks and shows it in the Transfer Link. The first time you run it, allow it through the firewall so the phone can connect.

## Logs and privacy

Oxdrop has no telemetry. It keeps a local log, which you can attach to a bug report. Click "Show log folder" in the app to find it:

- macOS: `~/Library/Logs/Oxdrop`
- Windows: `%LOCALAPPDATA%\Oxdrop\logs`

The log rolls over daily, and the last 7 days are kept. If the app crashes, the crash message and backtrace are written there too.

On launch, Oxdrop makes one anonymous request to the GitHub Releases API to see whether a newer version exists. If one does, it shows a notice linking to the release. Apart from the local network, this is the only connection the app makes. If the request fails, the app says nothing.

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

## Releasing

1. Set the new `version` in the root `Cargo.toml` and commit it.
2. Push a matching tag, such as `git tag v0.2.0 && git push origin v0.2.0`.

The [Release workflow](.github/workflows/release.yml) checks that the tag matches the version, builds the macOS `.dmg` and both Windows installers, and attaches them to a **draft** GitHub Release. Install them by hand on macOS and Windows, then publish the draft. The app's update notice only sees published releases.

To build the installers without releasing, run the workflow by hand from the Actions tab. It also runs on pull requests that change `scripts/` or the workflow. To build the macOS `.dmg` locally, run `./scripts/macos/package.sh`. On Windows, run `.\scripts\windows\package.ps1 -Arch x64`, which needs [NSIS](https://nsis.sourceforge.io/) 3.

Signing is optional. Each step runs only when its repository secrets are set; without them, the workflow still succeeds and produces unsigned installers.

| Secret | Used for |
|---|---|
| `MACOS_CERTIFICATE` | The "Developer ID Application" certificate and its private key, as a base64-encoded `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | The `.p12` file's password |
| `MACOS_SIGNING_IDENTITY` | The identity to sign with, such as `Developer ID Application: Name (TEAMID)` |
| `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` | Notarization: the Apple ID, its team ID, and an [app-specific password](https://support.apple.com/102654). Used only when the app is signed. |
| `WINDOWS_CERTIFICATE` | A code-signing certificate and its private key, as a base64-encoded `.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | The `.pfx` file's password |

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
