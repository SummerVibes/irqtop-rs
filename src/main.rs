use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame, Terminal,
};

use memchr;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Interrupt statistics
#[derive(Debug, Default, Clone)]
struct IrqStats {
    counts: Vec<u64>,
    name: String,
}

/// Parse command-line arguments
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Refresh interval in milliseconds
    #[arg(short, long, default_value_t = 1000)]
    interval: u64,
    
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show per cpu stats for a single IRQ
    Show { irq_name: String },
}

/// Application state
struct App {
    irq_data: HashMap<String, IrqStats>,
    prev_irq_data: HashMap<String, IrqStats>,
    deltas: Vec<(String, u64)>,
    per_cpu_deltas: HashMap<String, Vec<u64>>,
    affinity_map: HashMap<String, String>,
    effective_affinity_map: HashMap<String, String>,
    selected_row: usize,
    sort_by: SortBy,
    show_help: bool,
    show_irq_detail: bool,
    detail_irq_name: Option<String>,
    detail_scroll_offset: usize,
    running: bool,
    last_update: Instant,
}

#[derive(PartialEq, Eq)]
enum SortBy {
    Irq,
    Delta,
    Affinity,
    EffectiveAffinity,
    Device,
}

impl Default for App {
    fn default() -> Self {
        Self {
            irq_data: HashMap::new(),
            prev_irq_data: HashMap::new(),
            deltas: Vec::new(),
            per_cpu_deltas: HashMap::new(),
            affinity_map: HashMap::new(),
            effective_affinity_map: HashMap::new(),
            selected_row: 0,
            sort_by: SortBy::Delta,
            show_help: false,
            show_irq_detail: false,
            detail_irq_name: None,
            detail_scroll_offset: 0,
            running: true,
            last_update: Instant::now(),
        }
    }
}



/// Optimized /proc/interrupts reader
fn read_interrupts() -> Result<HashMap<String, IrqStats>> {
    // 1. Read file as raw bytes to avoid UTF-8 validation
    let content = fs::read("/proc/interrupts")?;
    
    // 2. Pre-allocate hashmap with expected size
    let mut irq_map = HashMap::with_capacity(256);
    
    // 3. Use memchr for fast line splitting
    let mut pos = 0;
    let mut line_num = 0;
    
    while pos < content.len() {
        // Find next newline
        let end = memchr::memchr(b'\n', &content[pos..])
            .map(|p| pos + p)
            .unwrap_or(content.len());
        
        // Skip header line
        if line_num == 0 {
            pos = end + 1;
            line_num += 1;
            continue;
        }

        // Process line in-place without allocation
        let line = &content[pos..end];
        if line.is_empty() {
            pos = end + 1;
            continue;
        }

        // 4. Fast IRQ number parsing
        let mut irq_end = 0;
        while irq_end < line.len() && line[irq_end] != b':' {
            irq_end += 1;
        }
        if irq_end == 0 {
            pos = end + 1;
            continue;
        }
        
        // 5. Parse counts with SIMD-accelerated number parsing
        let mut counts = Vec::with_capacity(256);
        let mut num_start = irq_end + 1;
        while num_start < line.len() {
            while num_start < line.len() && line[num_start].is_ascii_whitespace() {
                num_start += 1;
            }
            
            let mut num_end = num_start;
            while num_end < line.len() && (line[num_end] == b',' || line[num_end].is_ascii_digit()) {
                num_end += 1;
            }
            
            if num_start == num_end {
                break;
            }
            
            // 6. Fast u64 parsing without string allocation
            let mut value: u64 = 0;
            for &c in &line[num_start..num_end] {
                if c != b',' {
                    value = value * 10 + (c - b'0') as u64;
                }
            }
            counts.push(value);
            
            num_start = num_end;
        }

        // 7. Extract device name
        let name_start = num_start;
        let name = String::from_utf8_lossy(&line[name_start..]).trim().to_string();

        if !name.is_empty() && !counts.is_empty() {
            irq_map.insert(
                String::from_utf8_lossy(&line[..irq_end]).trim().to_string(),
                IrqStats {
                    counts,
                    name,
                },
            );
        }

        pos = end + 1;
        line_num += 1;
    }

    Ok(irq_map)
}

