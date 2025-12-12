use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use std::io;
use std::time::{Duration, Instant};

use crate::backend::{BackendKind, BackendRegistry};
use crate::models::DisplayOutput;

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

pub fn run_tui() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal);

    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    result
}

fn run_event_loop(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    let mut last_tick = Instant::now();
    let mut state = TuiState::new()?;

    loop {
        terminal.draw(|frame| {
            render_ui(frame, &mut state);
        })?;

        let timeout = REFRESH_INTERVAL
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if state.handle_key_event(key.code)? {
                    break;
                }
            }
        }

        if last_tick.elapsed() >= REFRESH_INTERVAL {
            state.silent_refresh();
            last_tick = Instant::now();
        }
    }

    // Clean up mirror processes on exit
    state.cleanup_mirrors();

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Enabled,
    Mode,
    Position,
    Mirror,
    Transform,
    Scale,
    AdaptiveSync,
    Brightness,
    Gamma,
    Temperature,
}

impl Field {
    fn next(&self) -> Self {
        match self {
            Field::Enabled => Field::Mode,
            Field::Mode => Field::Position,
            Field::Position => Field::Mirror,
            Field::Mirror => Field::Transform,
            Field::Transform => Field::Scale,
            Field::Scale => Field::AdaptiveSync,
            Field::AdaptiveSync => Field::Brightness,
            Field::Brightness => Field::Gamma,
            Field::Gamma => Field::Temperature,
            Field::Temperature => Field::Enabled,
        }
    }

    fn prev(&self) -> Self {
        match self {
            Field::Enabled => Field::Temperature,
            Field::Mode => Field::Enabled,
            Field::Position => Field::Mode,
            Field::Mirror => Field::Position,
            Field::Transform => Field::Mirror,
            Field::Scale => Field::Transform,
            Field::AdaptiveSync => Field::Scale,
            Field::Brightness => Field::AdaptiveSync,
            Field::Gamma => Field::Brightness,
            Field::Temperature => Field::Gamma,
        }
    }

    fn should_close_on_apply(&self) -> bool {
        matches!(self, Field::Enabled | Field::Mode)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MirrorScaling {
    Fit,
    Cover,
    Exact,
    Linear,
}

impl MirrorScaling {
    fn all() -> &'static [MirrorScaling] {
        &[MirrorScaling::Fit, MirrorScaling::Cover, MirrorScaling::Exact, MirrorScaling::Linear]
    }

    fn label(&self) -> &'static str {
        match self {
            MirrorScaling::Fit => "Fit",
            MirrorScaling::Cover => "Cover",
            MirrorScaling::Exact => "Exact",
            MirrorScaling::Linear => "Linear",
        }
    }

    fn arg(&self) -> &'static str {
        match self {
            MirrorScaling::Fit => "fit",
            MirrorScaling::Cover => "cover",
            MirrorScaling::Exact => "exact",
            MirrorScaling::Linear => "linear",
        }
    }
}

#[derive(Debug, Clone)]
struct MirrorSetup {
    dest: String,
    source: String,
}

struct TuiState {
    registry: BackendRegistry,
    backend: BackendKind,
    outputs: Vec<DisplayOutput>,
    selected_output: usize,
    selected_field: Field,
    dropdown_open: bool,
    dropdown_selection: usize,
    status: String,
    mirror_processes: std::collections::HashMap<String, std::process::Child>,
    mirror_pending: Option<MirrorSetup>,
    mirror_scaling: MirrorScaling,
}

impl TuiState {
    fn new() -> Result<Self> {
        let backend = BackendKind::auto_detect();
        let registry = BackendRegistry::default();
        let mut state = Self {
            registry,
            backend,
            outputs: Vec::new(),
            selected_output: 0,
            selected_field: Field::Mode,
            dropdown_open: false,
            dropdown_selection: 0,
            status: String::from("Ready"),
            mirror_processes: std::collections::HashMap::new(),
            mirror_pending: None,
            mirror_scaling: MirrorScaling::Fit,
        };
        state.refresh_outputs()?;
        Ok(state)
    }

