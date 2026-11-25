# lean-top

A lightweight terminal dashboard for Lean builds on 64-bit Ubuntu Linux. It shows server CPU, memory, swap, project-filesystem storage, Lean/Lake processes, Lake progress, and a live build-log tail.

Written in Rust with **no Cargo dependencies**. The default refresh interval is **500 ms**. The monitor reads system information and an existing log; it does not launch, stop, or modify your build.

## Build and preview

Requires Rust **1.70 or newer** and Cargo. Interactive mode also uses the system `stty` command and a UTF-8 terminal with ANSI cursor support.

```sh
git clone https://github.com/tsmvision/lean-top.git
cd lean-top
cargo build --release --offline
./target/release/lean-top --demo
```

For a plain-text preview without an interactive terminal:

```sh
./target/release/lean-top --demo --snapshot
```

`--snapshot` prints one 110-column, 38-row frame and exits. macOS can be used for the demo and development; real system metrics target Linux.

Optionally install the executable into Cargo's binary directory:

```sh
cargo install --path . --offline
lean-top --demo
```

## Monitor a Lean project

Start the build in one terminal. Use Bash so that `pipefail` preserves a failing build's exit status through `tee`:

```bash
cd /srv/riemann
set -o pipefail
lake build 2>&1 | tee build.log
```

In another terminal:

```sh
lean-top --project /srv/riemann --log /srv/riemann/build.log
```

`tee build.log` truncates the file at the start of a run. Prefer a unique log filename for each run when retaining logs or switching between builds. Pass that same filename to `--log`.

`--project` selects the filesystem for storage capacity and the directory shown in the header. A relative `--log` path is resolved against the monitor's working directory, independently of `--project`. The log file may be created after the monitor starts; missing files are retried.

When `--log` is omitted, startup selects the first regular, non-hidden `*.log` file in `~/logs`, ordered by filename. This is a nonrecursive lookup and does not mean the newest file. An explicit `--log` overrides discovery. If no matching file or home directory is available, system monitoring still starts without a log. Demo mode never selects a real log automatically.

## Screen

Abbreviated demo snapshot; all values below are simulated:

```text
LEAN TOP              Lean build monitor                     ● DEMO  0.5s
/srv/riemann           ·   ubuntu / 16 cores

CPU                   MEMORY                         STORAGE
76.0%                 11.6 GiB / 32.0 GiB             84.0 GiB / 240.0 GiB
████████████░░░░      ██████░░░░░░░░░░░░             █████░░░░░░░░░░░░
Load 6.42 / 5.81 / 4.29  Available 20.4 GiB            Free 156.0 GiB
                      Swap 0 MiB / 4.0 GiB

BUILD
● ACTIVE    6 Lean/Lake processes · server-wide
Est. Lean files ≈ 120 · done 34 · demo estimate
████████████████████████░░░░░░░░░░░░░░░░░░░░░░░░       342 / 680 jobs
Est. time ≈ 3m total · ≈ 2m 9s left
Latest event   ⣾ [342/680] Running RiemannHypothesis.ExplicitFormula

PROCESSES · 1-6 / 6
TYPE     MEMORY       TARGET
lean     1.8 GiB      ExplicitFormula.lean
lean     1.2 GiB      ZetaBounds.lean
lean     800 MiB      ZeroFreeRegion.lean
lean     600 MiB      CriticalStrip.lean
leanc     96 MiB      ZetaBounds.c
lake      84 MiB      build

BUILD LOG
✔ [340/680] Built RiemannHypothesis.ZetaBounds
warning: ExplicitFormula.lean:128: unused variable `h`
⣾ [342/680] Running RiemannHypothesis.ExplicitFormula

q Quit   Space Pause   ↑↓/jk Scroll   f Follow   ? Help
```

The full layout needs a terminal of at least **91 columns × 32 rows**. Smaller terminals use a compact layout with resource rows, build status, and logs. Its drawing area needs 52 columns × 20 rows, so use a terminal of at least **53 columns × 20 rows**: one terminal column is reserved to prevent wrapping. The compact layout omits the process table and resource detail rows.

