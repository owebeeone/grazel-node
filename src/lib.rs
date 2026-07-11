//! grazel — the gryth node's pure config/mapping logic.
//!
//! grazel is a glade APPLICATION (GDL-037): it composes glade suppliers, owns
//! app storage, and serves gryth-ui. This crate holds the dependency-light,
//! unit-testable core — CLI parsing, the mode→node-profile mapping, the data
//! layout, the node argv, and the `/bootstrap.json` body — so that `main.rs`
//! stays a thin orchestrator (spawn node · serve http · supervise).
//!
//! Nothing here touches the network or a store; nothing here reads the real
//! `~/.glade` (the node always runs under `GLADE_HOME=<data>/sys`, never $HOME).

use std::path::{Path, PathBuf};

/// Grazel's run mode. Composes the existing glade node profiles.
///
/// The node binary makes NO serve-only / mesh-only distinction: EVERY booted
/// profile seeds the registry, serves the WS carrier to clients, AND binds the
/// iroh mesh endpoint + accepts peers (verified from `glade-node.rs` — the
/// `booted` branch does both unconditionally). So the profile flag only picks
/// the default instance name; grazel's three modes are a grazel-layer
/// distinction (naming + forward intent) that gains teeth when the node grows
/// real serve-only / mesh-only levers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Serve UI + local client sessions (the dev-box entry node).
    Local,
    /// Mesh participant holding claims (workspace host).
    Peer,
    /// The dev-box default: one process doing serve + mesh. A booted node
    /// already does both, so this maps to the `local` node profile.
    Both,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "local" => Some(Mode::Local),
            "peer" => Some(Mode::Peer),
            "both" => Some(Mode::Both),
            _ => None,
        }
    }

    /// grazel's own name for the mode (goes into `/bootstrap.json`).
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Local => "local",
            Mode::Peer => "peer",
            Mode::Both => "both",
        }
    }

    /// The `--profile` value passed to `glade-node`. `both` maps to `local`
    /// because a booted local node already serves clients AND meshes.
    pub fn node_profile(self) -> &'static str {
        match self {
            Mode::Local | Mode::Both => "local",
            Mode::Peer => "peer",
        }
    }
}

pub const USAGE: &str = "\
grazel — the gryth node (a glade application; composes glade suppliers)

USAGE:
    grazel --mode local|peer|both [OPTIONS]

OPTIONS:
    --mode <local|peer|both>  run mode (required)
    --name <NAME>             node/session name (default: grazel)
    --data <DIR>              app-owned storage root: DIR/{sys,files,config}
                              (default: grazel-data) — NEVER the real ~/.glade
    --http <PORT>             grazel HTTP (ui + /bootstrap.json) (default: 8080)
    --node-port <PORT>        glade node WS carrier port; 0 = OS-assigned
                              (default: 9099)
    --ui <DIR>                static dir served over HTTP (default: ui)
    --app <FILE.glade>        grazel's app declaration
                              (default: apps/grazel-app.glade)
    --node-bin <PATH>         glade-node binary
                              (default: ../glade/node/target/debug/glade-node)
    -h, --help               print this help
";

/// Parsed CLI surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub mode: Mode,
    pub name: String,
    pub data: PathBuf,
    pub http_port: u16,
    pub node_port: u16,
    pub ui: PathBuf,
    pub app: PathBuf,
    pub node_bin: PathBuf,
}

