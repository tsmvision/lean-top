# Architecture

`lean-top` is a Rust terminal application targeting 64-bit Ubuntu Linux. It requires Rust 1.70 or newer and has no Cargo dependencies. It monitors an independently running build through Linux system information and an optional log file.

## Modules

| File | Responsibility |
| --- | --- |
| `src/main.rs` | CLI validation, sampling schedule, keyboard dispatch, demo generation, snapshot output |
| `src/monitor.rs` | System snapshots, Lean process discovery, bounded log tailing, output sanitization, progress parsing |
| `src/ui.rs` | Full and compact layouts, terminal cells, colors, CPU history, log scrolling, row-difference rendering |
| `src/terminal.rs` | TTY setup/restoration, terminal dimensions, alternate screen, shutdown signals |
| `src/monitor_tests.rs` | Synthetic system and log tests |

## Update loop

The main thread samples immediately, draws a frame, and waits for keyboard input or the next refresh. The default interval is 500 ms, configurable from 100 to 60000 ms. A bounded input channel carries bytes from a blocking stdin-reader thread. The main loop checks for shutdown and resize with waits capped at 100 ms; metrics are sampled on the configured interval.

Pausing suspends both system sampling and log reading. It does not suspend the independently running build. Resuming may skip older log data if the file has advanced beyond the bounded tail window.

Snapshot mode prints one 110 × 38 plain-text frame without changing terminal mode. A live snapshot takes a second system sample after 100 ms to obtain a CPU delta. Demo mode supplies simulated values and output without reading live resource metrics.

## System information

CPU utilization uses differences between aggregate `/proc/stat` counters. Idle includes idle and I/O-wait ticks. Guest counters are excluded from the sum because they are already included in user/nice time. No rate is reported before two valid samples or when total ticks have not advanced.

Memory uses `MemTotal` and `MemAvailable` from `/proc/meminfo`. Swap uses `SwapTotal` and `SwapFree`. Load averages come from `/proc/loadavg`.

Process discovery visits numeric `/proc` entries and matches `comm` against `lean`, `lake`, and `leanc`. It reads RSS from `status` and at most 4096 bytes of `cmdline`. Processes may disappear during a scan; missing process details are tolerated. Results are sorted by RSS, and the full layout displays the first three. Discovery is server-wide and does not associate processes with `--project`.

Storage calls `statvfs` directly for the project directory. The implementation uses a 64-bit Linux ABI declaration; unsupported targets report unavailable storage. Used space excludes filesystem free blocks, while the displayed free space uses blocks available to an unprivileged user. The monitor does not recursively measure project or `.lake` directory sizes, and it does not measure disk I/O throughput.

## Log ingestion

The monitor opens the log each refresh, identifies it by device/inode, and seeks to its previous byte position. A changed identity or an observed size decrease resets retained output and progress. Missing files are retried on later refreshes. Rotation detection does not guarantee detection of a same-inode truncate-and-regrow between samples; separate log filenames per build avoid that ambiguity.

Reads are limited to the most recent 64 KiB window. If the unread gap exceeds that window, the reader moves forward, discards the initial partial line, and adds a skipped-output message. It retains up to 200 completed lines and up to 4096 raw bytes for the current line. Bytes beyond a line's limit are discarded until a delimiter arrives. A partial line can be displayed before it is completed.

Newline and carriage return both delimit records, supporting Lake progress updates. Raw pending bytes preserve ordinary UTF-8 sequences split between file reads. Completed records are decoded with replacement for invalid bytes. The sanitizer strips terminal control characters and CSI/OSC escape sequences before rendering. Tabs expand to four spaces.

Progress parsing recognizes a leading Lake status record: an optional status icon, `[reported/total]`, an optional `(Optional)` marker, and a known action verb followed by its caption. Arbitrary diagnostic fractions are ignored. The numbers must be nonnegative with a positive total and `reported <= total`. A complete status prefix in the current partial line can update the display before its trailing newline or carriage return.

The counters are Lake job observations, not completed source-file counts. Dependency discovery can grow the denominator, and multiple facets can contribute jobs for one module. The raw numbers are preserved without an inferred offset or a source-tree scan. “Latest event” displays a retained recognized status record. In particular, Lake can use `Building` in a failed completed-job report; it must not be interpreted as evidence of a currently compiling file. See the official [build runner](https://github.com/leanprover/lean4/blob/v4.33.1/src/lake/Lake/Build/Run.lean#L53-L164) and [job action verbs](https://github.com/leanprover/lean4/blob/v4.33.1/src/lake/Lake/Build/Job/Basic.lean#L24-L58). Neither a job counter nor a caption is an exit-status or theorem-verification protocol.

## Terminal rendering

Frames consist of character/color cells. Only changed rows are emitted using ANSI cursor movement; an identical frame produces no output or flush. Unchanged log rows are left on screen as resource metrics update. The log's last-read indicator uses a fixed UTC wall-clock timestamp captured when bytes arrive, avoiding timer-only repaints. User interactions and terminal resizing still redraw the affected rows. Interactive mode uses `stty` when entering and leaving raw mode, not during ordinary refreshes. The alternate screen and cursor are restored on normal exit; handlers for hangup, interrupt, and termination request exit through the main loop.

One physical terminal column is reserved to avoid automatic wrapping. The full layout uses at least 90 drawing columns × 32 rows, which means a terminal at least 91 columns wide. The compact layout uses at least 52 drawing columns × 20 rows, requiring 53 physical columns. Smaller sizes show a resize message. The render area is capped at 300 drawing columns × 120 rows.

The renderer handles common CJK characters as double-width and omits common combining/format marks. Complex emoji and joined grapheme clusters are outside its width model. A UTF-8 monospace terminal is recommended. `--no-color` and `NO_COLOR` disable colors but retain ANSI cursor positioning; `--snapshot` provides plain output.

## Side effects and limits

The monitor does not launch builds, manage their lifecycle, execute `lake`, make network requests, or persist monitoring data. It reads files and writes its terminal display. Shell pipelines documented in the README write their own build logs independently.

The implementation avoids per-refresh child commands and bounds retained logs, CPU history, and keyboard input. System process scans and terminal-frame size still affect overhead. There is no guaranteed CPU, memory, or storage-usage budget.

macOS supports demo/development use, not the production system-metric path. Test fixtures exercise Linux text formats without establishing that every Linux ABI or terminal is supported. Validate live sampling on the actual Ubuntu target.
