# Troubleshooting

## Interactive mode requires a terminal

Both stdin and stdout must be attached to a terminal. SSH sessions need an allocated TTY for interactive use. Do not pipe the interactive display into a file.

For scripts, redirected output, or a quick preview:

```sh
lean-top --demo --snapshot
lean-top --project /srv/riemann --log /srv/riemann/build.log --snapshot
```

`--snapshot` writes one plain-text 110 × 38 frame and exits. `--no-color` only disables colors; it does not turn interactive output into plain text.

## The screen asks to be resized

Use at least 53 terminal columns × 20 rows for the compact layout, or 91 columns × 32 rows for the full layout. The application reserves one column to avoid wrapping at the right edge. Compact mode hides the process table and initially shows only the core resource rows; expand Resources to reveal available details. CPU history is no longer displayed in either layout.

## System metrics are unavailable

Live metrics target 64-bit Ubuntu Linux with accessible `/proc`. On macOS, use:

```sh
lean-top --demo
```

On Linux, confirm that the monitor can read `/proc/stat`, `/proc/meminfo`, `/proc/loadavg`, and the relevant process entries. Containers and restricted `/proc` mounts can limit visibility. The display reflects the system information visible to the monitor; it does not calculate container-specific CPU or memory limits.

## CPU initially says “Sampling…”

CPU utilization requires two samples. Wait for another refresh. If counters have not advanced, no rate is shown for that sample. CPU is aggregate server utilization on a 0–100% scale, rather than a per-process percentage that can exceed 100%.

## The log is unavailable or empty

Use the absolute path to the file receiving build output:

```sh
lean-top --project /srv/riemann --log /srv/riemann/build.log
```

A relative `--log build.log` refers to the directory from which you launched the monitor, even when `--project` points elsewhere. The monitor retries missing log files, so it may start before the build creates the file. Verify file and parent-directory read permissions if errors persist.

Capture both output streams in a separate Bash terminal:

```bash
cd /srv/riemann
set -o pipefail
lake build 2>&1 | tee build.log
```

The monitor does not create the log or launch the build. Check that the build is actually emitting output, press `Space` if sampling is paused, and press `f` to return to following the tail.

If `--log` is omitted, the application chooses the filename-sorted first regular non-hidden `.log` file in `~/logs` at startup. It does not choose by modification time or search subdirectories. If no file exists yet, pass the future filename explicitly or restart after creating it. Use `--log` when multiple runs are present and you need a specific one.

## Progress is missing, old, or apparently complete

The progress bar requires a recognized Lake status record such as `✔ [3/8] Built Foo` or `⣾ [3/8] Running Foo`. A complete status prefix can update the display before a newline arrives. Arbitrary `[n/m]` text in diagnostics does not update the counter. “Latest event” is a recent retained Lake status record; it does not identify the only file currently compiling.

The displayed numbers count Lake jobs, not `.lean` files. A source file may generate several jobs, cached/replayed work can be reported, and dependency discovery can increase the total during a build. Do not compare the denominator to a recursive source-file count or assume it is fixed. Before the first recognized status record arrives, the count is unknown. The monitor does not launch a second build or scan the source tree to invent a file total.

Progress is read from log text and can outlive the build process. It does not indicate an exit code, successful proof checking, absence of `sorry`, or validity of the Riemann hypothesis proof. Use the build's actual exit status and your Lean verification workflow for those conclusions.

Use a fresh log per build. Ordinary `tee build.log` truncates a shared filename, and truncate-and-regrow between refreshes can evade size-based detection. A timestamped file avoids mixing runs:

```bash
cd /srv/riemann
set -o pipefail
log_path="build-$(date -u +%Y%m%dT%H%M%SZ).log"
printf 'Monitor log: %s/%s\n' "$PWD" "$log_path"
lake build 2>&1 | tee "$log_path"
```

Pass the printed absolute path to the monitor's `--log` option.

## Build status shows ACTIVE for a different project

Process discovery is server-wide. Any visible process named `lean`, `lake`, or `leanc` contributes to ACTIVE status, even if it belongs to another project. `--project` selects the storage filesystem and header directory, not a process filter. Processes automatically expands to show all entries that fit while retaining other panes' minimum heights. With many processes or a manually reduced height, hover over Processes and use the wheel for remaining entries. The heading shows the visible range and total.

## PID, full paths, or command options are missing from Processes

