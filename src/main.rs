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
                    affinity_map.insert(irq.trim().to_string(), affinity.trim().to_string());
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
    let (width, height) = term_size::dimensions().unwrap_or((80, 120));
    let max_rows = (height - 4).max(1); // Reserve 4 lines for headers
    
    print!("\x1B[0J");
    println!("Real-time Interrupt Statistics with Affinity");

    let interrupts = read_interrupts().unwrap();
    let affinity_map = get_affinity_map();
    let effective_affinity_map = get_effective_affinity_map();
    // Create header, EAffinity means Effective Affinity
    println!(
        "{:<8} {:<10} {:<12} {:<12} {:<80}",
        "IRQ", "Δ/s", "Affinity", "EAffinity", "Device"
    );
    println!("{}", "-".repeat(width as usize));

    // Display rows with truncation based on terminal height
    for (irq, delta) in sorted.iter().take(max_rows) {
        let irq_str = irq.trim();
        let stats = interrupts.get(irq_str).unwrap();
        let affinity = affinity_map.get(irq_str).cloned().unwrap_or("N/A".to_string());
        let effective_affinity = effective_affinity_map.get(irq_str).cloned().unwrap_or("N/A".to_string());
        println!(
            "{:<8} {:<10} {:<12} {:<12} {:<80}",
            irq_str, delta, affinity, effective_affinity, stats.name, 
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
