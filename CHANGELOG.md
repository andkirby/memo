# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.0] - 2026-06-11

### Added
- Interactive TUI with ratatui — grouped process table, segmented memory bars, detail popup
- CLI mode (`--cli` for grouped, `--ps` for ungrouped)
- Process grouping by app name (Chrome, Electron, Node, Python, JetBrains, etc.)
- System memory header: RAM and Swap segmented bars with App/Wired/Compressed/Cache breakdown
- Health status indicator (OK/WARNING/CRITICAL) based on swap and RAM pressure
- Per-process detail popup showing RSS, swap, command line, CWD, exe, parent process
- Sort by Total, RSS, Swap, Name (T/P/S/N keys)
- Mouse support: scroll, click to select, double-click to open detail, right-click to expand
- btop-style segmented header bars with per-category colors
- Total column color gradient (teal → green → yellow → orange → red)
- Process kill via X key
- Parallel `footprint` invocations (batches of 20 × 4 concurrent)
- Dynamic page size detection (`hw.pagesize`) for Intel and Apple Silicon
- Swap accounting note showing unaccounted swap from tiny processes

### Fixed
- Corrected process selection in expanded groups (D key showed wrong PID)
- Fixed page size hardcoded to 4096 (4× error on Apple Silicon which uses 16384)
- Fixed system RAM calculation to exclude file-backed cache (now matches Activity Monitor within ~2%)
- Used "Pages occupied by compressor" instead of "Pages stored in compressor" (physical vs virtual)
- Removed swap normalization that inflated per-process swap by 3-6×
- Removed vmmap-based physical footprint that included shared GPU memory (showing 34 GB for Opera on 16 GB machine)

### Architecture
- Single source of truth: RSS from sysinfo, swap from footprint
- Detail popup reads from scanned data (no re-fetch for memory numbers)
- 3-step scan pipeline: system memory → process list → swap deep scan
- Removed vmmap, top.rs, swap normalization from critical path
