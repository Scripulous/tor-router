# torproxy

A tiny Tor client for Windows that needs **no admin rights**. It runs Tor using
[Arti](https://gitlab.torproject.org/tpo/core/arti) (the Tor Project's own Rust implementation),
opens a SOCKS5 proxy on `127.0.0.1:9150`, and points your per-user Windows proxy setting at it while
it runs. When you quit, your old proxy settings are put back.

## Using it

1. Run `torproxy.exe` (double-click is fine). The first start takes a minute or two.
2. When it says "SOCKS5 proxy ready", Edge, Chrome and other apps that follow the Windows proxy
   setting will go through Tor.
3. Press Ctrl+C or close the window to stop. Your settings are restored.

Options (run from a terminal):

| Command | What it does |
|---|---|
| `torproxy --port 9050` | Use a different local port |
| `torproxy --no-system-proxy` | Only run the proxy; don't touch Windows settings |
| `torproxy reset` | Restore proxy settings if the program was killed and your internet stopped working |

Firefox ignores the Windows setting by default: Settings > Network Settings > "Use system proxy
settings", or set a SOCKS5 proxy of `127.0.0.1` port `9150` by hand.

## What this is not

- **Not a VPN or a full "Tor router".** Routing all traffic needs a virtual network adapter, which
  requires admin. This is a proxy: only apps that honor the system proxy or a SOCKS5 setting use Tor.
  Apps that ignore it connect directly.
- **Not a replacement for Tor Browser** if you need strong anonymity. Tor Browser also defends
  against browser fingerprinting; this does not.
- Windows SmartScreen may warn about the exe because it isn't code-signed. That is expected.

## Building the exe

**Option A: GitHub Actions (nothing to install).** Create a GitHub repo, upload this folder, open the
Actions tab, run "Build Windows exe", then download `torproxy-windows` from the run. It contains
`torproxy.exe`.

**Option B: locally.** Install Rust from https://rustup.rs and the build tools it asks for, then:

```
cargo build --release
```

The exe is `target\release\torproxy.exe`.

Note: Arti is under active development and its API changes between releases. `Cargo.toml` accepts
any 0.x version of `arti-client`. If a future release breaks the build, use the `Cargo.lock` from a
build that worked, or pin `arti-client` to that version.