impl Config {
    /// Parse args (WITHOUT the program name). Returns the usage string on
    /// `--help` and a diagnostic + usage on any error.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Config, String> {
        let mut mode: Option<String> = None;
        let mut name = "grazel".to_string();
        let mut data = PathBuf::from("grazel-data");
        let mut http_port: u16 = 8080;
        let mut node_port: u16 = 9099;
        let mut ui = PathBuf::from("ui");
        let mut app = PathBuf::from("apps/grazel-app.glade");
        let mut node_bin = PathBuf::from("../glade/node/target/debug/glade-node");

        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--mode" => mode = Some(next(&mut it, "--mode")?),
                "--name" => name = next(&mut it, "--name")?,
                "--data" => data = PathBuf::from(next(&mut it, "--data")?),
                "--http" => http_port = parse_port(&next(&mut it, "--http")?, "--http")?,
                "--node-port" => {
                    node_port = parse_port(&next(&mut it, "--node-port")?, "--node-port")?
                }
                "--ui" => ui = PathBuf::from(next(&mut it, "--ui")?),
                "--app" => app = PathBuf::from(next(&mut it, "--app")?),
                "--node-bin" => node_bin = PathBuf::from(next(&mut it, "--node-bin")?),
                "-h" | "--help" => return Err(USAGE.to_string()),
                other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
            }
        }

        let mode = mode.ok_or_else(|| format!("--mode is required (local|peer|both)\n\n{USAGE}"))?;
        let mode = Mode::parse(&mode)
            .ok_or_else(|| format!("invalid --mode {mode:?} (want local|peer|both)\n\n{USAGE}"))?;

        Ok(Config { mode, name, data, http_port, node_port, ui, app, node_bin })
    }

    /// The `GLADE_HOME` for the spawned node: `<data>/sys`. The node nests its
    /// own `sys/<name>/` under this, so the instance lives at
    /// `<data>/sys/sys/<name>/` — glade's system tree, wholly inside the `sys`
    /// slot of the `data/{sys,files,config}` layout, never `~/.glade`.
    pub fn glade_home(&self) -> PathBuf {
        self.data.join("sys")
    }

    /// argv for `glade-node` (the booted profile form, GDL-036/037):
    /// `--profile <p> --name <name> --app <file> <node_port>`. No positional
    /// store dir — the node defaults it under its own instance cache, keeping
    /// glade's store layout glade's business (the data-seam rule).
    pub fn node_argv(&self) -> Vec<String> {
        vec![
            "--profile".to_string(),
            self.mode.node_profile().to_string(),
            "--name".to_string(),
            self.name.clone(),
            "--app".to_string(),
            self.app.display().to_string(),
            self.node_port.to_string(),
        ]
    }
}

fn next<I: Iterator<Item = String>>(it: &mut I, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))
}

fn parse_port(s: &str, flag: &str) -> Result<u16, String> {
    s.parse::<u16>().map_err(|_| format!("{flag}: {s:?} is not a valid port\n\n{USAGE}"))
}

