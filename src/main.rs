mod monitor;
mod terminal;
mod ui;

use monitor::{LogTail, Sampler};
use std::env;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use ui::{View, ViewState};

const HELP: &str = "lean-top — lightweight real-time Lean build monitor

Usage: lean-top [OPTIONS]

Options:
  --project PATH      Project directory (default: current directory)
  --log PATH          Existing or future build log to follow
  --interval-ms MS    Refresh interval, 100–60000 ms (default: 500)
  --demo              Show simulated metrics and build output
  --snapshot          Print one plain-text frame and exit (110 x 38)
  --no-color          Disable colors; ANSI cursor control still requires a TTY
  -h, --help          Show this help
  -V, --version       Show version

Keys: q/Ctrl-C quit, Space pause, Up/Down or j/k scroll, f follow, ? help.
Metrics and processes are server-wide. Storage follows the project filesystem.
The monitor only reads; run lake build separately and capture its output.
";

struct Options {
    project: PathBuf,
    log: Option<PathBuf>,
    interval: Duration,
    demo: bool,
    snapshot: bool,
    color: bool,
}

fn options(args: impl IntoIterator<Item = String>) -> Result<Option<Options>, String> {
    let mut options = Options {
        project: env::current_dir().map_err(|e| e.to_string())?,
        log: None,
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
        log.push("✔ [340/680] Built RiemannHypothesis.ZetaBounds".into());
        log.push("warning: ExplicitFormula.lean:128: unused variable `h`".into());
    }
    if tick % 3 == 0 {
        log.push(format!(
            "⣾ [{done}/680] Running RiemannHypothesis.ExplicitFormula"
        ));
        log.updated = Some(Instant::now());
    }
}

fn run(options: Options) -> io::Result<()> {
    let mut sampler = Sampler::new(options.project.clone());
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
    state.push_cpu(snapshot.cpu);

    if options.snapshot {
        // A second small sample gives a meaningful CPU delta for a live snapshot.
        if !options.demo {
            std::thread::sleep(Duration::from_millis(100));
            snapshot = sampler.sample();
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
    let mut escape = 0;
    let mut dirty = true;
    loop {
        if terminal::stopping() {
            break;
        }
        let (width, height) = terminal::size();
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
                let mut stdout = io::stdout().lock();
                stdout.write_all(frame.ansi_diff(previous.as_ref(), options.color).as_bytes())?;
                stdout.flush()?;
            }
            previous = Some(frame);
            dirty = false;
        }
        let wait = next_tick
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100));
        match receive.recv_timeout(wait) {
            Ok(byte) => {
                dirty = true;
                match (escape, byte) {
                    (_, b'q' | 3 | 4) => break,
                    (_, 27) => escape = 1,
                    (1, b'[' | b'O') => escape = 2,
                    (2, b'A') | (0, b'k') => {
                        state.scroll(true, 1, &log);
                        escape = 0;
                    }
                    (2, b'B') | (0, b'j') => {
                        state.scroll(false, 1, &log);
                        escape = 0;
                    }
                    (_, b' ') => {
                        state.paused = !state.paused;
                        escape = 0;
                    }
                    (_, b'f') => {
                        state.follow = true;
                        state.offset = 0;
                        escape = 0;
                    }
                    (_, b'?') => {
                        state.help = !state.help;
                        escape = 0;
                    }
                    _ => escape = 0,
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= next_tick {
            if !state.paused {
                state.tick += 1;
                let old_cursor = log.cursor();
                let old_generation = log.generation;
                snapshot = if options.demo {
                    monitor::demo(state.tick)
                } else {
                    sampler.sample()
                };
                if options.demo {
                    demo_log(&mut log, state.tick);
                } else {
                    log.refresh();
                }
                if !state.follow {
                    // Keep the viewed window stable while new lines arrive.
                    let new_len = log.visible_lines().len();
                    state.offset = if log.generation != old_generation {
                        0
                    } else {
                        state
                            .offset
                            .saturating_add(log.cursor().saturating_sub(old_cursor) as usize)
                            .min(new_len.saturating_sub(1))
                    };
                }
                state.push_cpu(snapshot.cpu);
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
    fn contradictory_demo_input_is_rejected() {
        assert!(options(vec!["--demo".into(), "--log".into(), "build.log".into()]).is_err());
    }
}
