# lean-top

A lightweight terminal dashboard for Lean builds on 64-bit Ubuntu Linux. It shows server resources, inspectable Lean/Lake processes, distinct completed Lean modules against a fixed target total, estimated build times, and wrapping build logs. Mouse wheels navigate individual panels, and dragging section dividers adjusts their heights.

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

## Update an existing installation

From your `lean-top` checkout:

```sh
git pull --ff-only
cargo build --release --offline
./target/release/lean-top --demo
```

If you run the Cargo-installed `lean-top` command, also replace that executable:

```sh
cargo install --path . --force --offline
```

Building `target/release/lean-top` alone does not update a previously installed copy. Run the rebuilt executable or reinstall before checking new behavior. If you copied the binary elsewhere, replace that copy too; `command -v lean-top` identifies the executable selected by your shell.

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

`--project` defaults to the current directory (`.`), selects the filesystem for storage capacity, and supplies the directory shown in the header. It does not filter the server-wide resource metrics or process list. A relative `--log` path is resolved against the monitor's working directory, independently of `--project`. An explicitly selected log may be created after the monitor starts; missing files are retried.

When `--log` is omitted, startup selects the first regular, non-hidden `*.log` file in `~/logs`, ordered by filename. This is a nonrecursive lookup and does not mean the newest file. An explicit `--log` overrides discovery. If no matching file or home directory is available, system monitoring still starts without a log. Demo mode never selects a real log automatically.

Examples using the defaults:

```sh
cd /srv/riemann
lean-top --log ./build.log
# Use this project and the first filename-sorted ~/logs/*.log:
lean-top
```

Automatic discovery runs once at startup. It does not switch to a different file when another log appears. For a build that has not created its log yet, select its future path explicitly.

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
████████████████████████░░░░░░░░░░░░░░░░░░░░░░░░       34 / 120
Est. time ≈ 3m 0s total · ≈ 2m 9s left
Log: simulated build.log · last read 12:34:56 UTC
Latest: Running RiemannHypothesis.ExplicitFormula

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

