//! grazel — the gryth node.
//!
//! A glade APPLICATION above the glade kernel: it composes glade suppliers
//! (none yet — P1), owns app storage (`--data DIR/{sys,files,config}`), and
//! serves gryth-ui + the session-placement bootstrap.
//!
//! Composition posture = WIRE ATTACHMENT (GLP-0006 P00-a): grazel SPAWNS the
//! glade node as a subprocess rather than embedding it. Embedding-as-a-crate
//! (loopback attach) is a later optimization; process composition is the
//! legitimate skeleton. grazel supervises the node one-directionally: the node
//! exiting is fatal — grazel exits nonzero with the tail of the node's stderr.
//! Conversely, a SIGINT/SIGTERM to grazel tears the node child down (clean
//! shutdown).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use grazel::{bootstrap_json, ensure_data_layout, read_static, Config};

/// PID of the spawned glade node, for the signal handler to tear down.
static NODE_PID: AtomicI32 = AtomicI32::new(0);
/// PID of the composed gwz supplier child (0 = none), torn down on shutdown.
static GWZ_PID: AtomicI32 = AtomicI32::new(0);

fn main() {
    let cfg = match Config::parse(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(msg) => {
            // --help returns the usage as an "error"; print to stdout, exit 0.
            let help = msg.starts_with("grazel");
            if help {
                println!("{msg}");
            } else {
                eprintln!("{msg}");
            }
            std::process::exit(if help { 0 } else { 2 });
        }
    };

    if let Err(e) = ensure_data_layout(&cfg.data) {
        eprintln!("[grazel] cannot create data layout under {}: {e}", cfg.data.display());
        std::process::exit(2);
    }

    install_signal_handlers();

    // ---- spawn the glade node (booted profile form) ------------------------
    let glade_home = cfg.glade_home();
    println!(
        "[grazel] mode={} node-profile={} GLADE_HOME={} node-bin={}",
        cfg.mode.as_str(),
        cfg.mode.node_profile(),
        glade_home.display(),
        cfg.node_bin.display()
    );
    let mut child = match Command::new(&cfg.node_bin)
        .args(cfg.node_argv())
        .env("GLADE_HOME", &glade_home) // NEVER the real ~/.glade
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[grazel] failed to spawn node {}: {e}", cfg.node_bin.display());
            std::process::exit(2);
        }
    };
    NODE_PID.store(child.id() as i32, Ordering::SeqCst);

    // Drain node stdout: forward it, and capture the actual listening port.
    let (port_tx, port_rx) = mpsc::channel::<Option<u16>>();
    let stdout = child.stdout.take().expect("node stdout piped");
    thread::spawn(move || {
        let mut sent = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            println!("[node] {line}");
            if !sent {
                if let Some(p) = line.strip_prefix("listening ").and_then(|r| r.trim().parse().ok()) {
                    let _ = port_tx.send(Some(p));
                    sent = true;
                }
            }
        }
        if !sent {
            let _ = port_tx.send(None); // node closed stdout before ever listening
        }
    });

    // Drain node stderr: forward it, and keep the last ~20 lines for the tail.
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let stderr = child.stderr.take().expect("node stderr piped");
    let tail_w = tail.clone();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            eprintln!("[node] {line}");
            let mut t = tail_w.lock().unwrap();
            t.push_back(line);
            while t.len() > 20 {
                t.pop_front();
            }
        }
    });

    // Wait for the node's actual listening port (or an early exit).
    let node_port = match port_rx.recv() {
        Ok(Some(p)) => p,
        _ => {
            let _ = child.wait();
            eprintln!("[grazel] node exited before it started listening.\n{}", stderr_tail(&tail));
            std::process::exit(1);
        }
    };
    println!("[grazel] node listening on ws://127.0.0.1:{node_port}");

    // ---- compose suppliers: spawn glade-gwz as a CHILD supplier ------------
    // Wire-attachment composition (P00-a): the supplier attaches over the node's
    // WS carrier as an ordinary authority session — grazel just SPAWNS it and
    // supervises it non-fatally (a supplier is OPTIONAL; its exit never stops
    // grazel). The chat supplier is TS/in-process in the UI host, not run here;
    // its surfaces are pre-declared in grazel-app.glade instead.
    spawn_gwz_supplier(&cfg, node_port);

    // ---- serve HTTP (ui + /bootstrap.json) on a worker thread --------------
    let boot = bootstrap_json(node_port, cfg.mode.as_str(), &cfg.name);
    let ui = cfg.ui.clone();
    let http_port = cfg.http_port;
    thread::spawn(move || {
        if let Err(e) = serve_http(http_port, &ui, &boot) {
            eprintln!("[grazel] http server error: {e}");
            std::process::exit(2);
        }
    });

    // ---- supervise: node exit is fatal -------------------------------------
    let status = child.wait().expect("wait on node");
    eprintln!("[grazel] node exited: {status}\n{}", stderr_tail(&tail));
    // The node is gone; tear the composed supplier down too (it would otherwise
    // spin reattaching to a dead node). A supplier exit is NOT fatal, but a node
    // exit is — so bring the whole tree down.
    kill_supplier();
    // Node should never exit while grazel runs -> always nonzero.
    std::process::exit(status.code().filter(|c| *c != 0).unwrap_or(1));
}

