# Architecture

`lean-top` is a Rust terminal application targeting 64-bit Ubuntu Linux. It requires Rust 1.70 or newer and has no Cargo dependencies. It monitors an independently running build through Linux system information and an optional log file.

## Modules

| File | Responsibility |
| --- | --- |
| `src/main.rs` | CLI validation, sampling schedule, keyboard dispatch, demo generation, snapshot output |
| `src/input.rs` | Bounded keyboard, SGR mouse, and legacy X10 input decoding |
| `src/monitor.rs` | System snapshots, Lean process discovery, bounded log tailing, output sanitization, progress parsing |
| `src/estimate.rs` | Bounded background source/import analysis and timing/throughput estimates |
| `src/ui.rs` | Shared layout and mouse hit testing, terminal cells, wrapped logs, process details, independent panel scrolling, row-difference rendering |
| `src/terminal.rs` | TTY setup/restoration, terminal dimensions, alternate screen, shutdown signals |
| `src/monitor_tests.rs` | Synthetic system and log tests |

## Update loop

The main thread samples immediately, draws a frame, and waits for keyboard input or the next refresh. The default interval is 500 ms, configurable from 100 to 60000 ms. A bounded input channel carries bytes from a blocking stdin-reader thread. The main loop checks for shutdown and resize with waits capped at 100 ms; metrics are sampled on the configured interval.

Pausing suspends both system sampling and log reading. It does not suspend the independently running build. Resuming may skip older log data if the file has advanced beyond the bounded tail window.

Snapshot mode prints one 110 × 38 plain-text frame without changing terminal mode. A live snapshot takes a second system sample after 100 ms to obtain a CPU delta. Demo mode supplies simulated values and output without reading live resource metrics.

`--project` is canonicalized from the selected path or current working directory. It determines storage and estimate scope, while CPU/memory/process discovery remains server-wide. `--log` is independent of that path: relative explicit paths use the launch directory. Without an explicit path, startup chooses the filename-minimum regular nonhidden `.log` directly under `$HOME/logs`. Discovery is read-only, happens once, and is bypassed by demo mode. A missing match leaves logs unselected; an explicitly selected future file is retried by the tail reader.

## System information

CPU utilization uses differences between aggregate `/proc/stat` counters. Idle includes idle and I/O-wait ticks. Guest counters are excluded from the sum because they are already included in user/nice time. No rate is reported before two valid samples or when total ticks have not advanced.

Memory uses `MemTotal` and `MemAvailable` from `/proc/meminfo`. Swap uses `SwapTotal` and `SwapFree`. Load averages come from `/proc/loadavg`.

Process discovery visits numeric `/proc` entries and matches `comm` against `lean`, `lake`, and `leanc`. It reads RSS from `status` and at most 4096 bytes of `cmdline`. NUL-separated argument boundaries are retained for best-effort source detection, skipping known output and option values. Display commands quote whitespace/empty/apostrophe-containing arguments; no command is executed. Relative compiler targets are joined to a readable `/proc/PID/cwd`; when it is inaccessible, the supplied relative path is retained. Lake targets are command labels.

Results are sorted by RSS. Automatic layout sizes Processes for all matching entries that fit, reducing optional summary space before clipping the list; the log minimum is preserved. Manual divider heights override automatic sizing. The concise table shows type, RSS, and target basename. Details use the selected PID to look up live data and retain one cached row for the no-longer-visible case. Long fields wrap and scroll independently of background panels. Process exits, sorting, empty lists, and inaccessible details are tolerated. Discovery is server-wide and does not associate processes with `--project`.

Storage calls `statvfs` directly for the project directory. The implementation uses a 64-bit Linux ABI declaration; unsupported targets report unavailable storage. Used space excludes filesystem free blocks, while the displayed free space uses blocks available to an unprivileged user. The monitor does not recursively measure project or `.lake` directory sizes, and it does not measure disk I/O throughput.

## Log ingestion

The monitor opens the log each refresh, identifies it by device/inode, and seeks to its previous byte position. A changed identity or an observed size decrease resets retained output and progress. Missing files are retried on later refreshes. Rotation detection does not guarantee detection of a same-inode truncate-and-regrow between samples; separate log filenames per build avoid that ambiguity.

Reads are limited to the most recent 64 KiB window. If the unread gap exceeds that window, the reader moves forward, discards the initial partial line, and adds a skipped-output message. It retains up to 200 completed lines and up to 4096 raw bytes for the current line. Bytes beyond a line's limit are discarded until a delimiter arrives. A partial line can be displayed before it is completed.

Newline and carriage return both delimit records, supporting Lake progress updates. Raw pending bytes preserve ordinary UTF-8 sequences split between file reads. Completed records are decoded with replacement for invalid bytes. The sanitizer strips terminal control characters and CSI/OSC escape sequences before rendering. Tabs expand to four spaces.