fn calculate_delta(old: &HashMap<String, IrqStats>, new: &HashMap<String, IrqStats>) -> Vec<(String, u64)> {
    let mut deltas = Vec::new();
    for (irq, new_stats) in new {
        if let Some(old_stats) = old.get(irq) {
            let delta: u64 = new_stats.counts.iter()
                .zip(old_stats.counts.iter())
                .map(|(n, o)| n.saturating_sub(*o))
                .sum();
            deltas.push((irq.clone(), delta));
        }
    }
    deltas
}

/// Get affinity mapping for all IRQs
fn get_affinity_map() -> HashMap<String, String> {
    let irq_dir = PathBuf::from("/proc/irq");
    let mut affinity_map = HashMap::new();
    
    if let Ok(entries) = fs::read_dir(irq_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(irq) = path.file_name().and_then(|n| n.to_str()) {
                let affinity_path = path.join("smp_affinity_list");
                if let Ok(affinity) = fs::read_to_string(affinity_path) {
                    affinity_map.insert(irq.to_string(), affinity.trim().to_string());
                }
            }
        }
    }
    affinity_map
}

fn get_effective_affinity_map() -> HashMap<String, String> {
    let irq_dir = PathBuf::from("/proc/irq");
    let mut affinity_map = HashMap::new();
    
    if let Ok(entries) = fs::read_dir(irq_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(irq) = path.file_name().and_then(|n| n.to_str()) {
                let affinity_path = path.join("effective_affinity_list");
                if let Ok(affinity) = fs::read_to_string(affinity_path) {
                    affinity_map.insert(irq.trim().to_string(), affinity.trim().to_string());
                }
            }
        }
    }
    affinity_map
}

impl App {
fn update_data(&mut self) -> Result<()> {
        let new_data = read_interrupts()?;
        let new_deltas = calculate_delta(&self.irq_data, &new_data);
        
        // Calculate per-CPU deltas
        self.per_cpu_deltas.clear();
        for (irq, new_stats) in &new_data {
            if let Some(old_stats) = self.prev_irq_data.get(irq) {
                let deltas: Vec<u64> = new_stats.counts.iter()
                    .zip(old_stats.counts.iter())
                    .map(|(n, o)| n.saturating_sub(*o))
                    .collect();
                self.per_cpu_deltas.insert(irq.clone(), deltas);
            } else {
                // First time seeing this IRQ, use current counts as deltas
                self.per_cpu_deltas.insert(irq.clone(), new_stats.counts.clone());
            }
        }
        
        // Update previous data
        self.prev_irq_data = new_data.clone();
        self.irq_data = new_data;
        self.deltas = new_deltas;
        self.affinity_map = get_affinity_map();
        self.effective_affinity_map = get_effective_affinity_map();
        self.last_update = Instant::now();
        
        Ok(())
    }

    fn sort_data(&mut self) {
        let default_str = "N/A";
        
        match self.sort_by {
            SortBy::Irq => self.deltas.sort_by(|a, b| a.0.cmp(&b.0)),
            SortBy::Delta => self.deltas.sort_by(|a, b| b.1.cmp(&a.1)),
            SortBy::Affinity => self.deltas.sort_by(|a, b| {
                let a_aff = self.affinity_map.get(&a.0).map(|s| s.as_str()).unwrap_or(default_str);
                let b_aff = self.affinity_map.get(&b.0).map(|s| s.as_str()).unwrap_or(default_str);
                a_aff.cmp(b_aff)
            }),
            SortBy::EffectiveAffinity => self.deltas.sort_by(|a, b| {
                let a_aff = self.effective_affinity_map.get(&a.0).map(|s| s.as_str()).unwrap_or(default_str);
                let b_aff = self.effective_affinity_map.get(&b.0).map(|s| s.as_str()).unwrap_or(default_str);
                a_aff.cmp(b_aff)
            }),
            SortBy::Device => self.deltas.sort_by(|a, b| {
                let a_dev = self.irq_data.get(&a.0).map(|s| s.name.as_str()).unwrap_or(default_str);
                let b_dev = self.irq_data.get(&b.0).map(|s| s.name.as_str()).unwrap_or(default_str);
                a_dev.cmp(b_dev)
            }),
        }
    }

