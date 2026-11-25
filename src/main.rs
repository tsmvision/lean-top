mod estimate;
mod input;
mod monitor;
mod terminal;
mod ui;

use estimate::{Estimate, Estimator};
use input::{Decoder, Event};
use monitor::{LogTail, Sampler};
use std::env;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use ui::{View, ViewState};

const HELP: &str = "lean-top — lightweight real-time Lean build monitor

Usage: lean-top [OPTIONS]

Options:
  --project PATH      Project directory (default: current directory)
  --log PATH          Build log (default: first filename-sorted ~/logs/*.log)
  --module-list PATH  Fixed target modules, one name per line (auto: lean-top.modules)
  --interval-ms MS    Refresh interval, 100–60000 ms (default: 500)
  --demo              Show simulated metrics and build output
  --snapshot          Print one plain-text frame and exit (110 x 38)
  --no-color          Disable colors; ANSI cursor control still requires a TTY
  -h, --help          Show this help
  -V, --version       Show version

Keys: q/Ctrl-C quit, Space pause, Up/Down or j/k scroll, f follow, ? help.
Click a process row or press p/Enter for details; Enter/Esc closes details.
Mouse: wheel over Processes or Build Log to scroll that panel independently.
Left-drag a section divider to resize the adjacent sections.
Metrics and processes are server-wide. Storage follows the project filesystem.
The monitor only reads; run lake build separately and capture its output.
";

struct Options {
    project: PathBuf,
    log: Option<PathBuf>,
    module_list: Option<PathBuf>,
    interval: Duration,
    demo: bool,
    snapshot: bool,
    color: bool,
}

fn discover_log(directory: &Path) -> Option<PathBuf> {
    std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "log")
                && !path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .starts_with('.')
                && path.is_file()
        })
        .min()
}

fn options(args: impl IntoIterator<Item = String>) -> Result<Option<Options>, String> {
    let mut options = Options {
        project: env::current_dir().map_err(|e| e.to_string())?,
        log: None,
        module_list: None,
        interval: Duration::from_millis(500),
        demo: false,
        snapshot: false,
        color: env::var_os("NO_COLOR").is_none(),
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => options.project = args.next().ok_or("--project requires a path")?.into(),
            "--log" => options.log = Some(args.next().ok_or("--log requires a path")?.into()),
            "--module-list" => {
                options.module_list =
                    Some(args.next().ok_or("--module-list requires a path")?.into())
            }
            "--interval-ms" => {
                let ms = args
                    .next()
                    .ok_or("--interval-ms requires a value")?
                    .parse::<u64>()
                    .map_err(|_| "Invalid interval; expected milliseconds")?;
                if !(100..=60000).contains(&ms) {
                    return Err("Interval must be between 100 and 60000 ms".into());
                }
                options.interval = Duration::from_millis(ms);
            }
            "--demo" => options.demo = true,
            "--snapshot" => options.snapshot = true,
            "--no-color" => options.color = false,
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("lean-top {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            _ => return Err(format!("Unknown option: {arg}. Use --help.")),
        }
    }
    options.project = options
        .project
        .canonicalize()
        .map_err(|e| format!("Invalid project directory: {e}"))?;
    if !options.project.is_dir() {
        return Err("--project must be a directory".into());
    }
    if options.demo && options.log.is_some() {
        return Err("--demo cannot be combined with --log".into());
    }
    if options.demo && options.module_list.is_some() {
        return Err("--demo cannot be combined with --module-list".into());
    }
    if !options.demo && options.module_list.is_none() {
        let manifest = options.project.join("lean-top.modules");
        if manifest.is_file() {
            options.module_list = Some(manifest);
        }
    }
    if let Some(manifest) = &options.module_list {
        if !manifest.is_file() {
            return Err("--module-list must name an existing regular file".into());
        }
    }
    if !options.demo && options.log.is_none() {
        options.log = env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .and_then(|home| discover_log(&PathBuf::from(home).join("logs")));
    }
    if let Some(path) = &options.log {
        if path.is_dir() {
            return Err("--log must be a file path".into());
        }
    }
    Ok(Some(options))
}

fn demo_log(log: &mut LogTail, tick: u64) {
    let done = 342 + (tick / 3) % 300;
    log.progress = Some((done, 680));
    if tick == 0 {
        for index in 320..340 {
            log.push(format!(
                "✔ [{index}/680] Built RiemannHypothesis.Lemma{index}"
            ));
        }
        log.push("✔ [340/680] Built RiemannHypothesis.ZetaBounds".into());
        log.push("warning: ExplicitFormula.lean:128: unused variable `h`".into());
    }
    if tick % 3 == 0 {
        log.push(format!(
            "⣾ [{done}/680] Running RiemannHypothesis.ExplicitFormula"
        ));
        log.updated = Some(std::time::SystemTime::now());
    }
}

fn demo_estimate(tick: u64) -> Estimate {
    let completed = (34 + tick as usize / 6).min(120);
    Estimate {
        files: Some(120),
        completed: Some(completed),
        total_seconds: Some(180.0),
        remaining_seconds: Some((120 - completed) as f64 * 1.5),
        error: None,
    }
}

fn run(options: Options) -> io::Result<()> {
    let mut sampler = Sampler::new(options.project.clone());
    let mut estimator = Estimator::new(options.project.clone(), options.module_list.clone());
    let mut log = LogTail::new(options.log.clone());
    let mut state = ViewState {
        follow: true,
        demo: options.demo,
        ..ViewState::default()
    };
    let project = options.project.display().to_string();
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_else(|_| "localhost".into())
        .trim()
        .to_string();
    let cores = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let mut snapshot = if options.demo {
        monitor::demo(0)
    } else {
        sampler.sample()
    };
    if options.demo {
        demo_log(&mut log, 0);
    } else {
        log.refresh();
    }
    state.estimate = if options.demo {
        demo_estimate(0)
    } else {
        estimator.refresh(&log)
    };

    if options.snapshot {
        // A second small sample gives a meaningful CPU delta for a live snapshot.
        if !options.demo {
            std::thread::sleep(Duration::from_millis(100));
            snapshot = sampler.sample();
            state.estimate = estimator.refresh(&log);
        }
        let frame = ui::render(
            &View {
                snapshot: &snapshot,
                log: &log,
                state: &state,
                project: &project,
                host: &host,
                cores,
                interval_ms: options.interval.as_millis() as u64,
            },
            110,
            38,
        );
        print!("{}", frame.plain());
        return Ok(());
    }

    let _terminal = terminal::Terminal::enter()?;
    // Raw input blocks on its own thread; the UI sleeps on the bounded channel.
    let (send, receive) = mpsc::sync_channel(32);
    std::thread::spawn(move || {
        for byte in io::stdin().lock().bytes() {
            match byte {
                Ok(byte) => {
                    if send.send(byte).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let mut previous = None;
    let mut next_tick = Instant::now() + options.interval;
    let mut decoder = Decoder::default();
    let mut dirty = true;
    loop {
        if terminal::stopping() {
            break;
        }
        let (width, height) = terminal::size();
        state.clamp_processes(snapshot.processes.len(), (width, height));
        if dirty
            || previous.as_ref().map(|f: &ui::Frame| (f.width, f.height)) != Some((width, height))
        {
            let frame = ui::render(
                &View {
                    snapshot: &snapshot,
                    log: &log,
                    state: &state,
                    project: &project,
                    host: &host,
                    cores,
                    interval_ms: options.interval.as_millis() as u64,
                },
                width,
                height,
            );
            {
                let diff = frame.ansi_diff(previous.as_ref(), options.color);
                if !diff.is_empty() {
                    let mut stdout = io::stdout().lock();
                    stdout.write_all(diff.as_bytes())?;
                    stdout.flush()?;
                }
            }
            previous = Some(frame);
            dirty = false;
        }
        let wait = next_tick
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100));
        match receive.recv_timeout(wait) {
            Ok(byte) => {
                if let Some(event) = decoder.feed(byte) {
                    dirty = true;
                    match event {
                        Event::Key(b'q' | 3 | 4) => break,
                        Event::Up | Event::Key(b'k') if !state.help => {
                            if state.process_detail.is_some() {
                                state.scroll_detail(true, 1, (width, height), &snapshot);
                            } else {
                                state.scroll(true, 1, &log, (width, height), &snapshot);
                            }
                        }
                        Event::Down | Event::Key(b'j') if !state.help => {
                            if state.process_detail.is_some() {
                                state.scroll_detail(false, 1, (width, height), &snapshot);
                            } else {
                                state.scroll(false, 1, &log, (width, height), &snapshot);
                            }
                        }
                        Event::Key(b'p' | b'\r' | b'\n') if !state.help => {
                            if state.process_detail.is_some() {
                                state.close_detail();
                            } else {
                                dirty = state.open_process(&snapshot, (width, height));
                            }
                        }
                        Event::Wheel { up, column, row } => {
                            dirty = state.wheel(up, column, row, (width, height), &snapshot, &log);
                        }
                        Event::Mouse { kind, column, row } => {
                            dirty = state.mouse(kind, column, row, (width, height), &snapshot);
                        }
                        Event::Key(b' ') => {
                            state.paused = !state.paused;
                        }
                        Event::Key(b'f') if !state.help && state.process_detail.is_none() => {
                            state.follow_log();
                        }
                        Event::Key(b'?') => {
                            state.dragging_divider = None;
                            state.help = !state.help;
                        }
                        _ => dirty = false,
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if (state.process_detail.is_some() || state.help) && decoder.take_escape() {
                    if state.help {
                        state.help = false;
                    } else {
                        state.close_detail();
                    }
                    dirty = true;
                }
            }
        }
        if Instant::now() >= next_tick {
            if !state.paused {
                state.tick += 1;
                let old_log = (!state.follow && state.log_history.is_none())
                    .then(|| (log.visible_lines(), log.cursor(), log.generation));
                snapshot = if options.demo {
                    monitor::demo(state.tick)
                } else {
                    sampler.sample()
                };
                state.clamp_processes(snapshot.processes.len(), (width, height));
                if options.demo {
                    demo_log(&mut log, state.tick);
                } else {
                    log.refresh();
                }
                state.sync_log_history(&log);
                state.estimate = if options.demo {
                    demo_estimate(state.tick)
                } else {
                    estimator.refresh(&log)
                };
                if let Some((old_lines, old_cursor, old_generation)) = old_log {
                    // Keep the viewed window stable while new lines arrive.
                    let new_lines = log.visible_lines();
                    state.offset = if log.generation != old_generation {
                        0
                    } else {
                        state
                            .offset
                            .saturating_add(ui::appended_log_rows(
                                &old_lines,
                                old_cursor,
                                &new_lines,
                                log.cursor(),
                                width,
                            ))
                            .min(ui::log_row_count(&new_lines, width).saturating_sub(1))
                    };
                }
                dirty = true;
            }
            next_tick = Instant::now() + options.interval;
        }
    }
    Ok(())
}

fn main() {
    let result = options(env::args().skip(1)).and_then(|options| match options {
        Some(options) => run(options).map_err(|e| e.to_string()),
        None => Ok(()),
    });
    if let Err(error) = result {
        eprintln!("lean-top: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_log_discovery_is_deterministic_and_ignores_non_files() {
        let directory = env::temp_dir().join(format!(
            "lean-top-log-default-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        assert_eq!(discover_log(&directory), None);
        for name in ["z.log", "a.log", "a.txt", ".hidden.log"] {
            std::fs::write(directory.join(name), "").unwrap();
        }
        std::fs::create_dir(directory.join("0.log")).unwrap();
        assert_eq!(discover_log(&directory), Some(directory.join("a.log")));
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(discover_log(&directory), None);
    }

    #[test]
    fn default_is_realtime_and_invalid_intervals_are_rejected() {
        assert_eq!(
            options(Vec::new()).unwrap().unwrap().interval,
            Duration::from_millis(500)
        );
        for value in ["0", "99", "60001", "-1", "abc"] {
            assert!(options(vec!["--interval-ms".into(), value.into()]).is_err());
        }
        assert!(options(vec!["--interval-ms".into()]).is_err());
        assert!(options(vec!["--unknown".into()]).is_err());
    }

    #[test]
    fn target_list_options_require_valid_paths_and_reject_demo_inputs() {
        assert!(options(vec!["--module-list".into()]).is_err());
        assert!(options(vec![
            "--module-list".into(),
            "/nonexistent/lean-top-modules".into()
        ])
        .is_err());
        assert!(options(vec![
            "--demo".into(),
            "--module-list".into(),
            "modules.txt".into()
        ])
        .is_err());
    }

    #[test]
    fn contradictory_demo_input_is_rejected() {
        assert!(options(vec!["--demo".into(), "--log".into(), "build.log".into()]).is_err());
    }
}