Unchanged log rows stay on screen while resource metrics continue to refresh. The log's last-read time is a fixed UTC timestamp that changes only when new bytes are read, rather than a continuously advancing age counter. Scrolling, help, and terminal resizing can still redraw the affected area.

Log text wraps to the panel width, including when the terminal narrows. Scroll through display rows to inspect every retained part of a long record; continuation rows keep the original severity color. The original 200-record/4 KiB-per-record retention limits still apply. CPU shows its current utilization bar; the history graph has been removed.

## Controls and options

| Key | Action |
| --- | --- |
| Mouse wheel over Processes | Scroll that process list up/down; full layout only |
| Mouse wheel over Build Log | Scroll older/newer retained log lines |
| Left-drag a section divider | Resize the sections immediately above and below it |
| Click a process row | Open PID, memory, full target path, and command details |
| `p` or `Enter` | Open details for the first visible process, or close an open details panel |
| `Esc` or click outside details | Close the process details panel |
| `q` or `Ctrl-C` | Quit the monitor; the separate build continues |
| `Space` | Pause/resume metric sampling and log reading |
| `↑` / `k` | Scroll toward older retained log lines |
| `↓` / `j` | Scroll toward newer retained log lines |
| `f` | Resume following the latest output |
| `?` | Toggle keyboard help |

Hover over the panel you want to scroll; clicking to focus is unnecessary. The two panels keep independent positions. Process scrolling leaves log following unchanged. Log scrolling disables automatic following; press `f` to return to the live tail. Mouse scrolling also works while sampling is paused and is ignored while help is open. It requires a terminal that forwards XTerm-compatible mouse reports. The log's existing keyboard controls remain available.

The process table shows only type, memory, and a short target name. Open details to read full paths and command arguments, using the wheel or arrow keys to scroll long values. Details track the selected PID across sorting changes and label the last captured data if that process is no longer visible. Metrics continue updating behind the panel. Compiler targets are detected from kernel-provided arguments; relative paths are resolved against the process working directory when permissions allow. Lake rows show their command label, such as `build`, rather than a fabricated source filename.

Hold the left mouse button on a section divider and drag up or down. Each divider redistributes space between the two adjacent sections:

| Divider | Sections resized |
| --- | --- |
| `BUILD` | Resources above it and Build below it |
| `PROCESSES` | Build above it and Processes below it |
| `BUILD LOG` | Processes above it and Build Log below it; in compact mode, Build and Build Log |

CPU, Memory, and Storage share one resource row and resize together. Only the application header and footer remain fixed. Minimum heights preserve resource values/bars, build status/progress, at least one process row with table headings, and three log rows. Optional resource details and build captions appear only when their section has room. Narrow layouts can also reveal load, swap, and available/free space by expanding Resources. Heights are retained for the current session and clamped to fit smaller terminals. Resizing preserves log scroll/follow state and adjusts process scroll bounds to the new viewport.

| Option | Meaning |
| --- | --- |
| `--project PATH` | Existing project directory; defaults to the current directory |
| `--log PATH` | Existing or future build-log file; default is the first filename-sorted `~/logs/*.log` |
| `--interval-ms MS` | Refresh interval from 100 to 60000 ms; default 500 |
| `--demo` | Simulated resources, processes, and build output; cannot be combined with `--log` |
| `--snapshot` | Print a single plain-text frame and exit |
| `--no-color` | Disable colors; interactive mode still requires ANSI cursor support |
| `-h`, `--help` | Show usage |
| `-V`, `--version` | Show version |

The `NO_COLOR` environment variable also disables colors. For a slower refresh rate:

```sh
lean-top --project /srv/riemann --log /srv/riemann/build.log --interval-ms 2000
```

## Reading the numbers

