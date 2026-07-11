//! End-to-end: build glade-node, boot grazel in `both` mode with temp dirs,
//! and prove the skeleton's three seams — `/bootstrap.json`, a static file,
//! and the node WS port — then a clean shutdown.
//!
//! NEVER touches the real `~/.glade`: grazel runs the node under
//! `GLADE_HOME=<data>/sys`, and this test also pins `HOME` to a temp dir.
//!
//! Another agent may be committing to `../glade/node` in parallel, so the node
//! build is attempted ONCE, retried ONCE if transiently broken, then the test
//! SKIPs with a loud marker rather than failing.
//!
//! A SECOND test (`grazel_both_mode_composes_gwz_supplier`, P1.S3) boots grazel
//! WITH the composed glade-gwz supplier and PROBES the node over the wire — a
//! real `gwz.status` round-trips, and a disallowed verb comes back as data. It
//! needs both the node AND the glade-gwz binary built, plus a real `gwz` on the
//! box to `gwz init` the app-owned workspace; any missing piece is a loud SKIP.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn node_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("glade").join("node")
}

fn node_bin() -> PathBuf {
    node_dir().join("target").join("debug").join("glade-node")
}

/// Build glade-node; returns false if it should be SKIPped (build broken twice).
fn build_node_or_skip() -> bool {
    for attempt in 1..=2 {
        let ok = Command::new("cargo")
            .args(["build", "--offline", "--bin", "glade-node"])
            .current_dir(node_dir())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok && node_bin().exists() {
            return true;
        }
        eprintln!("[test] glade-node build attempt {attempt} failed");
    }
    eprintln!("\n================ SKIP: grazel integration test ================");
    eprintln!("  glade-node failed to build twice (parallel edits in ../glade/node?).");
    eprintln!("  Skipping the boot test rather than failing. Re-run once the node builds.");
    eprintln!("==============================================================\n");
    false
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Minimal HTTP/1.1 GET; `None` on connect/read failure. Returns (status, body).
fn http_get(port: u16, path: &str) -> Option<(u16, Vec<u8>)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).ok()?;
    let sep = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = &buf[..sep];
    let body = buf[sep + 4..].to_vec();
    let first = head.split(|&b| b == b'\n').next()?;
    let first = String::from_utf8_lossy(first);
    let status = first.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, body))
}