    fn backend_label(&self) -> &'static str {
        match self.backend {
            BackendKind::X11 => "X11",
            BackendKind::Wlroots => "Wlroots",
            BackendKind::Gnome => "GNOME",
        }
    }

    fn handle_key_event(&mut self, code: KeyCode) -> Result<bool> {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.dropdown_open {
                    self.dropdown_open = false;
                    self.dropdown_selection = 0;
                    self.mirror_pending = None;
                } else {
                    return Ok(true);
                }
            }
            KeyCode::Char('r') => {
                let result = self.manual_refresh();
                self.handle_action_result(result);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.dropdown_open {
                    self.dropdown_prev();
                } else {
                    self.select_prev_output();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.dropdown_open {
                    self.dropdown_next();
                } else {
                    self.select_next_output();
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if !self.dropdown_open {
                    self.selected_field = self.selected_field.prev();
                    self.status = format!("Selected: {:?}", self.selected_field);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if !self.dropdown_open {
                    self.selected_field = self.selected_field.next();
                    self.status = format!("Selected: {:?}", self.selected_field);
                }
            }
            KeyCode::Tab => {
                if self.dropdown_open {
                    self.dropdown_open = false;
                    self.dropdown_selection = 0;
                    self.mirror_pending = None;
                } else {
                    self.dropdown_open = true;
                    self.dropdown_selection = 0;
                }
            }
            KeyCode::Enter => {
                if self.dropdown_open {
                    let should_close = self.selected_field.should_close_on_apply();
                    let result = self.apply_dropdown_action();
                    self.handle_action_result(result);
                    if should_close {
                        self.dropdown_open = false;
                        self.dropdown_selection = 0;
                    }
                }
            }
            _ => {}
        }

        Ok(false)
    }

    fn handle_action_result(&mut self, result: Result<()>) {
        if let Err(err) = result {
            self.status = format!("Error: {err}");
        }
    }

    fn silent_refresh(&mut self) {
        if let Err(err) = self.refresh_outputs() {
            self.status = format!("Error: {err}");
        }
    }

    fn manual_refresh(&mut self) -> Result<()> {
        self.refresh_outputs()?;
        self.status = "Outputs refreshed".to_string();
        Ok(())
    }

    fn refresh_outputs(&mut self) -> Result<()> {
        let outputs = self.registry.fetch_outputs(self.backend)?;
        self.outputs = outputs;

        if !self.outputs.is_empty() && self.selected_output >= self.outputs.len() {
            self.selected_output = 0;
        }

        // Drop pending mirror if either output is gone
        if let Some(pending) = &self.mirror_pending {
            let dest_exists = self.outputs.iter().any(|out| out.name == pending.dest);
            let src_exists = self.outputs.iter().any(|out| out.name == pending.source);
            if !dest_exists || !src_exists {
                self.mirror_pending = None;
                self.dropdown_open = false;
                self.dropdown_selection = 0;
                self.status = "Mirror setup cancelled: output disconnected".to_string();
            }
        }

        // Clean up dead wl-mirror processes (user closed window with 'q')
        self.mirror_processes.retain(|_name, process| {
            match process.try_wait() {
                Ok(Some(_status)) => false,  // Process exited, remove it
                Ok(None) => true,             // Still running, keep it
                Err(_) => false,              // Error checking, remove it
            }
        });

        Ok(())
    }

    fn select_next_output(&mut self) {
        if !self.outputs.is_empty() {
            self.selected_output = (self.selected_output + 1) % self.outputs.len();
        }
    }

    fn select_prev_output(&mut self) {
        if !self.outputs.is_empty() {
            self.selected_output = if self.selected_output == 0 {
                self.outputs.len() - 1
            } else {
                self.selected_output - 1
            };
        }
    }

    fn dropdown_next(&mut self) {
        let max = self.get_dropdown_items().len();
        if max > 0 {
            self.dropdown_selection = (self.dropdown_selection + 1) % max;
        }
    }

    fn dropdown_prev(&mut self) {
        let max = self.get_dropdown_items().len();
        if max > 0 {
            self.dropdown_selection = if self.dropdown_selection == 0 {
                max - 1
            } else {
                self.dropdown_selection - 1
            };
        }
    }

    fn get_dropdown_items(&self) -> Vec<String> {
        let Some(output) = self.outputs.get(self.selected_output) else {
            return vec![];
        };

        match self.selected_field {
            Field::Enabled => vec![
                if output.enabled { "Disable".to_string() } else { "Enable".to_string() }
            ],
            Field::Mode => {
                output.available_modes.iter().map(|mode| {
                    format!("{}x{}{}", 
                        mode.width, 
                        mode.height,
                        mode.refresh_hz.map(|hz| format!(" @ {:.0}Hz", hz)).unwrap_or_default()
                    )
                }).collect()
            }
            Field::Position => {
                let mut items = vec!["Custom X,Y".to_string()];
                for other in &self.outputs {
                    if other.name != output.name {
                        items.push(format!("Left of {}", other.name));
                        items.push(format!("Right of {}", other.name));
                        items.push(format!("Above {}", other.name));
                        items.push(format!("Below {}", other.name));
                    }
                }
                items
            }
            Field::Transform => vec![
                "normal".to_string(),
                "90°".to_string(),
                "180°".to_string(),
                "270°".to_string(),
                "flipped".to_string(),
                "flipped-90°".to_string(),
                "flipped-180°".to_string(),
                "flipped-270°".to_string(),
            ],
            Field::Mirror => {
                if self.mirror_pending.is_some() {
                    let mut items: Vec<String> = MirrorScaling::all()
                        .iter()
                        .map(|scaling| scaling.label().to_string())
                        .collect();
                    items.push("Cancel".to_string());
                    items
                } else {
                    let mut items = vec![];

                    // Check if current output is already mirroring something
                    if self.mirror_processes.contains_key(&output.name) {
                        items.push("Stop mirroring".to_string());
                    }

                    // Add available outputs to mirror
                    for other in &self.outputs {
                        if other.name != output.name && other.enabled {
                            items.push(format!("Mirror: {}", other.name));
                        }
                    }

                    if items.is_empty() {
                        items.push("No other outputs available".to_string());
                    }

                    items
                }
            }
            Field::Scale => vec![
                "0.5x".to_string(),
                "0.75x".to_string(),
                "1.0x".to_string(),
                "1.25x".to_string(),
                "1.5x".to_string(),
                "2.0x".to_string(),
            ],
            Field::AdaptiveSync => vec![
                "Enable".to_string(),
                "Disable".to_string(),
            ],
            Field::Brightness => vec!["+5%".to_string(), "-5%".to_string()],
            Field::Gamma => vec!["+0.1".to_string(), "-0.1".to_string()],
            Field::Temperature => vec!["+100K".to_string(), "-100K".to_string()],
        }
    }

    fn apply_dropdown_action(&mut self) -> Result<()> {
        let Some(output) = self.outputs.get(self.selected_output) else {
            return Ok(());
        };
        let name = output.name.clone();

        match self.selected_field {
            Field::Enabled => {
                let new_enabled = !output.enabled;
                self.registry.execute_apply(
                    self.backend,
                    &name,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(new_enabled),
                )?;
                self.refresh_outputs()?;
                self.status = format!("{}: {}", name, if new_enabled { "enabled" } else { "disabled" });
            }
            Field::Mode => {
                if let Some(mode) = output.available_modes.get(self.dropdown_selection).cloned() {
                    let mode_str = format!("{}x{}{}", 
                        mode.width, 
                        mode.height,
                        mode.refresh_hz.map(|hz| format!(" @ {:.0}Hz", hz)).unwrap_or_default()
                    );
                    self.registry.execute_apply(
                        self.backend,
                        &name,
                        Some(mode),
                        None,
                        None,
                        None,
                        None,
                        None,
                    )?;
                    self.refresh_outputs()?;
                    self.status = format!("{}: mode {}", name, mode_str);
                }
            }
            Field::Brightness => {
                let current = output.color.brightness.unwrap_or(1.0);
                let delta = if self.dropdown_selection == 0 { 0.05 } else { -0.05 };
                let new_value = (current + delta).clamp(0.0, 1.0);
                self.registry.execute_apply(
                    self.backend,
                    &name,
                    None,
                    None,
                    Some(new_value),
                    None,
                    None,
                    None,
                )?;
                self.refresh_outputs()?;
                self.status = format!("{}: brightness {:.0}%", name, new_value * 100.0);
            }
            Field::Gamma => {
                let current = output.color.gamma.unwrap_or(1.0);
                let delta = if self.dropdown_selection == 0 { 0.1 } else { -0.1 };
                let new_value = (current + delta).max(0.1);
                self.registry.execute_apply(
                    self.backend,
                    &name,
                    None,
                    None,
                    None,
                    Some(new_value),
                    None,
                    None,
                )?;
                self.refresh_outputs()?;
                self.status = format!("{}: gamma {:.2}", name, new_value);
            }
            Field::Temperature => {
                let current = output.color.temperature.unwrap_or(6500);
                let delta = if self.dropdown_selection == 0 { 100 } else { -100 };
                let new_value = ((current as i32) + delta).clamp(1000, 10000) as u16;
                self.registry.execute_apply(
                    self.backend,
                    &name,
                    None,
                    None,
                    None,
                    None,
                    Some(new_value),
                    None,
                )?;
                self.refresh_outputs()?;
                self.status = format!("{}: temperature {}K", name, new_value);
            }
            Field::Position => {
                let items = self.get_dropdown_items();
                if let Some(item) = items.get(self.dropdown_selection) {
                    if item == "Custom X,Y" {
                        // TODO: Implement custom position input
                        self.status = "Custom position not yet implemented".to_string();
                    } else if item.starts_with("Left of ") {
                        let target = item.strip_prefix("Left of ").unwrap();
                        self.apply_position_relative(&name, target, "left")?;
                    } else if item.starts_with("Right of ") {
                        let target = item.strip_prefix("Right of ").unwrap();
                        self.apply_position_relative(&name, target, "right")?;
                    } else if item.starts_with("Above ") {
                        let target = item.strip_prefix("Above ").unwrap();
                        self.apply_position_relative(&name, target, "above")?;
                    } else if item.starts_with("Below ") {
                        let target = item.strip_prefix("Below ").unwrap();
                        self.apply_position_relative(&name, target, "below")?;
                    }
                }
            }
            Field::Transform => {
                let transforms = ["normal", "90", "180", "270", "flipped", "flipped-90", "flipped-180", "flipped-270"];
                if let Some(transform) = transforms.get(self.dropdown_selection) {
                    self.apply_transform(&name, transform)?;
                }
            }
            Field::Mirror => {
                let items = self.get_dropdown_items();
                if let Some(item) = items.get(self.dropdown_selection) {
                    if let Some(pending) = self.mirror_pending.clone() {
                        if item == "Cancel" {
                            self.mirror_pending = None;
                            self.dropdown_open = false;
                            self.dropdown_selection = 0;
                            self.status = "Mirror selection cancelled".to_string();
                        } else if let Some((_, &scaling)) = MirrorScaling::all()
                            .iter()
                            .enumerate()
                            .find(|(_, scaling)| scaling.label() == item)
                        {
                            self.mirror_scaling = scaling;
                            let dest = pending.dest.clone();
                            let source = pending.source.clone();
                            self.mirror_pending = None;
                            self.dropdown_open = false;
                            self.dropdown_selection = 0;
                            self.start_mirror(&dest, &source, self.mirror_scaling)?;
                            self.status = format!("{}: mirroring {} ({} scaling)", dest, source, self.mirror_scaling.label().to_lowercase());
                        }
                    } else if item.starts_with("Stop") {
                        self.stop_mirror(&name)?;
                    } else if item.starts_with("Mirror: ") {
                        if let Some(target) = item.strip_prefix("Mirror: ") {
                            if target == "No other outputs available" {
                                return Ok(());
                            }
                            self.mirror_pending = Some(MirrorSetup {
                                dest: name.clone(),
                                source: target.to_string(),
                            });
                            if let Some(index) = MirrorScaling::all()
                                .iter()
                                .position(|scaling| *scaling == self.mirror_scaling)
                            {
                                self.dropdown_selection = index;
                            } else {
                                self.dropdown_selection = 0;
                            }
                            self.status = format!("{}: choose scaling for mirror {}", name, target);
                        }
                    }
                }
            }
            Field::Scale => {
                let scales = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
                if let Some(&scale) = scales.get(self.dropdown_selection) {
                    self.registry.execute_apply(
                        self.backend,
                        &name,
                        None,
                        Some(scale),
                        None,
                        None,
                        None,
                        None,
                    )?;
                    self.refresh_outputs()?;
                    self.status = format!("{}: scale {:.2}x", name, scale);
                }
            }
            Field::AdaptiveSync => {
                let enabled = self.dropdown_selection == 0;
                self.apply_adaptive_sync(&name, enabled)?;
            }
        }

        Ok(())
    }

    fn apply_position_relative(&mut self, output: &str, relative_to: &str, direction: &str) -> Result<()> {
        self.registry.execute_position_relative(self.backend, output, relative_to, direction)?;
        self.refresh_outputs()?;
        self.status = format!("{}: positioned {} {}", output, direction, relative_to);
        Ok(())
    }

    fn apply_transform(&mut self, output: &str, transform: &str) -> Result<()> {
        self.registry.execute_transform(self.backend, output, transform)?;
        self.refresh_outputs()?;
        self.status = format!("{}: transform {}", output, transform);
        Ok(())
    }

    fn start_mirror(&mut self, output: &str, target: &str, scaling: MirrorScaling) -> Result<()> {
        use std::process::{Command, Stdio};
        
        // Stop existing mirror if any
        if self.mirror_processes.contains_key(output) {
            self.stop_mirror(output)?;
        }
        
        // IMPORTANT: Restore normal layout first to avoid double mirroring
        // Calculate proper positions based on actual screen widths
        let mut x_offset = 0i32;
        for out in &self.outputs {
            let backend = self.registry.get_backend(self.backend)?;
            // Ignore errors if monitor was disconnected
            let _ = self.registry.runtime.block_on(async {
                backend.set_position(&out.name, x_offset, 0).await
            });
            
            // Calculate logical width (no scaling, so just use physical width)
            if let Some(mode) = &out.current_mode {
                x_offset += mode.width as i32;
            }
        }
        
        // Start wl-mirror in fullscreen mode with the selected scaling
        // --fullscreen-output puts it on the target output automatically
        let child = Command::new("wl-mirror")
            .arg("--fullscreen-output")
            .arg(output)  // Display on this output
            .arg("--scaling")
            .arg(scaling.arg())
            .arg(target)  // Mirror this source output
            .env("WAYLAND_DISPLAY", std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())  // Suppress errors to avoid blocking
            .spawn();
        
        match child {
            Ok(process) => {
                self.mirror_processes.insert(output.to_string(), process);
                self.status = format!("{}: mirroring {} ({} scaling, press q in window to stop)", output, target, scaling.label().to_lowercase());
                tracing::info!("Started wl-mirror fullscreen {}: {} -> {}", scaling.arg(), target, output);
                Ok(())
            }
            Err(e) => {
                self.status = format!("Failed to start wl-mirror: {}. Install: nix-shell -p wl-mirror", e);
                tracing::error!("Failed to start wl-mirror: {}", e);
                Err(anyhow::anyhow!("wl-mirror not found: {}", e))
            }
        }
    }

    fn stop_mirror(&mut self, output: &str) -> Result<()> {
        if let Some(mut process) = self.mirror_processes.remove(output) {
            // Kill wl-mirror process if running
            let _ = process.kill();
            let _ = process.wait();
            
            // Restore normal side-by-side layout
            // Calculate proper positions based on actual screen widths
            let mut x_offset = 0i32;
            for out in &self.outputs {
                let backend = self.registry.get_backend(self.backend)?;
                // Ignore errors if monitor was disconnected
                let _ = self.registry.runtime.block_on(async {
                    backend.set_position(&out.name, x_offset, 0).await
                });
                
                // Calculate logical width (no scaling, so just use physical width)
                if let Some(mode) = &out.current_mode {
                    x_offset += mode.width as i32;
                }
            }
            let _ = self.refresh_outputs();  // Ignore errors if monitors changed
            self.status = format!("{}: mirroring stopped, layout restored", output);
        } else {
            self.status = format!("{}: no active mirroring", output);
        }
        Ok(())
    }

    fn apply_adaptive_sync(&mut self, output: &str, enabled: bool) -> Result<()> {
        self.registry.execute_adaptive_sync(self.backend, output, enabled)?;
        self.refresh_outputs()?;
        self.status = format!("{}: adaptive sync {}", output, if enabled { "enabled" } else { "disabled" });
        Ok(())
    }

    fn cleanup_mirrors(&mut self) {
        for (output, mut process) in self.mirror_processes.drain() {
            let _ = process.kill();
            let _ = process.wait();
            tracing::info!("Stopped mirror process for {}", output);
        }
    }
}