The normal table intentionally shows only `TYPE`, `MEMORY`, and `TARGET`. Click a process row, or press `p`/`Enter` for the first visible row, to open details. Scroll long details with the wheel or arrow keys. Press `Enter`/`Esc` or click outside to close. A process that disappears retains its last captured details with a no-longer-visible label. If target detection or working-directory access fails, its path may be unavailable or relative; full command display remains available when readable.

## Compile estimates are unavailable, learning, or different from Lake jobs

Estimated Lean files are separate from the Lake job counter. The background source/import scan needs a readable Lean project under `--project`; a folder containing no local Lean sources has no file estimate. Time remains `Learning` until recognized source-module completion timings or throughput provide enough samples. Replayed and native jobs do not train Lean compilation time.

Estimates are approximate: custom build directories, generated sources, dynamic target configuration, and Lake's hash/artifact cache can differ from a source/standard-output timestamp estimate. The scope label indicates low confidence or a partial scan. Joining a build midway or reading a bounded tail cannot recover its true start time and all earlier work. Use a fresh, explicitly selected log for each run and the correct project directory; no second build is launched for estimation.

## Mouse scrolling does not work

Place the pointer over the Processes or Build Log panel before using the wheel. Processes is available only in the full layout (at least 91 terminal columns × 32 rows). Wheel events over resource metrics, borders, and the footer are ignored. Close help with `?` before scrolling. A process list that already fits its viewport has no additional rows to reveal.

Mouse scrolling requires a terminal or terminal multiplexer that forwards XTerm-compatible mouse reports to the application. Some terminals or modifier keys route the wheel to terminal history instead. Check your terminal or multiplexer mouse settings if events do not reach the application. Keyboard `↑`/`↓` or `j`/`k` remain available for logs; `f` resumes following the latest output. The monitor continues to update resource metrics while you scroll.

## A section divider does not move

Start dragging on a horizontal divider labeled `BUILD`, `PROCESSES`, or `BUILD LOG`, using the left mouse button. The divider resizes the adjacent sections; dragging inside their content does not resize them. CPU, Memory, and Storage share the Resources section, adjusted using the `BUILD` divider below them. Close help first, and ensure your terminal forwards button-motion reports.

Dividers stop when either adjacent pane reaches its minimum height. The title/header and footer remain fixed, and Processes is hidden in compact mode. Expand the terminal if there is not enough room to grow a section further. Shrinking a section hides optional details while retaining its core values or progress. Release the button before scrolling. Height preferences last for the current session only.

## Storage does not match the project directory's size

Storage is filesystem capacity, not recursive project size. “Free” is user-available space; reserved blocks can make it smaller than total minus used. Choose the project directory on the intended mount. The monitor does not measure I/O speed or `.lake` cache size.

## Old log lines disappear or long lines are cut off

Retained log text wraps to the panel width. Narrowing the terminal creates additional display rows rather than cutting off the right-hand side. Scroll with the wheel or arrow keys to inspect older parts, including rows within a single long record; `f` returns to the latest output.

The monitor intentionally retains only 200 completed records and a partial record, keeping at most 4096 raw bytes per record. It reads a recent 64 KiB window per refresh and can skip older data during a burst or after a long pause. These ingestion bounds still apply after wrapping. Read the original log with another tool when you need complete history beyond them.

## Reduce monitoring overhead

Increase the refresh interval:

```sh
lean-top --project /srv/riemann --log /srv/riemann/build.log --interval-ms 2000
```

Accepted intervals range from 100 to 60000 ms. Use a release build for deployment. Overhead depends on the number of visible processes, output volume, and terminal dimensions; this project does not promise a fixed CPU or memory budget.

## Characters or colors look wrong

Use a UTF-8 monospace terminal with ANSI cursor support. Unicode width conventions differ between terminals; complex emoji and joined character sequences are not fully modeled. Try `--no-color` or set `NO_COLOR=1` for color problems. Use `--snapshot` for plain output.

## The terminal is left in raw mode

Normal quit, `Ctrl-C`, and supported shutdown signals request terminal restoration. A forced kill such as `SIGKILL`, a crash, or a lost session can prevent cleanup. In the affected terminal, type the following and press Enter even if typing is not echoed:

```sh
stty sane
printf '\033[?1000l\033[?1002l\033[?1006l\033[0m\033[?25h\033[?1049l'
```

## Offline build fails

The project has no Cargo dependencies, but an installed Rust compiler and Cargo are still required. Offline mode cannot install the toolchain. Confirm Rust 1.70 or newer is available, then run from the repository directory:

```sh
cargo build --release --offline
```

For source changes, run `cargo fmt --check`, `cargo test --offline`, and `cargo clippy --offline --all-targets -- -D warnings` before rebuilding.
