use super::*;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

#[test]
fn exactly_64_kib_append_keeps_first_complete_line() {
    let fixture = Fixture::new();
    let path = fixture.write("build.log", "initial\n");
    let mut log = LogTail::new(Some(path));
    log.refresh();
    let mut output = b"[1/2] first complete line\n".to_vec();
    output.resize(65535, b'x');
    output.push(b'\n');
    fixture.append("build.log", &output);
    log.refresh();
    assert_eq!(log.progress, Some((1, 2)));
    assert!(log.lines.iter().any(|s| s.contains("first complete line")));
}

#[test]
fn cursor_counts_new_lines_after_retention_window_fills() {
    let fixture = Fixture::new();
    let output = (0..200).map(|i| format!("{i}\n")).collect::<String>();
    let path = fixture.write("build.log", output);
    let mut log = LogTail::new(Some(path));
    log.refresh();
    let before = log.cursor();
    fixture.append("build.log", b"next\nnewest\n");
    log.refresh();
    assert_eq!(log.cursor() - before, 2);
    assert_eq!(log.lines.len(), 200);
}

#[test]
fn non_regular_log_is_rejected_without_reading() {
    let fixture = Fixture::new();
    let mut log = LogTail::new(Some(fixture.0.clone()));
    log.refresh();
    assert!(log.error.as_deref().unwrap().contains("regular file"));
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn linux_statvfs_reports_real_filesystem_capacity() {
    let directory = std::env::current_dir().unwrap();
    let (used, total, available) = disk_usage(&directory).expect("Linux statvfs failed");
    assert!(total > 0);
    assert!(used <= total);
    assert!(available <= total - used);
    assert_eq!(
        disk_usage(Path::new("/nonexistent-lean-top-test-path")),
        None
    );
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lean-top-tests-{}-{stamp}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        path
    }

    fn append(&self, name: &str, bytes: &[u8]) {
        fs::OpenOptions::new()
            .append(true)
            .open(self.0.join(name))
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn process(&self, pid: u32, name: &str, rss_kib: u64, args: &[u8]) {
        self.write(&format!("{pid}/comm"), format!("{name}\n"));
        self.write(
            &format!("{pid}/status"),
            format!("Name:\t{name}\nVmRSS:\t{rss_kib} kB\n"),
        );
        self.write(&format!("{pid}/cmdline"), args);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cpu_excludes_guest_counters_and_includes_iowait_in_idle() {
    // guest and guest_nice are already included in user and nice.
    assert_eq!(
        cpu_ticks("cpu 100 20 30 400 50 6 7 8 90 10\ncpu0 1 2 3 4\n"),
        Some((621, 450))
    );
    assert_eq!(cpu_ticks("cpu 100 20 30 400\n"), Some((550, 400)));
    assert_eq!(cpu_ticks("cpu0 100 20 30 400\n"), None);
    assert_eq!(cpu_ticks("cpu 1 2 3\n"), None);
    assert_eq!(cpu_ticks("cpu 1 no 3 4\n"), None);
    assert_eq!(cpu_ticks(""), None);
}

#[test]
fn sampler_uses_counter_deltas_and_available_memory() {
    let fixture = Fixture::new();
    fixture.write("stat", "cpu 100 0 100 800 0 0 0 0\n");
    fixture.write(
        "meminfo",
        "MemTotal: 1000 kB\nMemFree: 100 kB\nMemAvailable: 600 kB\nSwapTotal: 200 kB\nSwapFree: 150 kB\n",
    );
    fixture.write("loadavg", "1.25 2.50 3.75 1/40 99\n");
    let mut sampler = Sampler::new(fixture.0.clone());
    sampler.proc = fixture.0.clone();
    let first = sampler.sample();
    assert_eq!(first.cpu, None, "a first sample cannot provide a rate");
    assert_eq!(first.memory, Some((400 * 1024, 1000 * 1024)));
    assert_eq!(first.swap, Some((50 * 1024, 200 * 1024)));
    assert_eq!(first.load, "1.25 / 2.50 / 3.75");
    fixture.write("stat", "cpu 150 0 150 900 0 0 0 0\n");
    assert_eq!(sampler.sample().cpu, Some(50.0));
    assert_eq!(
        sampler.sample().cpu,
        None,
        "unchanged counters have no rate"
    );
    fixture.write("stat", "cpu 1 0 1 8 0 0 0 0\n");
    assert_eq!(
        sampler.sample().cpu,
        None,
        "counter resets must not underflow"
    );
}

#[test]
fn memory_fields_are_exact_and_invalid_or_overflowing_values_are_absent() {
    let text = "MemTotal: 2048 kB\nMemAvailable: 1024 kB\nVmRSS: 42 kB\n";
    assert_eq!(memory(text, "MemAvailable"), Some(1024 * 1024));
    assert_eq!(memory(text, "VmRSS"), Some(42 * 1024));
    assert_eq!(memory(text, "Mem"), None);
    assert_eq!(memory(text, "SwapTotal"), None);
    assert_eq!(memory("MemTotal: nope kB", "MemTotal"), None);
    assert_eq!(
        memory("MemTotal: 18446744073709551615 kB", "MemTotal"),
        None
    );
}

#[test]
fn process_scan_filters_names_and_orders_by_resident_memory() {
    let fixture = Fixture::new();
    fixture.process(10, "lake", 100, b"lake\0build\0");
    fixture.process(11, "lean", 800, b"lean\0Riemann/Zeta.lean\0");
    fixture.process(12, "python", 9000, b"python\0lean_script.py\0");
    fixture.process(13, "leanc", 200, b"leanc\0generated.c\0");
    fixture.write("self/comm", "lean\n");
    // A racing process can leave only its numeric directory behind.
    fs::create_dir(fixture.0.join("14")).unwrap();
    let found = processes(&fixture.0).unwrap();
    assert_eq!(
        found.iter().map(|p| p.pid).collect::<Vec<_>>(),
        vec![11, 13, 10]
    );
    assert_eq!(found[0].rss, 800 * 1024);
    assert_eq!(found[0].name, "lean");
    assert_eq!(found[0].command, "lean Riemann/Zeta.lean");
}

#[test]
fn process_arguments_are_bounded_and_missing_details_are_tolerated() {
    let fixture = Fixture::new();
    fixture.process(10, "lean", 20, &vec![b'x'; 20_000]);
    fixture.write("11/comm", "lake\n");
    let found = processes(&fixture.0).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].command.len(), 4096);
    assert_eq!(found[1].rss, 0);
    assert!(found[1].command.is_empty());
}

#[test]
fn log_append_preserves_split_utf8_and_escape_sequences() {
    let fixture = Fixture::new();
    let path = fixture.write("build.log", b"first\n\x1b[3");
    let mut log = LogTail::new(Some(path));
    log.refresh();
    assert_eq!(
        log.lines.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["first"]
    );
    fixture.append("build.log", &[b'2', b'm', 0xea, 0xb2]);
    log.refresh();
    fixture.append(
        "build.log",
        &[0x80, b' ', b'o', b'k', 0x1b, b'[', b'0', b'm', b'\n'],
    );
    log.refresh();
    assert_eq!(log.lines.back().map(String::as_str), Some("검 ok"));
    assert!(log.error.is_none());
    assert!(log.updated.is_some());
    let lines = log.visible_lines();
    log.refresh();
    assert_eq!(
        log.visible_lines(),
        lines,
        "unchanged files must not duplicate output"
    );
}

#[test]
fn log_carriage_returns_and_pending_line_are_visible() {
    let fixture = Fixture::new();
    let path = fixture.write(
        "build.log",
        b"[1/4] Building A\r[2/4] Building B\r\nworking",
    );
    let mut log = LogTail::new(Some(path));
    log.refresh();
    assert_eq!(log.progress, Some((2, 4)));
    assert_eq!(log.lines.len(), 2);
    assert_eq!(
        log.visible_lines().last().map(String::as_str),
        Some("working")
    );
    fixture.append("build.log", b"...\n");
    log.refresh();
    assert_eq!(log.lines.back().map(String::as_str), Some("working..."));
}

#[test]
fn progress_accepts_lake_counts_and_rejects_invalid_totals() {
    assert_eq!(progress("✔ [ 12 / 24 ] Built Riemann.Zeta"), Some((12, 24)));
    assert_eq!(progress("[trace] [24/24] done"), Some((24, 24)));
    assert_eq!(progress("[0/10] starting"), Some((0, 10)));
    for line in [
        "[1/0]",
        "[11/10]",
        "[-1/10]",
        "[a/10]",
        "[1/10",
        "ordinary message",
    ] {
        assert_eq!(progress(line), None, "unexpected progress in {line:?}");
    }
}

#[test]
fn log_rotation_discards_previous_run_and_progress() {
    let fixture = Fixture::new();
    let path = fixture.write("build.log", "[9/10] old\nunfinished");
    let mut log = LogTail::new(Some(path));
    log.refresh();
    fs::rename(fixture.0.join("build.log"), fixture.0.join("build.log.1")).unwrap();
    fixture.write("build.log", "new run\n");
    log.refresh();
    assert_eq!(log.visible_lines(), vec!["new run"]);
    assert_eq!(log.progress, None);
}

#[test]
fn log_truncation_resets_pending_bytes_and_progress() {
    let fixture = Fixture::new();
    let path = fixture.write("build.log", "[9/10] old output\nunfinished");
    let mut log = LogTail::new(Some(path));
    log.refresh();
    fixture.write("build.log", "fresh\n");
    log.refresh();
    assert_eq!(log.visible_lines(), vec!["fresh"]);
    assert_eq!(log.progress, None);
}

#[test]
fn log_missing_file_reports_error_then_recovers_without_restarting_monitor() {
    let fixture = Fixture::new();
    let mut log = LogTail::new(Some(fixture.0.join("later.log")));
    log.refresh();
    assert!(log.error.is_some());
    fixture.write("later.log", "[1/2] ready\n");
    log.refresh();
    assert!(log.error.is_none());
    assert_eq!(log.progress, Some((1, 2)));
}

#[test]
fn log_keeps_recent_window_and_bounds_unterminated_lines() {
    let fixture = Fixture::new();
    let text = (0..1000).map(|i| format!("line {i}\n")).collect::<String>();
    let path = fixture.write("build.log", text);
    let mut log = LogTail::new(Some(path));
    log.refresh();
    assert_eq!(log.lines.len(), 200);
    assert_eq!(log.lines.front().map(String::as_str), Some("line 800"));
    assert_eq!(log.lines.back().map(String::as_str), Some("line 999"));
    fixture.append("build.log", &vec![b'x'; 10_000]);
    log.refresh();
    assert_eq!(log.pending.len(), 4096);
    fixture.append("build.log", b"\nnext\n");
    log.refresh();
    assert_eq!(log.lines.len(), 200);
    assert_eq!(log.lines.back().map(String::as_str), Some("next"));
}

#[test]
fn log_starts_at_recent_tail_of_large_file() {
    let fixture = Fixture::new();
    let mut text = "historical line\n".repeat(20_000);
    text.push_str("[5/8] latest module\n");
    let path = fixture.write("build.log", text);
    let mut log = LogTail::new(Some(path));
    log.refresh();
    assert_eq!(log.progress, Some((5, 8)));
    assert_eq!(
        log.lines.back().map(String::as_str),
        Some("[5/8] latest module")
    );
    assert!(log.lines.len() <= 200);
    assert!(log.pending.len() <= 4096);
    assert_eq!(
        log.position,
        fs::metadata(fixture.0.join("build.log")).unwrap().len()
    );
}

#[test]
fn cleaner_removes_terminal_commands_without_losing_text() {
    assert_eq!(
        clean("\x1b[31merror\x1b[0m\tmessage\x07"),
        "error    message"
    );
    assert_eq!(clean("before\x1b]0;hostile title\x07after"), "beforeafter");
    assert_eq!(
        clean("before\x1b]0;hostile title\x1b\\after"),
        "beforeafter"
    );
    assert_eq!(clean("검증 완료"), "검증 완료");
}

#[test]
fn missing_proc_produces_warnings_without_panicking() {
    let fixture = Fixture::new();
    let mut sampler = Sampler::new(fixture.0.clone());
    sampler.proc = fixture.0.join("missing-proc");
    let snapshot = sampler.sample();
    assert_eq!(snapshot.cpu, None);
    assert_eq!(snapshot.memory, None);
    assert!(snapshot.processes.is_empty());
    assert!(!snapshot.warnings.is_empty());
}
