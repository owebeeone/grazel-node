grazel — the gryth node (a glade application; composes glade suppliers)…

grazel is the application authority that sits **above** the glade kernel. It
composes glade suppliers, owns app storage, and serves gryth-ui plus the
session-placement bootstrap. Base glade knows nothing in here: grazel is just
the first contributor of records (GDL-037), declared by `apps/grazel-app.glade`
(grazel's own copy — grazel owns its declaration; kept byte-identical with the
demo's `glade/apps/grazel-app.glade` copy until a shared source is factored out).

**P1.S3** — grazel now composes the **glade-gwz** supplier: after the node is
up, grazel spawns `glade-gwz` as a child process that attaches over the wire and
stands behind the `(ws-razel, gwz.ops)` command surface (see below). The
**glade-chat** supplier is TS/in-process in the UI host (gryth-ui / the demo),
NOT run by grazel; its surfaces (`chat.msgs`, `chat.groups`) are pre-declared in
`grazel-app.glade` so they exist node-side regardless of a running TS host.

## Composition posture — wire attachment

grazel **spawns the glade node as a subprocess** and attaches over the wire
(GLP-0006 P00-a: wire-attached supplier sessions are the ruled contract).
Embedding the node as a crate (loopback attach) is a later optimization;
process composition is the legitimate skeleton, not a shortcut. grazel
supervises the node one-directionally — if the node exits, grazel exits nonzero
with the tail of the node's stderr; a SIGINT/SIGTERM to grazel tears the node
child down.

## Run

    grazel --mode local|peer|both [--name N] [--data DIR] [--http PORT] \
           [--node-port PORT] [--ui DIR] [--app FILE.glade] [--node-bin PATH] \
           [--gwz-supplier-bin PATH] [--no-suppliers]

Defaults: `--name grazel`, `--data grazel-data`, `--http 8080`,
`--node-port 9099` (0 = OS-assigned), `--ui ui`,
`--app apps/grazel-app.glade`, `--node-bin ../glade/node/target/debug/glade-node`,
`--gwz-supplier-bin ../glade-gwz/target/debug/glade-gwz`.

## Composed suppliers

After the node is listening, grazel spawns each composed supplier as a **child
process** attached over the node's WS carrier (P00-a wire-attachment — embedding
is a later optimization, not the contract):

- **glade-gwz** — `glade-gwz --node ws://127.0.0.1:<node-port> --root <data>/files
  --share ws-razel --principal grazel`. It stands behind `(ws-razel, gwz.ops)`
  (declared by `service grazel gwz.ops` + the `workspace ws-razel razel` claim in
  `grazel-app.glade`) and runs allow-listed read verbs against the app-owned
  `files` store.

Suppliers are **optional**: an absent `--gwz-supplier-bin` is a loud SKIP (never
fatal), and a supplier exit only logs — grazel keeps running (the surface just
has no live provider until it is respawned). `--no-suppliers` disables them
entirely (node only). A SIGINT/SIGTERM to grazel, or a node exit, tears the
supplier child down too.

`GET /bootstrap.json` → `{"node_ws":"ws://127.0.0.1:<node-port>","mode":<mode>,
"name":<name>}` — the GDL-032 session-placement seam. Grant-handoff fields
arrive with P2.

## Modes

`--mode` composes the existing glade node profiles. The node binary makes **no**
serve-only / mesh-only distinction: every booted profile seeds the registry,
serves the WS carrier to clients, **and** binds the iroh mesh endpoint +
accepts peers. So the mode selects the node's default instance name today; the
grazel-layer distinction gains teeth when the node grows real serve-only /
mesh-only levers.

| `--mode` | node `--profile` | role |
| --- | --- | --- |
| `local` | `local` | serve UI + local client sessions (dev-box entry node) |
| `peer` | `peer` | mesh participant holding claims (workspace host) |
| `both` | `local` | dev-box default: one process serving **and** meshing (a booted local node already does both) |

## App-owned storage seam

`--data DIR` is grazel's storage root, laid out as:

    DIR/
      sys/      glade's system home — GLADE_HOME points here (the node nests
                its own sys/<name>/ under it). NEVER the real ~/.glade.
      files/    app-owned storage grazel manages (chat history, gwz workspaces,
                file trees — P1+). glade never sees this directly.
      config/   grazel's own configuration.

**The seam rule:** glade never sees grazel's files. **Private data = never
declared.** **Shared data = a declared surface a supplier serves from that
storage** (the file↔surface mapping is grazel's alone). "Files for now" cannot
leak into glade's model because glade only ever sees the declared surfaces.

## Repo name

This repo is **`grazel-node`** (`git@github.com:owebeeone/grazel-node.git`) —
the same what-not-role convention as `glial-runtime`. The bare
`owebeeone/grazel` name is squatted by the June razel release spike and is
**untouched** by this work.