    fn next_sort(&mut self) {
        self.sort_by = match self.sort_by {
            SortBy::Irq => SortBy::Delta,
            SortBy::Delta => SortBy::Affinity,
            SortBy::Affinity => SortBy::EffectiveAffinity,
            SortBy::EffectiveAffinity => SortBy::Device,
            SortBy::Device => SortBy::Irq,
        };
    }
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    tick_rate: Duration,
) -> Result<()> {
    let mut last_tick = Instant::now();
    
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => {
                        app.running = false;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.running = false;
                    }
                    KeyCode::Down => {
                        let max_row = app.deltas.len().saturating_sub(1);
                        if app.selected_row < max_row {
                            app.selected_row += 1;
                        }
                    }
                    KeyCode::Up => {
                        if app.selected_row > 0 {
                            app.selected_row -= 1;
                        }
                    }
                    KeyCode::PageDown => {
                        let max_row = app.deltas.len().saturating_sub(1);
                        app.selected_row = (app.selected_row + 10).min(max_row);
                    }
                    KeyCode::PageUp => {
                        app.selected_row = app.selected_row.saturating_sub(10);
                    }
                    KeyCode::Home => {
                        app.selected_row = 0;
                    }
                    KeyCode::End => {
                        app.selected_row = app.deltas.len().saturating_sub(1);
                    }
                    KeyCode::Tab => {
                        app.next_sort();
                        app.sort_data();
                    }
                    KeyCode::Char('h') | KeyCode::Char('H') => {
                        app.show_help = !app.show_help;
                    }
                    KeyCode::Enter => {
                        if !app.deltas.is_empty() && app.selected_row < app.deltas.len() {
                            let (irq_name, _) = &app.deltas[app.selected_row];
                            app.detail_irq_name = Some(irq_name.clone());
                            app.show_irq_detail = true;
                            app.detail_scroll_offset = 0;
                        }
                    }
                    KeyCode::Esc => {
                        app.show_irq_detail = false;
                        app.detail_irq_name = None;
                        app.detail_scroll_offset = 0;
                    }
                    KeyCode::Char('j') | KeyCode::Char('J') => {
                        if app.show_irq_detail {
                            app.detail_scroll_offset += 1;
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Char('K') => {
                        if app.show_irq_detail {
                            app.detail_scroll_offset = app.detail_scroll_offset.saturating_sub(1);
                        }
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => {
                        if app.show_irq_detail {
                            app.detail_scroll_offset += 10;
                        }
                    }
                    KeyCode::Char('u') | KeyCode::Char('U') => {
                        if app.show_irq_detail {
                            app.detail_scroll_offset = app.detail_scroll_offset.saturating_sub(10);
                        }
                    }
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.update_data()?;
            app.sort_data();
            last_tick = Instant::now();
        }

        if !app.running {
            break;
        }
    }

    Ok(())
}

fn ui(f: &mut Frame, app: &mut App) {
    let size = f.size();
    
    if app.show_irq_detail {
        show_irq_detail(f, app);
        return;
    }
    
    if app.show_help {
        show_help(f);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(size);

    // Header
    let header = Paragraph::new(format!(
        "IRQTop v0.1.0 - Real-time Interrupt Statistics | Update: {:?} ago | Sort: {} | Press 'h' for help",
        app.last_update.elapsed().as_millis(),
        match app.sort_by {
            SortBy::Irq => "IRQ",
            SortBy::Delta => "Delta",
            SortBy::Affinity => "Affinity",
            SortBy::EffectiveAffinity => "Eff. Affinity",
            SortBy::Device => "Device",
        }
    ))
    .style(Style::default().fg(Color::Cyan))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(header, chunks[0]);

    // Table
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);
    let normal_style = Style::default().bg(Color::DarkGray);
    
    let header_cells = vec![
        Cell::from("IRQ"),
        Cell::from("Δ/s"),
        Cell::from("Affinity"),
        Cell::from("Eff. Affinity"),
        Cell::from("Device"),
    ];
    let header = Row::new(header_cells)
        .style(Style::default().fg(Color::Yellow))
        .height(1)
        .bottom_margin(1);

    let default_str = "N/A";
    let rows: Vec<Row> = app
        .deltas
        .iter()
        .enumerate()
        .map(|(i, (irq, delta))| {
            let stats = app.irq_data.get(irq).unwrap();
            let affinity = app.affinity_map.get(irq).map(|s| s.as_str()).unwrap_or(default_str);
            let effective_affinity = app.effective_affinity_map.get(irq).map(|s| s.as_str()).unwrap_or(default_str);
            
            let cells = vec![
                Cell::from(irq.as_str()),
                Cell::from(delta.to_string()),
                Cell::from(affinity),
                Cell::from(effective_affinity),
                Cell::from(stats.name.as_str()),
            ];
            
            if i == app.selected_row {
                Row::new(cells).style(selected_style)
            } else {
                Row::new(cells).style(normal_style)
            }
        })
        .collect();

    let table = Table::new(rows, &[Constraint::Length(8), Constraint::Length(12), Constraint::Length(12), Constraint::Length(15), Constraint::Percentage(40)])
        .header(header)
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(table, chunks[1]);

    // Footer
    let footer = Paragraph::new("q: Quit | ↑/↓: Navigate | Tab: Sort | Enter: Detail | h: Help")
        .style(Style::default().fg(Color::Gray))
        .alignment(ratatui::layout::Alignment::Center);
    f.render_widget(footer, chunks[2]);
}

fn show_help(f: &mut Frame) {
    let help_text = "IRQTop Help\n\nNavigation:\n  ↑/↓     - Move selection up/down\n  PageUp  - Move up 10 rows\n  PageDown- Move down 10 rows\n  Home    - Go to first row\n  End     - Go to last row\n\nSorting:\n  Tab     - Cycle through sort options\n\nDetail View:\n  Enter   - View selected IRQ details\n  Esc     - Return to main view\n  j/k     - Scroll down/up in detail view\n  d/u     - Scroll page down/up in detail view\n\nOther:\n  h       - Toggle this help screen\n  q       - Quit\n  Ctrl+C  - Force quit\n\nPress any key to close this help...";

    let help = Paragraph::new(help_text)
        .style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .title("Help")
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::Yellow)),
        );

    let area = centered_rect(60, 25, f.size());
    f.render_widget(help, area);
}

