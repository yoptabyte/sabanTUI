# sabanTUI

![Logo](Logo.png)

Universal TUI and CLI utility for managing display configurations across X11, wlroots-based Wayland compositors, GNOME Mutter, and KDE Plasma environments.

## Goals

1. **Unified interface** – offer a consistent terminal workflow for listing and configuring displays regardless of the underlying display server or desktop environment.
2. **Modular backends** – isolate environment-specific logic into backend modules that implement a shared trait-driven API.
3. **TUI + CLI** – provide both interactive Ratatui-driven dashboard and one-shot command execution for scripts.
4. **Extensibility** – make it easy to add new compositors or desktop environments.

## High-level architecture

```
+-------------------+
|     sabanTUI      |
| (CLI + Ratatui)   |
+---------+---------+
          |
   +------+------+
   |             |
+--v--+       +--v--+
| CLI |       | TUI |
+-----+-------+-----+
      |               |
      +---+---+---+---+---+---+
          |   |   |   |   |   |
      +---v---+   |   |   +---v---+
      | X11    |   |   |   | KDE   |
      +---+----+   |   |   +---+---+
          |        |   |       |
      xrandr      |   |   kscreen-doctor
                  |   |
              +---v---+   +---v---+
              | GNOME  |   | ...   |
              +---+----+   +-------+
                  |
            Mutter D-Bus
```

### Crate layout

```
src/
├── main.rs              # CLI entrypoint, tracing init, runtime bootstrap
├── app.rs               # AppRuntime – routes CLI commands to backends
├── models.rs            # Shared data models (DisplayOutput, DisplayMode, etc.)
├── cli/
│   └── mod.rs          # clap command definitions with short flags
├── tui/
│   └── mod.rs          # Ratatui UI: state, event loop, rendering
└── backend/
    ├── mod.rs          # DisplayBackend trait + BackendRegistry + auto-detection
    ├── x11.rs          # xrandr CLI backend
    ├── wlroots.rs      # wlr-randr + wl-gammarelay-rs backend
    └── gnome.rs        # Mutter D-Bus + gsettings + ddcutil backend
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

- **wlr-randr** (wlroots): spawn command, parse JSON output (`--json`) or fallback to text parsing.
- **wl-gammarelay-rs** (wlroots): D-Bus interface for brightness, gamma, and color temperature.
- **zbus** (GNOME): interact with Mutter D-Bus API natively.
- **gsettings** (GNOME): Night Light color temperature control.
- **ddcutil** (GNOME, optional): hardware brightness for external monitors via DDC/CI.
- **xrandr** (X11): spawn `xrandr` CLI for mode/position/transform/mirror operations.

### System dependencies

The application requires external tools to be installed in your system (found via `PATH`). These dependencies are **not** bundled with the application - they must be provided by your distribution or desktop environment.

**Required for X11:**
- `xrandr` (part of xorg)

**Required for Wayland (wlroots-based compositors - Sway, Hyprland, etc.):**
- `wlr-randr` - display configuration
- `wl-gammarelay-rs` - color temperature/brightness control

**Required for GNOME:**
- `gsettings` (part of glib) - night light control
- `ddcutil` - hardware brightness for external monitors (optional, enables DDC/CI)

**Optional:**
- `wl-mirror` - for mirror functionality on wlroots

On NixOS or with flakes, these tools can be installed via configuration. For traditional systems, install them via your package manager.

### Configuration persistence

Later iteration: optional YAML/JSON config file describing preferred layouts per backend; CLI `apply-profile` command.

### Telemetry & logging

Use `tracing` and `tracing-subscriber` for structured logging with env-filter to help debugging backend interactions.

### Testing strategy

- Unit tests for backend detection logic and command parsing (pure functions).
- Mock backend trait implementation for TUI state handling tests.
- Integration tests gated behind feature flags to run on target systems (requires environment detection). For CI, run subset with dry-run mode.

## CLI usage

```bash
# Launch interactive TUI
sabantui

# List detected outputs
sabantui list
sabantui list -b x11          # force X11 backend

# Apply configuration (use `sabantui list` to find output names)
sabantui apply -o <OUTPUT> -m 1920x1080 -r 144
sabantui apply -o <OUTPUT> -s 1.25 -B 0.8
sabantui apply -o <OUTPUT> -t 4500
sabantui apply -o <OUTPUT> -p 1920,0
sabantui apply -o <OUTPUT> -M <OTHER_OUTPUT>
sabantui apply -o <OUTPUT> -e false    # disable output
```

Run `sabantui apply -h` for the full list of options.

## Roadmap

1. ~~Scaffolding: CLI skeleton, backend trait, environment detection~~ ✅
2. ~~X11 backend (xrandr CLI)~~ ✅
3. ~~wlroots backend (wlr-randr + wl-gammarelay-rs)~~ ✅
4. ~~GNOME backend (zbus + gsettings + ddcutil)~~ ✅
5. ~~Ratatui TUI interface~~ ✅
6. **KDE backend** (kscreen-doctor) — planned
7. **Refactor**: split monolithic modules (gnome, tui, models)
8. **Profile persistence**: YAML/JSON config for saved layouts
9. **Scriptable commands**: `apply-profile`, `save-profile`