- **CPU** is aggregate server utilization calculated from consecutive `/proc/stat` samples, on a 0–100% scale. The first sample has no rate yet. Load averages cover 1, 5, and 15 minutes.
- **Memory** is `MemTotal - MemAvailable`, so reclaimable memory is treated as available. Process memory is resident memory (RSS).
- **Storage** is capacity on the filesystem containing `--project`, not the size of the project directory. “Free” means space available to the current user and can differ from total minus used because of reserved blocks.
- **Processes** include server-wide processes named `lean`, `lake`, or `leanc`, sorted by RSS. By default the pane grows to show all entries that fit, reducing optional summary space if necessary while preserving the log minimum. Excess entries remain scrollable when terminal space runs out. Dragging a divider sets a manual height for the session. The heading shows the visible range and total. Processes are live and not filtered by project.
- **ACTIVE/IDLE** reflects whether any of those processes were detected. **Lake jobs** preserves the latest `[reported/total]` pair from a recognized Lake status record, including a running spinner before its trailing newline. “Latest event” shows a recent retained Lake status record.

Lake counts build jobs, not `.lean` source files. One module can produce several jobs (including native-code facets), and cached or replayed work also contributes to Lake's reporting. Its total can grow as dependency jobs are discovered, so the displayed fraction can decrease while the build advances. The monitor preserves Lake's reported numbers without adding one, counting source files, or fixing the denominator. Before a status record arrives, the job count remains unknown. These semantics follow [Lake's build runner](https://github.com/leanprover/lean4/blob/v4.33.1/src/lake/Lake/Build/Run.lean#L53-L164).

**Estimated Lean compilation files and time:** one bounded background scan inventories local sources and reachable imports, checks source/standard `.olean` timestamps, and conservatively propagates stale dependencies to their importers. The initial set is frozen for the observed run and can expand if real compilations contradict it. Recognized source-module `Built` events update completion counts. Time estimates learn from observed completion throughput and reported module timings; cold starts show `Learning` until samples exist. No second Lake build or recurring source-tree scan is launched.

All predictions use `≈` and a confidence/scope label. They are not Lake's exact build plan: dynamic/custom target configuration, generated modules, cache layout, and hash-based dependency decisions can change actual work. The scan uses a local/import fallback rather than executing `lakefile.lean`. The displayed total time estimates the analyzed Lean compilation work from the monitor's observation window, not a recovered start time for a build you joined midway, and it excludes unrelated native/link jobs. See [Lake's module build decisions](https://github.com/leanprover/lean4/blob/master/src/lake/Lake/Build/Module.lean#L1193-L1248).

Log progress can belong to an earlier run and does not supply a build exit code. A full progress bar does not establish a successful build or a valid theorem proof. This tool does not inspect Lean declarations, axioms, `sorry`, or proof artifacts.

## Resource design

The monitor reads `/proc` and filesystem metadata directly instead of spawning `top`, `ps`, or `df` on every refresh. It sleeps between updates and redraws changed terminal rows. Log ingestion is bounded to a recent 64 KiB window per refresh, with 200 completed lines retained and a 4 KiB raw-byte limit per line, plus a partial line. Older output may be skipped to keep up.

The monitor makes no network requests and does not persist metrics, logs, or configuration. Build-log writes in the example come from `tee`. Actual CPU and memory costs depend on process count, log volume, terminal size, and refresh interval; no fixed overhead budget is guaranteed.

## Development

```sh
cargo fmt --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
cargo build --release --offline
python3 tests/terminal_smoke.py target/release/lean-top
```

Unit tests use synthetic `/proc` data and temporary logs to cover sampling, progress, log rotation and bounds, CLI validation, and rendering. The Python smoke test exercises the actual executable through a Unix PTY, including keyboard controls, resizing, shutdown, and terminal restoration. Python 3 is only needed for this development check, not for running the monitor.

GitHub Actions runs the checks on Ubuntu 24.04 and macOS, plus a Rust 1.70 compatibility check on Ubuntu. The Ubuntu checks also exercise live CPU, memory, and filesystem sampling. Test live sampling on your deployment server when its permissions or environment differ.

See [architecture](docs/architecture.md) for implementation details and [troubleshooting](docs/troubleshooting.md) for operational issues.