q Quit  Space Pause  Wheel Scroll  Click/p Details  ↑↓ Logs  f Follow  ? Help
```

The full layout needs a terminal of at least **91 columns × 32 rows**. Smaller terminals use a compact layout with resource rows, build status, and logs. Its drawing area needs 52 columns × 20 rows, so use a terminal of at least **53 columns × 20 rows**: one terminal column is reserved to prevent wrapping. Compact mode hides the process table and initially shows core resource values; expanding Resources reveals additional details. Build always retains its status, module progress, and estimated total/remaining time row. The Build counter uses only `completed / total`, without `files`, `fixed`, `extra`, or a duplicate file-summary line. Unknown counts appear as `— / —`. Resizing can hide log-source information and latest-event captions.

Unchanged log rows stay on screen while resource metrics continue to refresh. The log's last-read time is a fixed UTC timestamp that changes only when new bytes are read, rather than a continuously advancing age counter. Scrolling, help, and terminal resizing can still redraw the affected area.

Log text wraps to the panel width, including when the terminal narrows. Scroll through display rows to inspect every retained part of a long record; continuation rows keep the original severity color. The live tail keeps 200 records; scrolling up reads older records from the original log file on demand, so scrollback is not limited to those 200 records. The 4 KiB raw-byte display limit per record still applies. CPU shows its current utilization bar; the history graph has been removed.

## Controls and options

| Key | Action |
| --- | --- |
| Mouse wheel over Processes | Scroll that process list up/down; full layout only |
| Mouse wheel over Build Log | Scroll older/newer retained log lines |
| Left-drag a section divider | Resize the sections immediately above and below it |
| Click a process row | Open PID, memory, full target path, and command details |
| `p` or `Enter` | Open details for the first visible process, or close an open details panel |
| `Esc` | Close help first, or close process details |
| Click outside details | Close details without operating the background panel |
| `q` or `Ctrl-C` | Quit the monitor, including from details; the separate build continues |
| `Space` | Pause/resume metric sampling and log reading |
| `↑` / `k` | Scroll older log display rows, or scroll up in open process details |
| `↓` / `j` | Scroll newer log display rows, or scroll down in open process details |
| `f` | Resume following the latest log output when details/help are closed |
| `?` | Toggle keyboard help |

Hover over the panel you want to scroll; clicking to focus is unnecessary. The two panels keep independent positions. Process scrolling leaves log following unchanged. Log scrolling disables automatic following and preserves the reading position while new output arrives. Scroll down to the live end or press `f` to resume following. Older content is read from the selected log file in 64 KiB pages, including output written before the monitor started; no duplicate history file is created. Mouse scrolling also works while sampling is paused and is ignored while help is open. It requires a terminal that forwards XTerm-compatible mouse reports. The log's existing keyboard controls remain available. Unchanged visible log text is left on screen while system metrics update. Repeated Lake spinner frames with the same progress content do not append history. New or changed log text updates only the affected character spans without clearing rows; scrolling, reflow, overlays, and panel movement redraw the affected content.

The process table shows only type, memory, and a short target name. Open details to read full paths and command arguments, using the wheel or arrow keys to scroll long values. Details track the selected PID across sorting changes and label the last captured data if that process is no longer visible. Metrics continue updating behind the panel. Compiler targets are detected from kernel-provided arguments; relative paths are resolved against the process working directory when permissions allow. Lake rows show their command label, such as `build`, rather than a fabricated source filename.

Opening details never suspends the build or changes either background panel's scroll position. While details are open, wheel events over that dialog scroll its contents and leave the background alone. Close details before dragging dividers. `p`/`Enter` can open the first visible process only when the process table is available and nonempty.

Hold the left mouse button on a section divider and drag up or down. Each divider redistributes space between the two adjacent sections:

| Divider | Sections resized |
| --- | --- |
| `BUILD` | Resources above it and Build below it |
| `PROCESSES` | Build above it and Processes below it |
| `BUILD LOG` | Processes above it and Build Log below it; in compact mode, Build and Build Log |

CPU, Memory, and Storage share one resource row and resize together. Only the application header and footer remain fixed. Minimum heights preserve resource values/bars, build status/module progress and the time estimate (four Build rows), at least one process row with table headings, and three log rows. Optional resource details and build captions appear only when their section has room. Narrow layouts can also reveal load, swap, and available/free space by expanding Resources. Heights are retained for the current session and clamped to fit smaller terminals. Resizing preserves log scroll/follow state and adjusts process scroll bounds to the new viewport.

| Option | Meaning |
| --- | --- |
| `--project PATH` | Project source root for automatic module counting, storage, and header; defaults to `.` |
| `--log PATH` | Existing or future build-log file; default is the first filename-sorted `~/logs/*.log` |
| `--module-list PATH` | Optional verified module list; defaults to `PROJECT/lean-top.modules` when present, otherwise counts project source filenames |
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
- **ACTIVE/IDLE** reflects whether any of those processes were detected. **Build** displays distinct successfully compiled target modules divided by a total calculated once from module data. It does not use Lake's job index or require a numeric total argument.

**Automatic counting:** on startup, a background worker walks `--project` once and records the module names represented by `.lean` filenames. It does not read source contents, parse imports, read `.olean` artifacts, or run Lake. Hidden directories (including `.lake` and `.git`), `lakefile.lean`, symlink directories, and nonregular sources are excluded. Visible source directories are included. The number of unique discovered modules becomes the denominator and remains fixed during that run. Source or list edits require restarting the application; the catalog is reused across log rotation. The total is not hardcoded; a 175,187-module tree is supported without the old 20,000-module cap.

```sh
lean-top --project /srv/riemann --log /srv/riemann/build.log
```

This automatic scope is appropriate when the selected build, such as your full-project `Main`, compiles all project modules. It is not a claim that every Lean project always builds all its source files. For a subset target, custom source-root layout, or external modules, provide the actual target module list instead:

```text
Main
RiemannHypothesis.Zeta
RiemannHypothesis.ExplicitFormula
```

The example is a three-module list; use your complete actual list. Save it as `lean-top.modules` in the project for automatic selection, or pass `--module-list /path/to/modules.txt`. One dotted module name per line is accepted, with blank lines and `#` comments. The denominator is calculated from unique entries; repeated names do not inflate it. No manually supplied numeric denominator is used. Lists and inventory support up to 1,000,000 modules; directory walks inspect at most 2,000,000 entries and lists are limited to 64 MiB. Invalid names, unreadable directories, or limits report an error rather than a truncated, misleading total.

**Restart-safe completion count:** startup streams the selected log from its beginning in a background worker. Only successful `Built Module.Name` records in the calculated/listed scope count, once per module. Native `:c.o` facets, `Replayed`/`Reused` work, running jobs, diagnostics, failed records, and non-target modules do not count. The worker hands its file position and unfinished record to the live reader, which consumes at most 64 KiB of new bytes per refresh without losing completions from the 200-record display ring. Restarting with the same sources/list and build log restores the same count. No checkpoint or duplicate log file is written.

Use one log per build. Appending multiple runs to one file combines their distinct compiled modules; deleted or overwritten completion records cannot be recovered. Detected rotation/truncation resets the run and initializes the new file. An incomplete inventory or replay is shown as an error, not a fabricated count. For an incremental build, the whole project's module total is not necessarily the number that will actually compile; choose the scope appropriate to the build you are measuring.

**Estimated time:** after replay, the restored count becomes a baseline. New unique completions establish the rate after at least one second; historical replay speed is never treated as compiler speed. Up to 32 observations estimate total time as `calculated total / rate` and remaining time as `(calculated total - completed) / rate`. No numeric forecast is invented before that rate is measurable. Counts are restored across restarts; only time observations restart.

Lake's `[reported/total]` numbers can include native and cache jobs and may grow during discovery. They remain visible in raw Build Log records, but do not drive Build's module ratio. A full ratio is not proof of a successful build exit or theorem validity; use Lake's actual exit status and your verification workflow.

## Resource design

The monitor reads `/proc` and filesystem metadata directly instead of spawning `top`, `ps`, or `df` on every refresh. It sleeps between updates and redraws changed terminal rows. Log ingestion is bounded to a recent 64 KiB window per refresh, with 200 completed lines retained and a 4 KiB raw-byte limit per line, plus a partial line. Live ingestion may skip older output to keep up, but scrolling can retrieve it while it remains in the original file. The scrollback cache retains at most 4096 records and 256 KiB of text plus record metadata; it loads pages only during navigation. Detected rotation or truncation returns the view to the current log.

The monitor makes no network requests and does not persist metrics, logs, or configuration. Source filenames or a target list are read once and completion history is replayed once at startup; there is no recurring source-tree or artifact-directory walk. Module names map to indexed completion bits rather than duplicate completed-name strings. Build-log writes in the example come from `tee`. Actual CPU and memory costs depend on process count, log volume, terminal size, and refresh interval; no fixed overhead budget is guaranteed.

## Development

```sh
cargo fmt --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
cargo build --release --offline
python3 tests/terminal_smoke.py target/release/lean-top
```

Unit tests cover synthetic `/proc` sampling, argument/target handling, log retention and wrapped scrolling, CLI defaults, pane resizing, details, and estimator state transitions. The Python smoke test exercises the actual executable through a Unix PTY, including automatic/explicit log selection, process details, every divider, narrow-terminal wrapping, shutdown/restoration, and fixed module totals, duplicate/native exclusion, and restart recovery against a 175,187-module list without source files or artifact writes. Python 3 is only needed for this development check, not for running the monitor.

GitHub Actions runs the checks on Ubuntu 24.04 and macOS, plus a Rust 1.70 compatibility check on Ubuntu. The Ubuntu checks also exercise live CPU, memory, and filesystem sampling. Test live sampling on your deployment server when its permissions or environment differ.

See [architecture](docs/architecture.md) for implementation details and [troubleshooting](docs/troubleshooting.md) for operational issues.
