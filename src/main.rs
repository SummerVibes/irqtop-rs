use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use std::result::Result::Ok;
/// Interrupt statistics
#[derive(Debug, Default, Clone)]
struct IrqStats {
    counts: Vec<u64>,
    name: String,
}

/// Paurse command-line arguments
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Refresh interval in seconds
    #[arg(short, long, default_value_t = 1)]
    interval: u64,
    
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show per cpu stats for a single IRQ
    Show { irq_name: String },
}

/// read /proc/interrupts
fn read_interrupts() -> Result<HashMap<String, IrqStats>> {
    let content = fs::read_to_string("/proc/interrupts")?;
    let mut irq_map = HashMap::new();

    let mut lines = content.lines();
    let _header = lines.next().context("Missing header line")?; // Skip CPU header

    for line in lines {
        // Split line into IRQ number and remaining parts
        let (irq_part, rest) = line.trim().split_once(|c: char| c.is_whitespace())
            .context(format!("Invalid line format: {}", line))?;
        
        // Extract IRQ number (remove trailing colon)
        let irq_num = irq_part.trim_end_matches(':').to_string();
        // Split into numeric columns and description
        let mut parts = rest.split_whitespace().peekable();
        let mut counts = Vec::new();
        
        // Parse CPU counts until we hit non-numeric value
        while let Some(p) = parts.peek() {
            // Handle comma-separated values (e.g., "1,234")
            let num_str = p.replace(',', "");
            if num_str.parse::<u64>().is_ok() {
                counts.push(num_str.parse::<u64>().unwrap());
                parts.next();
            } else {
                break;
            }
        }

        // The remaining parts are the interrupt description
        let name = parts.collect::<Vec<&str>>().join(" ");
        if name.is_empty() {
            continue; // Skip lines without description
        }

        if !counts.is_empty() {
            irq_map.insert(
                irq_num,
                IrqStats {
                    counts,
                    name,
                },
            );
        }
    }

    Ok(irq_map)
}

fn calculate_delta(old: &HashMap<String, IrqStats>, new: &HashMap<String, IrqStats>) -> HashMap<String, u64> {
    let mut deltas = HashMap::new();
    for (irq, new_stats) in new {
        if let Some(old_stats) = old.get(irq) {
            let delta: u64 = new_stats.counts.iter()
                .zip(old_stats.counts.iter())
                .map(|(n, o)| n - o)
                .sum();
            deltas.insert(irq.clone(), delta);
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
                    affinity_map.insert(irq.to_string(), affinity.trim().to_string());
                }
            }
        }
    }
    affinity_map
}

/// Display combined delta and affinity information
fn show_combined_stats(deltas: &HashMap<String, u64>) {
    print!("\x1B[?1049h");
    let mut sorted: Vec<_> = deltas.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));

    // Get terminal dimensions
    let (width, height) = term_size::dimensions().unwrap_or((80, 100));
    let max_rows = (height - 4).max(1); // Reserve 4 lines for headers
    
    print!("\x1B[0J");
    println!("Real-time Interrupt Statistics with Affinity");

    let interrupts = read_interrupts().unwrap();
    let affinity_map = get_affinity_map();
    let effective_affinity_map = get_effective_affinity_map();

    // Create header
    println!(
        "{:<8} {:<10} {:<50} {:<12} {:<12}",
        "IRQ", "Δ/s", "Device", "Affinity", "Effective Affinity"
    );
    println!("{}", "-".repeat(width as usize));

    // Display rows with truncation based on terminal height
    for (irq, delta) in sorted.iter().take(max_rows) {
        let stats = interrupts.get(*irq).unwrap();
        let affinity = affinity_map.get(*irq).cloned().unwrap_or_default();
        let effective_affinity = effective_affinity_map.get(*irq).cloned().unwrap_or_default();
        println!(
            "{:<8} {:<10} {:<50} {:<12} {:<12}",
            irq, delta, stats.name, affinity, effective_affinity
        );
    }
}

/// Display combined delta and affinity information
fn show_cpu_stats(irq_name: &str) -> Result<()> {
    use std::sync::Mutex;
    use std::sync::OnceLock;
    
    static PREV_STATS: OnceLock<Mutex<Option<IrqStats>>> = OnceLock::new();
    let prev_stats = PREV_STATS.get_or_init(|| Mutex::new(None));
    
    let curr_stats = read_interrupts()?.remove(irq_name)
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
    println!("{:<8} {:<16}", "CPU", "Δ/s");

    let counts_len = curr_stats.counts.len();
    let deltas: Vec<_> = deltas.unwrap_or_else(|| vec![0; counts_len])        .into_iter()
        .enumerate()
        .collect();
    
    // Get terminal dimensions
    let (term_width, term_height) = term_size::dimensions().unwrap_or((80, 24));
    let max_cpu_per_col = (term_height - 4).max(1) as usize; // Reserve 4 lines for headers
    let num_columns = (deltas.len() as f32 / max_cpu_per_col as f32).ceil() as usize;
    let col_width = 20; // 8 for "CPU" column
    
    // Print header
    println!("\nInterrupt: {:<8}", irq_name);
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
    
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Show { irq_name }) => {
            loop {
                show_cpu_stats(&irq_name)?;
                std::thread::sleep(Duration::from_secs(cli.interval));
            }
        }
        None => {
            let mut prev_stats = read_interrupts()?;
            loop {
                // Update and display stats
                let curr_stats = read_interrupts()?;
                let deltas = calculate_delta(&prev_stats, &curr_stats);
                show_combined_stats(&deltas);
                prev_stats = curr_stats;
                
                std::thread::sleep(Duration::from_secs(cli.interval));
            }
        }
    }
}
