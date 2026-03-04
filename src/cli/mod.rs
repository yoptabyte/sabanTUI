use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser, Clone)]
#[command(
    name = "sabantui",
    version,
    about = "Universal display configuration CLI/TUI",
    long_about = "Universal TUI and CLI utility for managing display configurations across\n\
                   X11, wlroots-based Wayland compositors, and GNOME Mutter environments.\n\n\
                   Run without arguments to launch the interactive TUI.\n\n\
                   Examples:\n  \
                     sabantui                                    # launch TUI\n  \
                     sabantui list                               # discover your outputs\n  \
                     sabantui apply -o <OUTPUT> -m 1920x1080     # set resolution\n  \
                     sabantui apply -o <OUTPUT> -s 1.25 -B 0.8   # scale + brightness"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// List outputs for the detected or specified backend
    List {
        /// Override backend detection (x11, wlroots, gnome)
        #[arg(short, long)]
        backend: Option<BackendSelector>,
    },

    /// Apply configuration to a display output
    ///
    /// Change resolution, scale, brightness, color temperature and other
    /// properties of a display output. Use `sabantui list` to discover
    /// available output names. The backend is auto-detected unless
    /// overridden with -b.
    ///
    /// Examples:
    ///   sabantui apply -o <OUTPUT> -m 1920x1080 -r 144
    ///   sabantui apply -o <OUTPUT> -s 2.0 -B 0.8
    ///   sabantui apply -o <OUTPUT> -t 4500
    ///   sabantui apply -o <OUTPUT> -p 1920,0
    ///   sabantui apply -o <OUTPUT> -M <OTHER_OUTPUT>
    Apply {
        /// Override backend detection (x11, wlroots, gnome)
        #[arg(short, long)]
        backend: Option<BackendSelector>,

        /// Target display output name (e.g. DP-1, HDMI-1, eDP-1)
        #[arg(short, long)]
        output: String,

        /// Resolution in WxH format (e.g. 1920x1080, 2560x1440)
        #[arg(short, long)]
        mode: Option<String>,

        /// Refresh rate in Hz (e.g. 60, 144); used together with --mode
        #[arg(short, long)]
        refresh: Option<u32>,

        /// Scale factor (e.g. 1.0, 1.25, 2.0)
        #[arg(short, long)]
        scale: Option<f64>,

        /// Brightness value in range 0.0–1.0
        #[arg(short = 'B', long)]
        brightness: Option<f32>,

        /// Gamma multiplier (1.0 = neutral)
        #[arg(short = 'G', long)]
        gamma: Option<f32>,

        /// Color temperature in Kelvin (1000–10000)
        #[arg(short = 't', long, alias = "temp")]
        temperature: Option<u16>,

        /// Position as X,Y coordinates (e.g. 1920,0)
        #[arg(short, long)]
        position: Option<String>,

        /// Display orientation/transform (normal, 90, 180, 270, flipped, etc.)
        #[arg(short = 'O', long)]
        orientation: Option<String>,

        /// Mirror to another output (e.g. DP-2)
        #[arg(short = 'M', long)]
        mirror: Option<String>,

        /// Enable or disable the output (true/false)
        #[arg(short, long)]
        enabled: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BackendSelector {
    X11,
    Wlroots,
    Gnome,
}