/// Spawn `glade-gwz` as a child supplier attached to the node's WS carrier.
/// OPTIONAL: `--no-suppliers` disables it, and an ABSENT binary is a loud SKIP
/// (never fatal — a supplier's absence must not stop grazel), mirroring the
/// node-binary handling. Supervised non-fatally: its stdout/stderr forward with
/// a `[gwz]` prefix and its exit only logs (grazel keeps running — the surface
/// simply has no live provider until it is restarted).
fn spawn_gwz_supplier(cfg: &Config, node_ws_port: u16) {
    if cfg.no_suppliers {
        println!("[grazel] suppliers disabled (--no-suppliers); node only");
        return;
    }
    if !cfg.gwz_supplier_bin.exists() {
        println!(
            "[grazel] SKIP gwz supplier: binary not found at {} \
             (build ../glade-gwz or pass --gwz-supplier-bin; a supplier is optional)",
            cfg.gwz_supplier_bin.display()
        );
        return;
    }
    let argv = cfg.gwz_supplier_argv(node_ws_port);
    println!("[grazel] spawning gwz supplier: {} {}", cfg.gwz_supplier_bin.display(), argv.join(" "));
    let mut child = match Command::new(&cfg.gwz_supplier_bin)
        .args(&argv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            // Spawn failure is non-fatal too — log and carry on without it.
            eprintln!("[grazel] gwz supplier failed to spawn ({e}); continuing without it");
            return;
        }
    };
    GWZ_PID.store(child.id() as i32, Ordering::SeqCst);

    if let Some(out) = child.stdout.take() {
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                println!("[gwz] {line}");
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                eprintln!("[gwz] {line}");
            }
        });
    }

    // Supervise non-fatally: reap the child and log its exit; grazel lives on.
    thread::spawn(move || {
        let status = child.wait();
        GWZ_PID.store(0, Ordering::SeqCst);
        match status {
            Ok(s) => eprintln!("[grazel] gwz supplier exited: {s} (non-fatal; surface has no provider until respawn)"),
            Err(e) => eprintln!("[grazel] gwz supplier wait failed: {e}"),
        }
    });
}

/// Tear the gwz supplier child down (SIGTERM) if one is running. Async-signal
/// unsafe (it loads + branches), so it is called only from the normal exit path;
/// the signal handler uses the raw async-safe `kill` directly.
fn kill_supplier() {
    let pid = GWZ_PID.swap(0, Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

fn stderr_tail(tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    let t = tail.lock().unwrap();
    if t.is_empty() {
        "[grazel] (node stderr was empty)".to_string()
    } else {
        format!("[grazel] node stderr tail:\n{}", t.iter().cloned().collect::<Vec<_>>().join("\n"))
    }
}

/// Serve static files from `ui` plus `GET /bootstrap.json`. Blocking; tiny_http
/// owns its own thread pool internally.
fn serve_http(port: u16, ui: &Path, bootstrap: &str) -> std::io::Result<()> {
    let server = tiny_http::Server::http(("127.0.0.1", port))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    println!("[grazel] http on http://127.0.0.1:{port} (ui {})", ui.display());
    for req in server.incoming_requests() {
        let path = req.url().split('?').next().unwrap_or("/").to_string();
        let resp = if path == "/bootstrap.json" {
            tiny_http::Response::from_string(bootstrap.to_string())
                .with_header(header("Content-Type", "application/json"))
                .boxed()
        } else {
            match read_static(ui, &path) {
                Some((bytes, ctype)) => {
                    tiny_http::Response::from_data(bytes).with_header(header("Content-Type", ctype)).boxed()
                }
                None => tiny_http::Response::from_string("not found").with_status_code(404).boxed(),
            }
        };
        let _ = req.respond(resp);
    }
    Ok(())
}

fn header(k: &str, v: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("valid header")
}

/// SIGINT/SIGTERM -> kill the node child, then exit. `kill` and `_exit` are
/// async-signal-safe, which is all the handler does.
fn install_signal_handlers() {
    let handler = on_signal as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

extern "C" fn on_signal(_sig: libc::c_int) {
    // Tear down BOTH children (node + composed supplier). `kill` is
    // async-signal-safe; the two atomic loads are lock-free integer reads.
    let node = NODE_PID.load(Ordering::SeqCst);
    let gwz = GWZ_PID.load(Ordering::SeqCst);
    unsafe {
        if node > 0 {
            libc::kill(node, libc::SIGTERM);
        }
        if gwz > 0 {
            libc::kill(gwz, libc::SIGTERM);
        }
        libc::_exit(130);
    }
}
