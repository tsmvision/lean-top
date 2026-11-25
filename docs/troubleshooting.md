# Troubleshooting

## Interactive mode requires a terminal

Both stdin and stdout must be attached to a terminal. SSH sessions need an allocated TTY for interactive use. Do not pipe the interactive display into a file.

For scripts, redirected output, or a quick preview:

```sh
lean-top --demo --snapshot
lean-top --project /srv/riemann --log /srv/riemann/build.log --snapshot
```

`--snapshot` writes one plain-text 110 × 38 frame and exits. `--no-color` only disables colors; it does not turn interactive output into plain text.

## An update does not appear in the running command

Stop the old monitor with `q`, update your checkout, and rebuild:

```sh
git pull --ff-only
cargo build --release --offline
./target/release/lean-top --demo
```

If `lean-top` was installed with Cargo, run `cargo install --path . --force --offline` from the updated checkout. A release build updates `target/release/lean-top`, not an older installed executable. Restart the monitor after rebuilding or reinstalling.

## The default project or log is not the one expected

Without `--project`, the project is your launch directory (`.`), used for job-history context, storage, and the header. It does not restrict the server-wide process list. Without `--log`, the log is selected independently from `~/logs`, using filename order rather than freshness. For an unambiguous pairing, pass both absolute paths:

