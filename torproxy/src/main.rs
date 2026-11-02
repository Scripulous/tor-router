//! torproxy: a tiny Tor client with a local SOCKS5 proxy.
//!
//! Runs entirely as the current user (no admin rights, no driver, no service).
//! It bootstraps Tor with Arti, listens on 127.0.0.1 only, and (on Windows) points the
//! per-user system proxy at itself while it runs, restoring your old settings on exit.

mod sysproxy;

use std::future::Future;
use std::io::Write as _;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use arti_client::config::TorClientConfigBuilder;
use arti_client::TorClient;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_util::compat::FuturesAsyncReadCompatExt;

const DEFAULT_PORT: u16 = 9150;

const HELP: &str = "\
torproxy - tiny Tor client, no admin needed

USAGE:
    torproxy [OPTIONS]
    torproxy reset

OPTIONS:
    --port <N>           Local SOCKS5 port on 127.0.0.1 (default 9150)
    --no-system-proxy    Only run the SOCKS5 proxy; don't touch Windows proxy settings
    -h, --help           Show this help

COMMANDS:
    reset                Undo the Windows proxy setting (use if the program was killed
                         and your internet stopped working)
";

/// Where torproxy keeps its Tor state, cache and the proxy-settings backup.
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(p).join("torproxy");
    }
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h)
            .join(".local")
            .join("share")
            .join("torproxy");
    }
    PathBuf::from("torproxy-data")
}

#[tokio::main]
async fn main() {
    // rustls needs a crypto backend chosen explicitly; without this Tor start-up panics.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let result = run().await;
    if let Err(e) = &result {
        eprintln!("\nError: {e:#}");
        // Keep the window open so a double-clicked exe doesn't vanish before you can read this.
        eprintln!("\nPress Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut port = DEFAULT_PORT;
    let mut set_system_proxy = true;
    let mut reset = false;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "reset" | "--reset" => reset = true,
            "--no-system-proxy" => set_system_proxy = false,
            "--port" => {
                port = it
                    .next()
                    .context("--port needs a number")?
                    .parse()
                    .context("--port must be a number between 1 and 65535")?;
            }
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            other => bail!("unknown argument '{other}'\n\n{HELP}"),
        }
    }

    if reset {
        sysproxy::restore()?;
        println!("Windows proxy settings restored.");
        return Ok(());
    }

    println!("torproxy: connecting to the Tor network.");
    println!("The first run can take a minute or two while Tor downloads its directory.");

    let data = data_dir();
    let config = TorClientConfigBuilder::from_directories(data.join("state"), data.join("cache"))
        .build()
        .context("invalid Tor configuration")?;

    // Print a dot every few seconds so it's clear we're alive while bootstrapping.
    let ticker = tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            print!(".");
            let _ = std::io::stdout().flush();
        }
    });
    let tor = TorClient::create_bootstrapped(config).await;
    ticker.abort();
    let tor =
        tor.context("could not connect to the Tor network (is your network blocking Tor?)")?;
    println!("\nConnected to Tor.");

    // Loopback only: nobody else on your network can use this proxy.
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| {
            format!("could not listen on 127.0.0.1:{port} (is it already in use? try --port)")
        })?;

    if set_system_proxy {
        sysproxy::enable(port)?;
    }

    println!("SOCKS5 proxy ready on 127.0.0.1:{port}");
    if set_system_proxy && cfg!(windows) {
        println!("Windows proxy is set for your user (Edge, Chrome and other proxy-aware apps will use Tor).");
        println!("Firefox: Settings > Network Settings > 'Use system proxy settings'.");
    }
    println!("Press Ctrl+C (or close this window) to stop and restore your settings.");

    let accept_loop = async {
        loop {
            match listener.accept().await {
                Ok((sock, _)) => {
                    let tor = tor.clone();
                    tokio::spawn(async move {
                        // Errors here are per-connection (client hung up, site unreachable...).
                        let _ = socks5(sock, move |host, port| async move {
                            let stream = tor.connect((host.as_str(), port)).await?;
                            Ok(stream.compat())
                        })
                        .await;
                    });
                }
                Err(e) => {
                    eprintln!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    };

    tokio::select! {
        _ = accept_loop => {}
        _ = shutdown_signal() => {}
    }

    println!("\nShutting down...");
    if set_system_proxy {
        sysproxy::restore()?;
        println!("Windows proxy settings restored.");
    }
    Ok(())
}

/// Resolves on Ctrl+C, and on Windows also when the console window is closed or the
/// user logs off / shuts down.
async fn shutdown_signal() {
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_close, ctrl_shutdown};
        let mut close = ctrl_close().expect("could not register close handler");
        let mut shutdown = ctrl_shutdown().expect("could not register shutdown handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = close.recv() => {}
            _ = shutdown.recv() => {}
        }
    }
    #[cfg(not(windows))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Minimal SOCKS5 server: no auth, CONNECT only. Hostnames are passed to Tor unresolved,
/// so DNS lookups happen inside Tor and don't leak.
async fn socks5<F, Fut, S>(mut client: TcpStream, connect: F) -> Result<()>
where
    F: FnOnce(String, u16) -> Fut,
    Fut: Future<Output = Result<S>>,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (host, port) = timeout(Duration::from_secs(30), handshake(&mut client))
        .await
        .context("SOCKS handshake timed out")??;

    match connect(host, port).await {
        Ok(mut upstream) => {
            client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            Ok(())
        }
        Err(e) => {
            // 0x04 = host unreachable
            let _ = client.write_all(&[5, 4, 0, 1, 0, 0, 0, 0, 0, 0]).await;
            Err(e)
        }
    }
}

async fn handshake(c: &mut TcpStream) -> Result<(String, u16)> {
    let mut head = [0u8; 2];
    c.read_exact(&mut head).await?;
    if head[0] != 5 {
        bail!("not a SOCKS5 client");
    }
    let mut methods = vec![0u8; head[1] as usize];
    c.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        c.write_all(&[5, 0xFF]).await?;
        bail!("client requires authentication, which is not supported");
    }
    c.write_all(&[5, 0]).await?;

    let mut req = [0u8; 4];
    c.read_exact(&mut req).await?;
    if req[0] != 5 {
        bail!("bad SOCKS request");
    }
    if req[1] != 1 {
        // 0x07 = command not supported
        c.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        bail!("only CONNECT is supported");
    }

    let host = match req[3] {
        1 => {
            let mut b = [0u8; 4];
            c.read_exact(&mut b).await?;
            Ipv4Addr::from(b).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            c.read_exact(&mut len).await?;
            let mut b = vec![0u8; len[0] as usize];
            c.read_exact(&mut b).await?;
            String::from_utf8(b).context("hostname is not valid UTF-8")?
        }
        4 => {
            let mut b = [0u8; 16];
            c.read_exact(&mut b).await?;
            Ipv6Addr::from(b).to_string()
        }
        _ => {
            // 0x08 = address type not supported
            c.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            bail!("unsupported address type");
        }
    };

    let mut p = [0u8; 2];
    c.read_exact(&mut p).await?;
    Ok((host, u16::from_be_bytes(p)))
}