Each retained log record wraps at the current panel width using the renderer's character-width model. Severity color applies to all its continuation rows. Log offsets count display rows, permitting scrolling within one long record. When new records arrive or ring entries are evicted, wrapped row counts preserve the reading position. Raw retention limits remain unchanged; terminal reflow does not increase them.

Follow mode anchors the viewport to the newest display rows. Manual navigation disables following. For new output, the loop accounts for both added wrapped rows and evicted records so reading older content does not resume following implicitly. Width changes recompute wrapping; height changes alter the visible row count. Neither changes the retained source records or creates persistent history.

Progress parsing recognizes a leading Lake status record: an optional status icon, `[reported/total]`, an optional `(Optional)` marker, and a known action verb followed by its caption. Arbitrary diagnostic fractions are ignored. The numbers must be nonnegative with a positive total and `reported <= total`. A complete status prefix in the current partial line can update the display before its trailing newline or carriage return.

The counters are Lake job observations, not completed source-file counts. Dependency discovery can grow the denominator, and multiple facets can contribute jobs for one module. The raw numbers are preserved without an inferred offset or a source-tree scan. “Latest event” displays a retained recognized status record. In particular, Lake can use `Building` in a failed completed-job report; it must not be interpreted as evidence of a currently compiling file. See the official [build runner](https://github.com/leanprover/lean4/blob/v4.33.1/src/lake/Lake/Build/Run.lean#L53-L164) and [job action verbs](https://github.com/leanprover/lean4/blob/v4.33.1/src/lake/Lake/Build/Job/Basic.lean#L24-L58). Neither a job counter nor a caption is an exit-status or theorem-verification protocol.

The exact future Lean-file compilation total is unknown without a complete build-side plan. Ordinary logs and sampled `/proc` targets cannot establish that denominator. `lake query` builds targets, and `--no-build` stops at stale work rather than producing a full read-only plan; neither is launched by the monitor. See [CLI semantics](https://github.com/leanprover/lean4/blob/master/src/lake/Lake/CLI/Help.lean#L46-L75) and [module build/cache decisions](https://github.com/leanprover/lean4/blob/master/src/lake/Lake/Build/Module.lean#L1193-L1248).

## Estimation

A background worker performs a bounded, read-only inventory of local `.lean` files and recursively resolved imports, including available `.lake/packages` sources. It uses the standard `.lake/build/lib/lean` artifact layout and safe source-directory hints; unsupported configuration is covered by the explicit low-confidence scope. Missing/newer artifacts and conservative dependency invalidation produce the initial stale set. Known active compiler targets may join the set before its first numeric display. After that, its membership and denominator stay fixed for the run. Later known targets outside it enter a separate additional set. A detected new log run resets both sets and starts a new analysis. File/entry/header limits prevent unbounded analysis. The UI polls a channel instead of blocking on the scan or repeating it every refresh.

Current bounds are 20,000 modules, 100,000 local directory entries, 256 dependency package roots in addition to the project, 16 KiB per source/config header, and 256 imports per module. Local inventory skips hidden directories, `target`, `node_modules`, and `lakefile.lean`; direct symlink sources and nonregular header files are not read. Import traversal is deduplicated. Unresolved `Init`, `Lean`, and `Std` imports are treated as toolchain imports rather than missing external packages. These bounds and the local/import fallback can produce a partial scope. Source mtimes and standard `.olean` files are an approximation of Lake's hash/cache rules, not a recreation of them.

Known source-module `Built` records contribute timings and post-baseline completions. Initial retained output calibrates timings without claiming those records completed after monitoring began. Replay/reuse, unmatched targets, and native facets are excluded. A bounded window of wall-clock completion observations captures parallel throughput; a single reported `ms`/`s` duration seeds a rough estimate immediately. With no duration data, two distinct live completion observations at least 0.5 seconds apart provide throughput once the observation span reaches one second. The time row identifies missing logs or missing timing data instead of displaying an indefinite learning message. Total and remaining predictions describe the estimated Lean work in the observation window, not the full historical Lake run or native/linking phases. Hash/cache semantics and dynamic target declarations can make the stale estimate over- or under-count actual work, so the display remains approximate.

The estimator retains at most 128 positive finite duration samples and 32 completion observations. Timing seeds start with one reported duration (a rough initial estimate) and use matching current Lean workers for a concurrency estimate. Throughput requires distinct completion observations over a meaningful wall-clock interval. Current known compiler targets are evidence even when timestamps appear fresh; after the baseline freezes they cannot change the initial denominator. A disconnected scan worker or absent/incomplete source scope returns unavailable data rather than inventing a count. A generation change clears run completion/throughput state and schedules at most one scan worker at a time; retained historical durations can still seed the next run.

The main bar uses only completed members of the fixed initial set and its initial file total. Lake job counters appear only as reference metadata, never as the bar numerator or denominator. `additional` tracks known files outside the initial set, including active or completed extra work; timing forecasts include that work. The UI labels file totals and total/remaining times as estimates and exposes the scope when space permits. `done` refers to unique initial-set completions after the baseline, including completions that arrived while the background scan was running. No estimate data is written to the Lean project or monitoring directory.

## Terminal rendering

Frames consist of character/color cells. Only changed rows are emitted using ANSI cursor movement; an identical frame produces no output or flush. Unchanged log rows are left on screen as resource metrics update. The log's last-read indicator uses a fixed UTC wall-clock timestamp captured when bytes arrive, avoiding timer-only repaints. User interactions and terminal resizing still redraw the affected rows. Interactive mode uses `stty` when entering and leaving raw mode, not during ordinary refreshes. The alternate screen and cursor are restored on normal exit; handlers for hangup, interrupt, and termination request exit through the main loop.

One physical terminal column is reserved to avoid automatic wrapping. The full layout uses at least 90 drawing columns × 32 rows, which means a terminal at least 91 columns wide. The compact layout uses at least 52 drawing columns × 20 rows, requiring 53 physical columns. Smaller sizes show a resize message. The render area is capped at 300 drawing columns × 120 rows.

Interactive mode enables normal tracking (1000), button-event tracking (1002), and SGR coordinates (1006); it does not enable unrestricted pointer-motion reporting. The decoder also consumes legacy X10 packets, bounds CSI payloads to 64 bytes, and ignores right clicks, hover-only motion, and horizontal wheel events. Its zero-based coordinates are hit-tested against the same layout used by the renderer. Wheel events move the process list by one row or the log by three rows, depending on pointer location; events outside those panels or over an open help overlay are ignored. Keyboard arrows and `j`/`k` continue to target logs. Mouse capture is disabled on normal exit and handled shutdown, alongside terminal restoration. Protocol details follow the [XTerm mouse tracking specification](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-Mouse-Tracking).

Clicking a visible process data row opens a modal keyed by PID; `p`/`Enter` opens the first visible row. The modal looks up that PID in the latest snapshot and falls back to one cached row when it disappears. Its wrapped content and offset are independent of both background panels. While open, arrows/`j`/`k` and wheel events inside it scroll details; an outside click closes without acting on the underlying pane. `Enter`, `p`, or standalone `Esc` closes details. Standalone Escape is resolved after 100 ms so ordinary keyboard/mouse escape packets can finish; `Esc` closes help first if help is open. `q` and interrupt bytes retain global quit behavior.

A left-button press on the Build, Processes, or Build Log header begins divider dragging. Each divider redistributes space between its two adjacent panes; nonadjacent panes retain their boundaries. CPU, Memory, and Storage share the Resources pane. The application header and footer stay fixed. The layout reserves at least three Resources rows, four Build rows (header, status, fixed file progress, and time estimate), three Processes rows including headings when visible, and four Build Log rows including its header. Optional details are clipped within their pane rather than written over neighboring content.

Requested heights for the three leading panes are held in memory; logs receive the remaining screen space. On terminal resize, a bounded top-down allocation fits these preferences around each pane's minimum and the log minimum. Compact mode hides Processes and permits both remaining dividers to move. Changing between full and compact modes during a gesture cancels that gesture, preventing an old pointer target from resizing a different pane. Resizing preserves log scroll/follow state, recomputes process scroll bounds, and updates wheel hit testing to the new boundaries. Help blocks drag initiation and cancels active dragging. No layout preferences are written to disk.

Before any manual divider preference is set, process height is based on the matching process count rather than a fixed three-row viewport. The automatic allocator reduces optional summary space to fit that list, still reserving pane minimums and log content. Physical terminal limits can require process scrolling. After a manual drag, the saved heights take precedence and are clamped as the list or terminal changes. The time estimate remains visible at minimum Build height, including compact mode. Optional resource details, latest-event text, and supplemental file summaries are rendered only inside their allocated bounds.

The renderer handles common CJK characters as double-width and omits common combining/format marks. Complex emoji and joined grapheme clusters are outside its width model. A UTF-8 monospace terminal is recommended. `--no-color` and `NO_COLOR` disable colors but retain ANSI cursor positioning; `--snapshot` provides plain output.

## Side effects and limits

The monitor does not launch builds, manage their lifecycle, execute `lake`, make network requests, or persist monitoring data. It reads files and writes its terminal display. Shell pipelines documented in the README write their own build logs independently.

The implementation avoids per-refresh child commands and bounds retained logs and keyboard input. CPU history storage and its graph have been removed. System process scans, wrapping, and terminal-frame size still affect overhead. There is no guaranteed CPU, memory, or storage-usage budget.

macOS supports demo/development use, not the production system-metric path. Test fixtures exercise Linux text formats without establishing that every Linux ABI or terminal is supported. Validate live sampling on the actual Ubuntu target.
