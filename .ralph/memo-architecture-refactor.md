## Goal: Clean up memo's data architecture so there's one source of truth

### Problems to fix
1. **Too many data sources** — sysinfo (RSS), vmmap (Physical footprint), footprint (phys_footprint + swapped), vm_stat (system), top (compressed), ps (fallback). Each measures different things.
2. **ProcessMemory has shifting semantics** — `physical_footprint` sometimes sysinfo RSS, sometimes vmmap's inflated number; three swap fields (`swap_disk`, `swap_disk_est`, `swapped_total`).
3. **Detail popup fetches fresh data via different code path** than the table — guaranteed inconsistency.
4. **Multi-stage scan pipeline** with threads/channels is hard to reason about.

### Target architecture
- **ProcessMemory** with clear semantics: `rss` (sysinfo), `swap` (footprint), `compressed` (footprint). Period.
- **Detail view reads from `app.all_processes[idx]`** — same data the table shows. No re-fetch.
- **One scan path** — sysinfo for all, footprint for big ones (swap only).
- **Kill vmmap** — it causes the inflation (GPU/shared memory).
- **Kill normalize** — already gutted, finish the job.
- **Kill top.rs** — compressed per-process isn't worth the overhead.

### Files to change
- `src/scanner.rs` — simplify ProcessMemory, remove vmmap, remove merge_footprint
- `src/footprint.rs` — keep but simplify, only extract swap
- `src/top.rs` — remove (or gut)
- `src/app.rs` — update to new ProcessMemory, simplify detail lookup
- `src/main.rs` — simplify scan pipeline, detail reads from existing data
- `src/ui.rs` — update column names, remove broken detail label edits
- `src/group.rs` — update field names
- `Cargo.toml` — remove top.rs if gutted

### Checklist
- [ ] Simplify ProcessMemory struct (rss, swap, compressed — clear semantics)
- [ ] Remove vmmap-based get_process_memory from scanner.rs
- [ ] Simplify footprint.rs to only return swap data
- [ ] Detail popup reads from app.all_processes (no re-fetch)
- [ ] Simplify scan pipeline in main.rs (one pass, no merge)
- [ ] Update group.rs field references
- [ ] Update ui.rs labels (RSS not "Physical Footprint")
- [ ] Build and test CLI mode
- [ ] Build and test TUI mode
- [ ] Install binary