fn render_ui(frame: &mut Frame<'_>, state: &mut TuiState) {
    let size = frame.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(size);

    let status_lines = vec![
        Line::from(vec![Span::styled(
            "sabantui – unified display manager",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(format!(
            "Backend: {} | Outputs: {}",
            state.backend_label(),
            state.outputs.len()
        )),
        Line::from(format!("Status: {}", state.status)),
        Line::from(
            "Keys: q/Esc quit, r refresh, ↑↓/jk select output, ←→/hl select field, Tab open/close menu, Enter apply"
                .to_string(),
        ),
    ];

    let header = Paragraph::new(status_lines)
        .block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(header, chunks[0]);

    // Render outputs
    let items: Vec<ListItem> = state.outputs.iter().enumerate().map(|(idx, output)| {
        let is_mirroring = state.mirror_processes.contains_key(&output.name);
        make_list_item(output, idx == state.selected_output, state.selected_field, is_mirroring)
    }).collect();
    
    let outputs_list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Outputs"));

    frame.render_widget(outputs_list, chunks[1]);

    // Render dropdown if open
    if state.dropdown_open {
        render_dropdown(frame, state, chunks[1]);
    }
}

fn make_list_item(output: &DisplayOutput, is_selected: bool, selected_field: Field, is_mirroring: bool) -> ListItem<'static> {
    let mut primary_line: Vec<Span<'static>> = vec![Span::styled(
        output.name.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )];

    if let Some(description) = &output.description {
        if !description.is_empty() {
            primary_line.push(Span::raw(" – "));
            primary_line.push(Span::raw(description.clone()));
        }
    }

    // Add mirror indicator
    if is_mirroring {
        primary_line.push(Span::raw(" "));
        primary_line.push(Span::styled(
            "[🔄 MIRRORING]",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
    }

    let enabled_text = if output.enabled { "enabled" } else { "disabled" };
    let enabled_style = if is_selected && selected_field == Field::Enabled {
        Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else if output.enabled {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Red)
    };

    let mode_text = output
        .current_mode
        .as_ref()
        .map(|mode| {
            let mut text = format!("{}x{}", mode.width, mode.height);
            if let Some(refresh) = mode.refresh_hz {
                text.push_str(&format!(" @ {:.0}Hz", refresh));
            }
            text
        })
        .unwrap_or_else(|| "--".to_string());

    let brightness_text = output
        .color
        .brightness
        .map(|b| format!("{:.0}%", b.clamp(0.0, 1.0) * 100.0))
        .unwrap_or_else(|| "--".to_string());
    let gamma_text = output
        .color
        .gamma
        .map(|g| format!("{:.2}", g))
        .unwrap_or_else(|| "--".to_string());
    let temp_text = output
        .color
        .temperature
        .map(|t| format!("{}K", t))
        .unwrap_or_else(|| "--".to_string());
    let scale_text = output
        .scale
        .map(|s| format!("{:.2}x", s))
        .unwrap_or_else(|| "--".to_string());

    let highlight_style = Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD);
    let normal_style = Style::default();

    let mut details_spans = vec![];
    
    details_spans.push(Span::styled(
        format!("[{}] ", enabled_text),
        enabled_style,
    ));
    
    details_spans.push(Span::styled(
        format!("Mode: {} ", mode_text),
        if is_selected && selected_field == Field::Mode { highlight_style } else { normal_style },
    ));
    
    details_spans.push(Span::raw("| "));
    
    details_spans.push(Span::styled(
        "Pos ",
        if is_selected && selected_field == Field::Position { highlight_style } else { normal_style },
    ));
    
    details_spans.push(Span::raw("| "));
    
    details_spans.push(Span::styled(
        "Mirror ",
        if is_selected && selected_field == Field::Mirror { highlight_style } else { normal_style },
    ));
    
    details_spans.push(Span::raw("| "));
    
    details_spans.push(Span::styled(
        "Transform ",
        if is_selected && selected_field == Field::Transform { highlight_style } else { normal_style },
    ));

    details_spans.push(Span::raw("| "));

    details_spans.push(Span::styled(
        format!("Scale: {} ", scale_text),
        if is_selected && selected_field == Field::Scale { highlight_style } else { normal_style },
    ));

    details_spans.push(Span::raw("| "));

    details_spans.push(Span::styled(
        "VRR ",
        if is_selected && selected_field == Field::AdaptiveSync { highlight_style } else { normal_style },
    ));
    
    details_spans.push(Span::raw("| "));
    
    details_spans.push(Span::styled(
        format!("Br: {} ", brightness_text),
        if is_selected && selected_field == Field::Brightness { highlight_style } else { normal_style },
    ));
    
    details_spans.push(Span::raw("| "));
    
    details_spans.push(Span::styled(
        format!("γ: {} ", gamma_text),
        if is_selected && selected_field == Field::Gamma { highlight_style } else { normal_style },
    ));
    
    details_spans.push(Span::raw("| "));
    
    details_spans.push(Span::styled(
        format!("T: {}", temp_text),
        if is_selected && selected_field == Field::Temperature { highlight_style } else { normal_style },
    ));

    let details_line = Line::from(details_spans);

    let mut item = ListItem::new(vec![Line::from(primary_line), details_line]);
    if !output.enabled {
        item = item.style(Style::default().fg(Color::DarkGray));
    }
    item
}

fn render_dropdown(frame: &mut Frame<'_>, state: &TuiState, area: ratatui::layout::Rect) {
    let items = state.get_dropdown_items();
    if items.is_empty() {
        return;
    }

    // Calculate dropdown position and size
    let dropdown_height = (items.len() + 2).min(20) as u16;
    let dropdown_width = items.iter().map(|s| s.len()).max().unwrap_or(20).max(20) as u16 + 4;
    
    let x = area.x + (area.width.saturating_sub(dropdown_width)) / 2;
    let y = area.y + (area.height.saturating_sub(dropdown_height)) / 2;
    
    let dropdown_area = ratatui::layout::Rect {
        x,
        y,
        width: dropdown_width,
        height: dropdown_height,
    };

    let list_items: Vec<ListItem> = items.iter().enumerate().map(|(idx, item)| {
        let style = if idx == state.dropdown_selection {
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        ListItem::new(Line::from(item.clone())).style(style)
    }).collect();

    let list = List::new(list_items)
        .block(Block::default().borders(Borders::ALL).title("Select"));

    frame.render_widget(ratatui::widgets::Clear, dropdown_area);
    frame.render_widget(list, dropdown_area);
}
