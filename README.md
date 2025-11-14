# unirandr

Universal TUI and CLI utility for managing display configurations across X11, wlroots-based Wayland compositors, GNOME Mutter, and KDE Plasma environments.

## Goals

1. **Unified interface** – offer a consistent terminal workflow for listing and configuring displays regardless of the underlying display server or desktop environment.
2. **Modular backends** – isolate environment-specific logic into backend modules that implement a shared trait-driven API.
3. **TUI + CLI** – provide both interactive Ratatui-driven dashboard and one-shot command execution for scripts.
4. **Extensibility** – make it easy to add new compositors or desktop environments.

## High-level architecture

```
+-------------------+
|      unirandr     |
| (CLI + Ratatui)   |
+---------+---------+
          |
   +------+------+
   |             |
+--v--+       +--v--+
| CLI |       | TUI |
+--+--+       +--+--+
   |             |
   +------+------+-----------+-----------+
          |                  |           |
      +---v----+      +------v-----+ +---v---+
      | Backend| ...  | Backend    | | ...   |
      | X11    |      | GNOME      |         |
      +---+----+      +------+-----+         |
          |                  |               |
    xrandr-rs API        Mutter D-Bus   etc.
```

### Crate layout (planned)

```
src/
├── main.rs              # CLI entrypoint, command parsing, runtime bootstrap
├── app.rs               # High-level mode/state management shared by TUI + CLI
├── tui/
│   ├── mod.rs          # Ratatui UI scaffolding
│   └── widgets.rs      # Custom widgets (output list, mode picker, etc.)
├── cli/
│   └── mod.rs          # clap command definitions, argument parsing helpers
├── backend/
│   ├── mod.rs          # Backend trait definitions + factory selection
│   ├── x11.rs          # xrandr backend implementation
│   ├── wlroots.rs      # way-displays integration
│   ├── gnome.rs        # Mutter D-Bus client
│   └── kde.rs          # kscreen-doctor invocation
└── models.rs           # Shared data models (Output, Mode, Layout, etc.)
```

### Runtime flow

1. Parse command line arguments (mode: interactive, list, configure, etc.).
2. Detect target backend unless overridden by CLI flag.
3. Instantiate backend via factory that returns an object implementing `DisplayBackend` trait.
4. CLI mode: execute the requested action directly using backend API.
5. TUI mode: initialize Ratatui terminal, load current configuration through backend, and drive event loop reacting to user input.

### Abstractions

```rust
#[async_trait::async_trait]
pub trait DisplayBackend {
    async fn list_outputs(&self) -> Result<Vec<DisplayOutput>>;
    async fn set_mode(&self, output: &str, mode: DisplayMode) -> Result<()>;
    async fn set_position(&self, output: &str, position: Position) -> Result<()>;
    async fn set_scale(&self, output: &str, scale: f64) -> Result<()>;
    async fn set_orientation(&self, output: &str, orientation: Orientation) -> Result<()>;
    async fn enable(&self, output: &str) -> Result<()>;
    async fn disable(&self, output: &str) -> Result<()>;
}
```

Backends stick to one async runtime contract. X11 backend can use blocking calls wrapped via `tokio::task::spawn_blocking` if needed.

### Environment detection

```rust
fn detect_backend(env: &Environment) -> BackendKind {
    if env.wayland_display.is_some() {
        if env.desktop.contains("GNOME") {
            BackendKind::Gnome
        } else if env.desktop.contains("KDE") {
            BackendKind::Kde
        } else {
            BackendKind::WlRoots
        }
    } else {
        BackendKind::X11
    }
}
```

### External tooling integration

- **way-displays** (wlroots): spawn command, parse JSON output when `--json` is exposed; otherwise parse text.
- **kscreen-doctor** (KDE): spawn command and parse text output.
- **zbus** (GNOME): interact with Mutter D-Bus API natively.
- **xrandr** (X11): use `xrandr` crate for binding-level access.

### Configuration persistence

Later iteration: optional YAML/JSON config file describing preferred layouts per backend; CLI `apply-profile` command.

### Telemetry & logging

Use `tracing` and `tracing-subscriber` for structured logging with env-filter to help debugging backend interactions.

### Testing strategy

- Unit tests for backend detection logic and command parsing (pure functions).
- Mock backend trait implementation for TUI state handling tests.
- Integration tests gated behind feature flags to run on target systems (requires environment detection). For CI, run subset with dry-run mode.

## Roadmap

1. Scaffolding (current): CLI skeleton, backend trait, environment detection.
2. Implement X11 backend with `xrandr` crate.
3. Integrate wlroots workflow via `way-displays` CLI invocations.
4. Add GNOME (zbus) + KDE (kscreen-doctor) backends.
5. Build Ratatui interface for interactive configuration.
6. Persist profiles and add scriptable commands.