```sh
lean-top --project /srv/riemann --log /srv/riemann/build.log
```

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
lake --no-ansi build 2>&1 | tee build.log
```

The monitor does not create the log or launch the build. Check that the build is actually emitting output, press `Space` if sampling is paused, and press `f` to return to following the tail.

If `--log` is omitted, the application chooses the filename-sorted first regular non-hidden `.log` file in `~/logs` at startup. It does not choose by modification time or search subdirectories. If no file exists yet, pass the future filename explicitly or restart after creating it. Use `--log` when multiple runs are present and you need a specific one.

## Progress is missing, old, or apparently complete

Build uses Lake job counts, not source-file counts. The numerator is the latest recognized `[reported/total]` status in the recent selected log tail. The denominator is the larger of the current Lake total and the previous comparable successful build's total. With no history it uses Lake's current total, which can grow during discovery. A success summary sets the exact final `N / N`; `Nothing to build.` sets `0 / 0`.

No source or artifact scan delays the counter. If the recent tail contains no recognized status or success summary, the current count is unknown. Capture both output streams and check that `--log` points at the running build. A cached total alone never invents a completed count.
Progress is read from log text and can outlive the build process. It does not indicate an exit code, successful proof checking, absence of `sorry`, or validity of the Riemann hypothesis proof. Use the build's actual exit status and your Lean verification workflow for those conclusions.

Use a fresh log per build. Ordinary `tee build.log` truncates a shared filename, and truncate-and-regrow between refreshes can evade size-based detection. A timestamped file avoids mixing runs:

```bash
cd /srv/riemann
set -o pipefail
log_path="build-$(date -u +%Y%m%dT%H%M%SZ).log"
printf 'Monitor log: %s/%s\n' "$PWD" "$log_path"
lake --no-ansi build 2>&1 | tee "$log_path"
```

Pass the printed absolute path to the monitor's `--log` option.

## Build status shows ACTIVE for a different project

Process discovery is server-wide. Any visible process named `lean`, `lake`, or `leanc` contributes to ACTIVE status, even if it belongs to another project. `--project` selects the storage filesystem and header directory, not a process filter. Processes reserves 16 data rows by default and does not grow or shrink when the process count changes. Small terminals may show fewer rows to retain other panes' minimum heights. With many processes or a manually reduced height, hover over Processes and use the wheel for remaining entries. The heading shows the visible range and total.

## PID, full paths, or command options are missing from Processes

The normal table intentionally shows only `TYPE`, `MEMORY`, and `TARGET`. Click a process row, or press `p`/`Enter` for the first visible row, to open details. Scroll long details with the wheel or arrow keys. Press `Enter`/`Esc` or click outside to close. A process that disappears retains its last captured details with a no-longer-visible label. If target detection or working-directory access fails, its path may be unavailable or relative; full command display remains available when readable.

While details are open, arrows and `j`/`k` scroll the dialog instead of logs, and wheel events do not operate the background panels. `Esc` closes help first if it is layered over details. Close details before resizing sections. `q` still exits the monitor rather than just closing the dialog.

## Estimated time is waiting or changes

The monitor needs at least one second of live observation and a positive change in the Lake-reported job count to measure a rate. It does not treat startup log reading as compiler speed. Without a build log, select the actual log path; without recognized Lake progress, the time row continues to explain what is missing.

Total and remaining time are approximate, based on at most 32 distinct job-count observations and the estimated total jobs. Parallelism, differing job costs, and changes to the total can alter the forecast. Restarting recovers progress from the recent log tail and prior successful total from the cache, but resets speed observations. A success summary sets remaining time to zero. There is no source/import/artifact scan or full-log replay.

Cache errors appear in the time row. Current Lake job counts remain usable; fix the cache/configuration problem to reuse successful totals on later runs.

## Some processes or estimate fields are not visible

The default process viewport reserves 16 data rows independently of process count, but cannot exceed physical terminal space while preserving other panes' minimums. Use the process range in the heading and scroll for additional entries, or enlarge the terminal. After you drag a divider, that manual height is retained for the session; it overrides the default viewport. Restart to restore the default allocation. Build reserves four rows so status, Lake progress, and the estimated total/remaining time remain visible even when crowded or manually shrunk. The Build counter shows only `completed / total`, with no `files`, `fixed`, or `extra` suffix and no duplicate file-summary row. Expanding Build reveals log-source information and latest-event text. The process table remains unavailable in compact mode.

## Mouse scrolling does not work

Place the pointer over the Processes or Build Log panel before using the wheel. Processes is available only in the full layout (at least 91 terminal columns × 32 rows). Wheel events over resource metrics, borders, and the footer are ignored. Close help with `?` before scrolling. A process list that already fits its viewport has no additional rows to reveal.

Mouse scrolling requires a terminal or terminal multiplexer that forwards XTerm-compatible mouse reports to the application. Some terminals or modifier keys route the wheel to terminal history instead. Check your terminal or multiplexer mouse settings if events do not reach the application. Keyboard `↑`/`↓` or `j`/`k` remain available for logs; `f` resumes following the latest output. The monitor continues to update resource metrics while you scroll.

## A section divider does not move

Start dragging on a horizontal divider labeled `BUILD`, `PROCESSES`, or `BUILD LOG`, using the left mouse button. The divider resizes the adjacent sections; dragging inside their content does not resize them. CPU, Memory, and Storage share the Resources section, adjusted using the `BUILD` divider below them. Close help first, and ensure your terminal forwards button-motion reports.

Dividers stop when either adjacent pane reaches its minimum height. The title/header and footer remain fixed, and Processes is hidden in compact mode. Expand the terminal if there is not enough room to grow a section further. Shrinking a section hides optional details while retaining its core values or progress. Release the button before scrolling. Height preferences last for the current session only.

Close process details as well as help before dragging. A drag is canceled if a terminal resize switches between full and compact layouts; start a new drag on the visible divider afterward.

## Storage does not match the project directory's size

Storage is filesystem capacity, not recursive project size. “Free” is user-available space; reserved blocks can make it smaller than total minus used. Choose the project directory on the intended mount. The monitor does not measure I/O speed or `.lake` cache size.

## Old log lines disappear or long lines are cut off

Retained log text wraps to the panel width. Narrowing the terminal creates additional display rows rather than cutting off the right-hand side. Scroll with the wheel or arrow keys to inspect older parts, including rows within a single long record; `f` returns to the latest output.

The live ingestion ring retains 200 completed records and a partial record. Scrolling up in Build Log reads earlier records from the selected file on demand, beyond that ring and including output from before monitoring began. Reading history does not pause sampling or jump when new output arrives. Scroll down to the newest output or press `f` to resume following. History reads use 64 KiB pages with a bounded 4096-record/256 KiB text cache and no duplicate disk file. The 4096-raw-byte display limit per record still applies.

Uncached history depends on the original log remaining readable. If the file is removed, cached text remains visible but older pages cannot be fetched. Detected rotation or truncation resets the view to the current file; same-inode truncate-and-regrow between samples may remain undetected. Demo mode has no backing file and keeps its bounded in-memory history.

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

## Build Log appears to refresh without new messages

Recognized Lake progress lines normalize the spinner glyph and suppress consecutive identical progress records, including an identical unfinished line. CPU/memory updates alone do not write unchanged log rows. New text updates changed character spans without clearing the row. Distinct progress counters or messages still update the log; repeated ordinary diagnostics are retained.

Scrolling, terminal resizing, overlays, and manual divider movement can redraw affected log content. Process-count changes do not move the default panel boundaries. These are viewport changes. If repeated updates remain at a fixed viewport, check whether the selected log contains genuinely changing counters or text rather than only spinner animation.

## Build counts differ after restarting

Restarting reads the recent 64 KiB log tail rather than starting a new completed-job counter. A recognized Lake status restores its reported numerator immediately; a final success summary restores the exact completed total. If that window has no recognized status, the numerator remains unknown. Select the actual current log and ensure both stdout and stderr are captured. There is no need to restart the build.

The denominator can exceed the raw Lake total because a previous comparable successful total seeds the estimate. It grows when Lake discovers more jobs than that history predicted, and becomes exact when the current success summary arrives. A prior success outside the tail is not searched automatically. Keep the monitor running through success to save the final total.

## The estimated total is unsuitable for this build

History is separated by canonical project directory, `--build-key`, and the contents of `lean-toolchain`, `lakefile.lean`, `lakefile.toml`, and `lake-manifest.json`. It cannot automatically distinguish target arguments or cold/warm cache states. Use a consistent label for comparable builds and separate labels for different conditions:

```sh
lean-top --project /srv/riemann --log /srv/riemann/build.log --build-key Main-cold
```

The default label is `default`. This option does not change the Lake command. Source changes can also change job totals without changing the history context. The previous total is an economical approximation, not a guaranteed fixed denominator. Failed or partial runs do not overwrite it; the next observed successful summary does.

## Job history cannot be read or saved

History lives under `$XDG_CACHE_HOME/lean-top/jobs`, or `$HOME/.cache/lean-top/jobs` when `XDG_CACHE_HOME` is unset or empty. The base must be absolute and readable/writable by the monitor. The four configuration inputs must be regular, readable files no larger than 1 MiB each when present. History records must have the expected version/context and fit within 512 bytes. Corrupt, nonregular, or oversized files produce diagnostics instead of trusted totals.

Fix the indicated permissions or file problem, then restart the monitor to reload history. Successful summaries save totals atomically with private file permissions. Cache errors do not require rerunning the Lean build and do not prevent displaying current Lake progress. Demo mode does not read or write this history.

## Build shows zero while log output is visible

The Build numerator now comes directly from the Lake status fraction; it does not depend on individual successful `Built` messages or `.olean`/`.trace` output. A real `[0/0]` discovery record can show zero before Lake reports completed jobs. Ordinary messages without a valid status do not establish a count. Verify the current log path and capture both streams:

```sh
lake --no-ansi build Main 2>&1 | tee build.log
```

If the visible log contains `[16000/20000] Running ...`, the current numerator should be 16000. Rebuild and reinstall the updated monitor if an older executable still shows a source/artifact-based count. Use `command -v lean-top` to identify the executable your shell runs.
