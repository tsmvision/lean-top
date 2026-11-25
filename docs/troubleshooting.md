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

Without `--project`, the project is your launch directory (`.`), used for storage and the header. It does not restrict the server-wide process list. Without `--log`, the log is selected independently from `~/logs`, using filename order rather than freshness. For an unambiguous pairing, pass both absolute paths:

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
lake build 2>&1 | tee build.log
```

The monitor does not create the log or launch the build. Check that the build is actually emitting output, press `Space` if sampling is paused, and press `f` to return to following the tail.

If `--log` is omitted, the application chooses the filename-sorted first regular non-hidden `.log` file in `~/logs` at startup. It does not choose by modification time or search subdirectories. If no file exists yet, pass the future filename explicitly or restart after creating it. Use `--log` when multiple runs are present and you need a specific one.

## Progress is missing, old, or apparently complete

Build shows distinct successful module completions against a total calculated from project `.lean` filenames once at startup. Its numbers are independent of Lake's `[reported/total]` job fraction. No numeric total is hardcoded or supplied manually. For subset targets or custom source layouts, use `--module-list`/`PROJECT/lean-top.modules`.

The calculated/listed total remains fixed during the run. Duplicate completions, native facets, cached/replayed jobs, failed records, and unmatched targets do not add to the numerator. An incomplete inventory or replay reports an error rather than an invented count.

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

While details are open, arrows and `j`/`k` scroll the dialog instead of logs, and wheel events do not operate the background panels. `Esc` closes help first if it is layered over details. Close details before resizing sections. `q` still exits the monitor rather than just closing the dialog.

## Estimated time is waiting or changes

The monitor first restores completion history in a background worker. It then needs a new unique completion and at least one second of elapsed live observation to measure compilation speed. Re-reading historical output does not manufacture a rate. The total is calculated automatically; inventory errors should be corrected, or a complete target list can be supplied. Without a build log, select the actual log path. Any target-list or replay error appears in the time row.

Total and remaining time are approximate, based on at most 32 observations of new completed modules. Parallelism and varying module costs can change the forecast. Restarting restores the completion count but restarts speed observations. Only the initial filename inventory is needed; no recurring source/import/artifact scan is performed.

## Some processes or estimate fields are not visible

Automatic process height attempts to show the complete matching list, but it cannot exceed physical terminal space while preserving other panes' minimums. Use the process range in the heading and scroll for additional entries, or enlarge the terminal. After you drag a divider, that manual height is retained for the session; it overrides the automatic viewport. Restart to restore the default allocation. Build reserves four rows so status, Lake progress, and the estimated total/remaining time remain visible even when crowded or manually shrunk. The Build counter shows only `completed / total`, with no `files`, `fixed`, or `extra` suffix and no duplicate file-summary row. Expanding Build reveals log-source information and latest-event text. The process table remains unavailable in compact mode.

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

Scrolling, terminal resizing, overlays, and automatic pane movement when process counts change can redraw affected log content. These are viewport changes. If repeated updates remain at a fixed viewport, check whether the selected log contains genuinely changing counters or text rather than only spinner animation.

## Build counts differ after restarting

Startup counts project `.lean` filenames once (or reads a supplied target list) and restores successful `Built` records from the log's beginning. Reopening the same sources/list and log restores the same ratio, even beyond the 200-record display ring and 64 KiB tail. Large inventories and logs can take time; counts remain unknown until initialization completes.

Sources added/removed, a changed list, a different log, deleted/truncated records, or multiple builds appended to one file can change the reconstructed count. Native facets, cached/replayed jobs, failed records, and non-target modules are excluded. The automatic denominator describes all project modules; for a partial target, a custom source root, or external dependencies use a verified module list.

## Target inventory or module list is rejected

Automatic discovery excludes hidden directories, `lakefile.lean`, symlink directories, and nonregular sources. Unreadable directories or invalid module paths cause an error rather than a truncated count. The supported maximum is 1,000,000 modules and 2,000,000 directory entries, above a 175,187-module project; no value of 175,187 is built into production defaults.

A list contains dotted names such as `Main` or `Proof.Zeta`, one per line. Blank lines and `#` comments are allowed; paths such as `Proof/Zeta.lean` are not module names. Duplicate names count once. Invalid, oversized, or nonregular lists report an error. The list size limit is 64 MiB. After correcting data, restart to initialize the target set again.
