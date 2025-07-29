# IRQTop-RS

A modern, high-performance interrupt monitoring tool for Linux with TUI interface.

## Features

- **Real-time monitoring**: Live updates of interrupt statistics
- **Modern TUI interface**: Built with Ratatui for excellent terminal experience
- **Interactive navigation**: Keyboard controls for browsing data
- **Multiple sorting options**: Sort by IRQ, delta, affinity, or device name
- **CPU affinity display**: Shows both configured and effective CPU affinity
- **Responsive design**: Adapts to terminal size
- **Zero-copy parsing**: Optimized interrupt file reading

## Installation

```bash
# Build from source
./build.sh

# Or manually
cargo build --release
```

## Usage

### Basic Usage
```bash
# Start with default 1-second refresh
./target/release/irqtop-rs

# Custom refresh interval (in milliseconds)
./target/release/irqtop-rs --interval 500
```

### TUI Controls

- **Navigation**: 
  - `↑/↓` - Move selection up/down
  - `Page Up/Down` - Move 10 rows at a time
  - `Home/End` - Jump to first/last row
  
- **Sorting**:
  - `Tab` - Cycle through sort options (IRQ, Delta, Affinity, Effective Affinity, Device)
  
- **Other**:
  - `h` - Toggle help screen
  - `q` or `Ctrl+C` - Quit

### Per-CPU Statistics
```bash
# Show detailed per-CPU stats for a specific IRQ
./target/release/irqtop-rs show 28
```

## Performance

The TUI version provides significant improvements:
- **Reduced screen flicker**: Only redraws changed content
- **Better responsiveness**: Non-blocking input handling
- **Efficient rendering**: Optimized terminal updates
- **Memory efficient**: Minimal allocations during updates

## Screenshots

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ IRQTop v0.1.0 - Real-time Interrupt Statistics | Update: 250ms ago | Sort: │
├────┬────────────┬────────────┬──────────────┬───────────────────────────────┤
│IRQ │Δ/s         │Affinity    │Eff. Affinity │Device                         │
├────┼────────────┼────────────┼──────────────┼───────────────────────────────┤
│28  │12345       │0-3         │0-3           │eth0                           │
│29  │5678        │4-7         │4-7           │eth1                           │
│30  │234         │0-15        │0-15          │ahci[0000:00:1f.2]            │
└────┴────────────┴────────────┴──────────────┴───────────────────────────────┘
```

## Requirements

- Linux kernel with `/proc/interrupts` support
- Terminal with Unicode support
- Rust 1.70+

## License

MIT