/// Create the app-owned storage layout `<data>/{sys,files,config}`.
///
/// The data-seam rule (README): glade never sees grazel's files. `sys` is
/// glade's system home (`GLADE_HOME`); `files` is app-owned storage a supplier
/// serves from only where a surface is DECLARED; `config` is grazel's own
/// config. Private = undeclared; shared = a declared surface.
pub fn ensure_data_layout(dir: &Path) -> std::io::Result<()> {
    for sub in ["sys", "files", "config"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    Ok(())
}

/// The `GET /bootstrap.json` body — the GDL-032 session-placement seam. Grant
/// handoff fields arrive with P2; today it carries the node WS URL + identity.
pub fn bootstrap_json(node_ws_port: u16, mode: &str, name: &str) -> String {
    format!(
        "{{\"node_ws\":\"ws://127.0.0.1:{}\",\"mode\":\"{}\",\"name\":\"{}\"}}",
        node_ws_port,
        json_escape(mode),
        json_escape(name)
    )
}

/// Minimal JSON string escaping for the controlled bootstrap fields.
pub fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// Resolve a request path to a file under `ui`, returning its bytes + a
/// content-type. `/` maps to `index.html`. Rejects `..` traversal. `None` =
/// 404 (missing or escaping the root).
pub fn read_static(ui: &Path, path: &str) -> Option<(Vec<u8>, &'static str)> {
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    if rel.is_empty() || rel.split('/').any(|seg| seg == ".." || seg == ".") {
        return None;
    }
    let full = ui.join(rel);
    let bytes = std::fs::read(&full).ok()?;
    Some((bytes, content_type(&full)))
}

/// Content-type from a file extension (skeleton set; octet-stream fallback).
pub fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse_roundtrip() {
        assert_eq!(Mode::parse("local"), Some(Mode::Local));
        assert_eq!(Mode::parse("peer"), Some(Mode::Peer));
        assert_eq!(Mode::parse("both"), Some(Mode::Both));
        assert_eq!(Mode::parse("server"), None);
        assert_eq!(Mode::parse(""), None);
    }

    #[test]
    fn mode_maps_to_node_profile() {
        // both -> local: a booted local node already serves + meshes.
        assert_eq!(Mode::Local.node_profile(), "local");
        assert_eq!(Mode::Peer.node_profile(), "peer");
        assert_eq!(Mode::Both.node_profile(), "local");
    }

    #[test]
    fn mode_as_str_is_its_own_name() {
        assert_eq!(Mode::Local.as_str(), "local");
        assert_eq!(Mode::Peer.as_str(), "peer");
        assert_eq!(Mode::Both.as_str(), "both");
    }

    #[test]
    fn config_defaults() {
        let c = Config::parse(["--mode", "both"].map(String::from)).unwrap();
        assert_eq!(c.mode, Mode::Both);
        assert_eq!(c.name, "grazel");
        assert_eq!(c.data, PathBuf::from("grazel-data"));
        assert_eq!(c.http_port, 8080);
        assert_eq!(c.node_port, 9099);
        assert_eq!(c.ui, PathBuf::from("ui"));
        assert_eq!(c.app, PathBuf::from("apps/grazel-app.glade"));
        assert_eq!(c.node_bin, PathBuf::from("../glade/node/target/debug/glade-node"));
    }

    #[test]
    fn config_overrides() {
        let args = [
            "--mode", "peer", "--name", "n1", "--data", "/tmp/d", "--http", "18080",
            "--node-port", "0", "--ui", "web", "--app", "a.glade", "--node-bin", "/x/glade-node",
        ]
        .map(String::from);
        let c = Config::parse(args).unwrap();
        assert_eq!(c.mode, Mode::Peer);
        assert_eq!(c.name, "n1");
        assert_eq!(c.data, PathBuf::from("/tmp/d"));
        assert_eq!(c.http_port, 18080);
        assert_eq!(c.node_port, 0);
        assert_eq!(c.ui, PathBuf::from("web"));
        assert_eq!(c.app, PathBuf::from("a.glade"));
        assert_eq!(c.node_bin, PathBuf::from("/x/glade-node"));
    }

    #[test]
    fn config_requires_mode() {
        let err = Config::parse(["--name", "x"].map(String::from)).unwrap_err();
        assert!(err.contains("--mode is required"));
    }

    #[test]
    fn config_rejects_bad_mode_and_unknown_and_bad_port() {
        assert!(Config::parse(["--mode", "nope"].map(String::from)).unwrap_err().contains("invalid --mode"));
        assert!(Config::parse(["--bogus"].map(String::from)).unwrap_err().contains("unknown argument"));
        assert!(Config::parse(["--mode", "both", "--http", "99999"].map(String::from)).unwrap_err().contains("not a valid port"));
    }

    #[test]
    fn glade_home_is_under_data_sys() {
        let c = Config::parse(["--mode", "both", "--data", "/var/g"].map(String::from)).unwrap();
        assert_eq!(c.glade_home(), PathBuf::from("/var/g/sys"));
    }

    #[test]
    fn node_argv_for_both_uses_local_profile() {
        let c = Config::parse(
            ["--mode", "both", "--name", "grz", "--app", "apps/grazel-app.glade", "--node-port", "9099"]
                .map(String::from),
        )
        .unwrap();
        assert_eq!(
            c.node_argv(),
            vec!["--profile", "local", "--name", "grz", "--app", "apps/grazel-app.glade", "9099"]
        );
    }

    #[test]
    fn node_argv_for_peer_uses_peer_profile() {
        let c = Config::parse(["--mode", "peer"].map(String::from)).unwrap();
        assert_eq!(c.node_argv()[..2], ["--profile".to_string(), "peer".to_string()]);
    }

    #[test]
    fn bootstrap_json_shape() {
        assert_eq!(
            bootstrap_json(9099, "both", "grazel"),
            r#"{"node_ws":"ws://127.0.0.1:9099","mode":"both","name":"grazel"}"#
        );
    }

    #[test]
    fn bootstrap_json_escapes_name() {
        let j = bootstrap_json(1, "local", "a\"b\\c");
        assert!(j.contains(r#""name":"a\"b\\c""#), "{j}");
    }

    #[test]
    fn static_rejects_traversal() {
        let ui = PathBuf::from("/nonexistent-ui-root");
        assert_eq!(read_static(&ui, "/../etc/passwd"), None);
        assert_eq!(read_static(&ui, "/a/../../b"), None);
        assert_eq!(read_static(&ui, "/"), None); // index.html missing -> 404, not panic
    }

    #[test]
    fn static_serves_a_file() {
        let dir = std::env::temp_dir().join(format!("grazel-static-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), b"<h1>hi</h1>").unwrap();
        std::fs::write(dir.join("app.js"), b"console.log(1)").unwrap();
        let (root, ct) = read_static(&dir, "/").unwrap();
        assert_eq!(root, b"<h1>hi</h1>");
        assert_eq!(ct, "text/html; charset=utf-8");
        let (js, jct) = read_static(&dir, "/app.js").unwrap();
        assert_eq!(js, b"console.log(1)");
        assert_eq!(jct, "text/javascript; charset=utf-8");
        assert_eq!(read_static(&dir, "/missing.js"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn content_type_map() {
        assert_eq!(content_type(Path::new("x.html")), "text/html; charset=utf-8");
        assert_eq!(content_type(Path::new("x.json")), "application/json");
        assert_eq!(content_type(Path::new("x.wasm")), "application/wasm");
        assert_eq!(content_type(Path::new("x.bin")), "application/octet-stream");
    }
}