fn show_irq_detail(f: &mut Frame, app: &mut App) {
    let size = f.size();
    
    if let Some(irq_name) = &app.detail_irq_name {
        if let Some(stats) = app.irq_data.get(irq_name) {
            // Find the delta for this IRQ
            let delta_value = app.deltas.iter()
                .find(|(name, _)| name == irq_name)
                .map(|(_, delta)| *delta)
                .unwrap_or(0);
            
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(0),
                    Constraint::Length(2),
                ])
                .split(size);

            // Header
            let header = Paragraph::new(format!(
                "IRQ Detail: {} ({}) | Total Δ: {} | Total CPUs: {} | Press Esc to return",
                irq_name,
                stats.name,
                delta_value,
                stats.counts.len()
            ))
            .style(Style::default().fg(Color::Cyan))
            .block(Block::default().borders(Borders::ALL));
            f.render_widget(header, chunks[0]);

            // CPU stats table
            let normal_style = Style::default().bg(Color::DarkGray);
            
            let header_cells = vec![
                Cell::from("CPU"),
                Cell::from("Δ"),
                Cell::from("CPU"),
                Cell::from("Δ"),
                Cell::from("CPU"),
                Cell::from("Δ"),
                Cell::from("CPU"),
                Cell::from("Δ"),
            ];
            let header = Row::new(header_cells)
                .style(Style::default().fg(Color::Yellow))
                .height(1)
                .bottom_margin(1);

            // Calculate visible rows and columns
            let available_height = chunks[1].height.saturating_sub(2) as usize;
            let rows_per_column = available_height.saturating_sub(1);
            let total_cpus = stats.counts.len();
            let cpus_per_row = 4;  // 4 CPUs per row
            
            let visible_rows = rows_per_column.min((total_cpus + cpus_per_row - 1) / cpus_per_row);
            let max_scroll = (total_cpus + cpus_per_row - 1) / cpus_per_row;
            let max_scroll = max_scroll.saturating_sub(visible_rows);
            
            // Clamp scroll offset
            app.detail_scroll_offset = app.detail_scroll_offset.min(max_scroll);

            // Get per-CPU deltas for this IRQ
            let per_cpu_deltas = app.per_cpu_deltas.get(irq_name)
                .unwrap_or(&stats.counts); // Fallback to counts if no deltas
            
            // Create rows for visible data
            let mut rows = Vec::new();
            for row_idx in 0..visible_rows {
                let start_cpu = (app.detail_scroll_offset + row_idx) * cpus_per_row;
                if start_cpu >= total_cpus {
                    break;
                }
                
                let mut cells = Vec::new();
                for col in 0..cpus_per_row {
                    let cpu_idx = start_cpu + col;
                    if cpu_idx < total_cpus && cpu_idx < per_cpu_deltas.len() {
                        cells.push(Cell::from(format!("CPU{}", cpu_idx)));
                        cells.push(Cell::from(per_cpu_deltas[cpu_idx].to_string()));
                    } else {
                        cells.push(Cell::from(""));
                        cells.push(Cell::from(""));
                    }
                }
                
                rows.push(Row::new(cells).style(normal_style));
            }

            let table = Table::new(rows, &[
                Constraint::Length(6),  // CPU label
                Constraint::Length(12), // Delta
                Constraint::Length(6),  // CPU label
                Constraint::Length(12), // Delta
                Constraint::Length(6),  // CPU label
                Constraint::Length(12), // Delta
                Constraint::Length(6),  // CPU label
                Constraint::Length(12), // Delta
            ])
                .header(header)
                .block(Block::default().borders(Borders::ALL));

            f.render_widget(table, chunks[1]);

            // Footer with navigation help
            let footer = Paragraph::new(format!(
                "j/k: Scroll down/up | d/u: Page down/up | Scroll: {}/{} | Esc: Return",
                app.detail_scroll_offset + 1,
                max_scroll + 1
            ))
                .style(Style::default().fg(Color::Gray))
                .alignment(ratatui::layout::Alignment::Center);
            f.render_widget(footer, chunks[2]);
        }
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Show { irq_name }) => {
            // For now, fall back to original behavior for show command
            // In a future enhancement, we could add a detailed view
            use std::sync::Mutex;
            use std::sync::OnceLock;
            
            static PREV_STATS: OnceLock<Mutex<Option<IrqStats>>> = OnceLock::new();
            let prev_stats = PREV_STATS.get_or_init(|| Mutex::new(None));
            
            loop {
                let curr_stats = read_interrupts()?.remove(&irq_name)
                    .with_context(|| format!("IRQ {} not found", irq_name))?;
                let cloned_stats = curr_stats.clone();
                
                let deltas = prev_stats.lock()
                    .unwrap()
                    .as_ref()
                    .map(|prev| {
                        cloned_stats.counts.iter()
                            .zip(prev.counts.iter())
                            .map(|(curr, prev)| curr - prev)
                            .collect::<Vec<u64>>()
                    });

                *prev_stats.lock().unwrap() = Some(cloned_stats);

                println!("\x1B[2J\x1B[H");
                println!("CPU Delta Statistics for {}:", irq_name);
                let counts_len = curr_stats.counts.len();
                let deltas: Vec<_> = deltas.unwrap_or_else(|| vec![0; counts_len])        
                    .into_iter()
                    .enumerate()
                    .collect();
                
                // Get terminal dimensions
                let (term_width, term_height) = term_size::dimensions().unwrap_or((80, 24));
                let max_cpu_per_col = (term_height - 4).max(1) as usize; // Reserve 4 lines for headers
                let num_columns = (deltas.len() as f32 / max_cpu_per_col as f32).ceil() as usize;
                let col_width = 20; // 8 for "CPU" column
                
                for col in 0..num_columns {
                    print!("{:<width$}", format!("Δ/s (Col {})", col+1), width = col_width);
                }
                println!("\n{}", "-".repeat(term_width as usize));

                // Print CPU deltas in columns
                for row in 0..max_cpu_per_col {
                    for col in 0..num_columns {
                        let idx = row + col * max_cpu_per_col;
                        if let Some((cpu, delta)) = deltas.get(idx) {
                            print!("{:<8} ", cpu);
                            print!("{:<width$}", delta, width = col_width-8);
                        }
                    }
                    println!();
                }
                
                std::thread::sleep(Duration::from_millis(cli.interval));
            }
        }
        None => {
            // Setup terminal
            enable_raw_mode()?;
            let mut stdout = std::io::stdout();
            execute!(stdout, EnterAlternateScreen)?;
            let backend = CrosstermBackend::new(stdout);
            let mut terminal = Terminal::new(backend)?;

            // Create app
            let mut app = App::default();
            app.update_data()?;
            app.sort_data();

            // Run app
            let tick_rate = Duration::from_millis(cli.interval);
            let res = run_app(&mut terminal, app, tick_rate);

            // Restore terminal
            disable_raw_mode()?;
            execute!(
                terminal.backend_mut(),
                LeaveAlternateScreen,
            )?;
            terminal.show_cursor()?;

            if let Err(err) = res {
                println!("Error: {:?}", err);
            }
        }
    }

    Ok(())
}