/// Poll `GET path` until it returns 200 or the deadline passes.
fn wait_for_200(port: u16, path: &str, secs: u64) -> Option<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Some((200, body)) = http_get(port, path) {
            return Some(body);
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Extract the `ws://127.0.0.1:<port>` port from a bootstrap.json body.
fn node_ws_port(bootstrap: &str) -> Option<u16> {
    let marker = "ws://127.0.0.1:";
    let start = bootstrap.find(marker)? + marker.len();
    let digits: String = bootstrap[start..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[test]
fn grazel_both_mode_serves_bootstrap_static_and_node() {
    if !build_node_or_skip() {
        return; // loud SKIP already printed
    }

    // ---- temp workspace (data + ui + a fake HOME) --------------------------
    let base = std::env::temp_dir().join(format!("grazel-it-{}-{}", std::process::id(), free_port()));
    let data = base.join("data");
    let ui = base.join("ui");
    let home = base.join("home");
    std::fs::create_dir_all(&ui).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(ui.join("index.html"), b"<h1>grazel it</h1>").unwrap();
    std::fs::write(ui.join("app.js"), b"// grazel static probe\n").unwrap();
    let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("apps").join("grazel-app.glade");

    let http_port = free_port();

    // ---- boot grazel in `both` mode (node-port 0 = OS-assigned) ------------
    let mut grazel = Command::new(env!("CARGO_BIN_EXE_grazel"))
        .args([
            "--mode", "both",
            "--name", "grazel-it",
            "--data", data.to_str().unwrap(),
            "--ui", ui.to_str().unwrap(),
            "--http", &http_port.to_string(),
            "--node-port", "0",
            "--node-bin", node_bin().to_str().unwrap(),
            "--app", app.to_str().unwrap(),
        ])
        .env("HOME", &home) // belt-and-suspenders: never the real ~/.glade
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn grazel");

    // ---- assert: /bootstrap.json served ------------------------------------
    let boot_body = wait_for_200(http_port, "/bootstrap.json", 30)
        .expect("grazel should serve /bootstrap.json within 30s");
    let boot = String::from_utf8(boot_body).unwrap();
    assert!(boot.contains("\"mode\":\"both\""), "bootstrap mode: {boot}");
    assert!(boot.contains("\"name\":\"grazel-it\""), "bootstrap name: {boot}");
    let ws_port = node_ws_port(&boot).expect("bootstrap carries a node ws port");
    assert!(ws_port > 0, "node ws port should be OS-assigned nonzero: {boot}");

    // ---- assert: a static file served --------------------------------------
    let (status, body) = http_get(http_port, "/app.js").expect("GET /app.js");
    assert_eq!(status, 200, "static file status");
    assert_eq!(body, b"// grazel static probe\n", "static file body");

    // ---- assert: the node WS port accepts a TCP connection -----------------
    let mut connected = false;
    for _ in 0..30 {
        if TcpStream::connect(("127.0.0.1", ws_port)).is_ok() {
            connected = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(connected, "node ws port {ws_port} should accept a TCP connection");

    // ---- assert: isolation — the node instance lives UNDER our temp data ---
    let instance = data.join("sys").join("sys").join("grazel-it");
    assert!(instance.join("node.key").exists(), "node booted under {}, not ~/.glade", instance.display());

    // ---- clean shutdown: SIGTERM grazel -> node torn down ------------------
    unsafe {
        libc::kill(grazel.id() as i32, libc::SIGTERM);
    }
    let exited = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match grazel.try_wait().unwrap() {
                Some(_) => break true,
                None if Instant::now() >= deadline => break false,
                None => thread::sleep(Duration::from_millis(100)),
            }
        }
    };
    assert!(exited, "grazel should exit on SIGTERM");

    // node + http ports should stop accepting once grazel tore down.
    let mut node_down = false;
    for _ in 0..30 {
        if TcpStream::connect(("127.0.0.1", ws_port)).is_err() {
            node_down = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(node_down, "node ws port {ws_port} should stop accepting after clean shutdown");
    assert!(http_get(http_port, "/bootstrap.json").is_none(), "http port should be closed after shutdown");

    let _ = grazel.wait();
    std::fs::remove_dir_all(&base).ok();
}

// ---- P1.S3: grazel composes the glade-gwz supplier -------------------------

fn glade_gwz_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("glade-gwz")
}
fn glade_gwz_bin() -> PathBuf {
    glade_gwz_dir().join("target").join("debug").join("glade-gwz")
}

/// Build glade-gwz; false => SKIP (build broken twice — a parallel edit, say).
fn build_gwz_or_skip() -> bool {
    for attempt in 1..=2 {
        let ok = Command::new("cargo")
            .args(["build", "--offline", "--bin", "glade-gwz"])
            .current_dir(glade_gwz_dir())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok && glade_gwz_bin().exists() {
            return true;
        }
        eprintln!("[test] glade-gwz build attempt {attempt} failed");
    }
    eprintln!("\n=========== SKIP: grazel gwz-composition test ============");
    eprintln!("  glade-gwz failed to build twice — skipping the composition test.");
    eprintln!("=========================================================\n");
    false
}

/// The real `gwz` binary (prefer the cargo-bin install, else PATH). `None` if it
/// looks absent — the composition test SKIPs (it needs `gwz init`).
fn real_gwz() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".cargo/bin/gwz");
        if p.exists() {
            return Some(p);
        }
    }
    // `gwz --version` on PATH: succeeds => installed.
    let ok = Command::new("gwz")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then(|| PathBuf::from("gwz"))
}

/// `gwz --root <dir> init` — the app-owned workspace the composed supplier
/// serves against (grazel points `--root` at `<data>/files`).
fn gwz_init(gwz: &Path, dir: &Path) -> bool {
    std::fs::create_dir_all(dir).ok();
    Command::new(gwz)
        .arg("--root")
        .arg(dir)
        .arg("init")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
async fn grazel_both_mode_composes_gwz_supplier() {
    if !build_node_or_skip() || !build_gwz_or_skip() {
        return; // loud SKIP already printed
    }
    let Some(gwz) = real_gwz() else {
        eprintln!("\n=========== SKIP: grazel gwz-composition test ============");
        eprintln!("  `gwz` is not installed — cannot init the app-owned workspace.");
        eprintln!("=========================================================\n");
        return;
    };

    // ---- temp workspace (data + ui + a fake HOME) --------------------------
    let base = std::env::temp_dir().join(format!("grazel-gwz-it-{}-{}", std::process::id(), free_port()));
    let data = base.join("data");
    let ui = base.join("ui");
    let home = base.join("home");
    std::fs::create_dir_all(&ui).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(ui.join("index.html"), b"<h1>grazel gwz it</h1>").unwrap();
    let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("apps").join("grazel-app.glade");

    // The supplier's root is `<data>/files` (grazel's argv). gwz-init it BEFORE
    // grazel spawns the supplier so a real `gwz status` has a workspace to read.
    let files = data.join("files");
    assert!(gwz_init(&gwz, &files), "gwz init the app-owned workspace");

    let http_port = free_port();

    // ---- boot grazel (both) with the composed gwz supplier -----------------
    let mut grazel = Command::new(env!("CARGO_BIN_EXE_grazel"))
        .args([
            "--mode", "both",
            "--name", "grazel-gwz-it",
            "--data", data.to_str().unwrap(),
            "--ui", ui.to_str().unwrap(),
            "--http", &http_port.to_string(),
            "--node-port", "0",
            "--node-bin", node_bin().to_str().unwrap(),
            "--app", app.to_str().unwrap(),
            "--gwz-supplier-bin", glade_gwz_bin().to_str().unwrap(),
        ])
        .env("HOME", &home)
        .env("PATH", std::env::var("PATH").unwrap_or_default()) // gwz resolves on PATH inside the supplier
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn grazel");

    // the node ws port comes from /bootstrap.json.
    let boot = String::from_utf8(
        wait_for_200(http_port, "/bootstrap.json", 30).expect("grazel serves /bootstrap.json"),
    )
    .unwrap();
    let ws_port = node_ws_port(&boot).expect("bootstrap carries a node ws port");

    // ---- probe: a real gwz.status round-trips through the composed supplier -
    let requester = glade_client::GladeClient::new("grazel-it-probe");
    requester
        .connect(&format!("ws://127.0.0.1:{ws_port}"))
        .await
        .expect("probe connects to the node");

    // poll until the supplier has attached AND the workspace claim routes.
    let status = poll_exchange(&requester, br#"{"verb":"status"}"#, |p| {
        p.contains("\"exit\":0") && p.contains("\"attributed_to\":\"grazel\"")
    })
    .await;
    assert!(
        status.is_some(),
        "the composed gwz supplier attached and answered gwz.status (exit 0, attributed to grazel)"
    );

    // a disallowed verb comes back as DATA (failure honestly rendered), never a hang.
    let denied = poll_exchange(&requester, br#"{"verb":"commit","args":["-m","x"]}"#, |p| {
        p.contains("\"ok\":false") && p.contains("allow-list")
    })
    .await;
    assert!(denied.is_some(), "a disallowed verb is refused as data (ok:false, allow-list)");

    requester.close().await;

    // ---- clean shutdown: SIGTERM grazel tears down node + supplier ----------
    unsafe {
        libc::kill(grazel.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let exited = loop {
        match grazel.try_wait().unwrap() {
            Some(_) => break true,
            None if Instant::now() >= deadline => break false,
            None => thread::sleep(Duration::from_millis(100)),
        }
    };
    assert!(exited, "grazel exits on SIGTERM");
    let node_down = (0..30).any(|_| {
        thread::sleep(Duration::from_millis(100));
        TcpStream::connect(("127.0.0.1", ws_port)).is_err()
    });
    assert!(node_down, "node ws port {ws_port} stops accepting after shutdown");

    let _ = grazel.wait();
    std::fs::remove_dir_all(&base).ok();
}

/// Poll `gwz.ops` with `envelope` until the decoded payload string satisfies
/// `want` (or a ~15s deadline). Returns the matching payload string.
async fn poll_exchange(
    client: &glade_client::GladeClient,
    envelope: &[u8],
    want: impl Fn(&str) -> bool,
) -> Option<String> {
    for _ in 0..150 {
        if let Ok(out) = client.exchange("ws-razel", "gwz.ops", envelope.to_vec()).await {
            if out.ok {
                if let Some(payload) = out.payload {
                    let s = String::from_utf8_lossy(&payload).into_owned();
                    if want(&s) {
                        return Some(s);
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}
