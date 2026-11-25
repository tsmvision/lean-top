use super::*;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

#[test]
fn repeated_spinner_frames_do_not_append_or_move_the_log_viewport() {
    let fixture = Fixture::new();
    let path = fixture.write("build.log", "✔ [1/4] Built A\n⣾ [2/4] Running B\r");
    let mut log = LogTail::new(Some(path));
    log.refresh();
    let before = log.visible_lines();
    let cursor = log.cursor();
    for chunk in ["⣷ [2/4] Running B\r", "⣯ [2/4] Running B", "\r\n"] {
        fixture.append("build.log", chunk.as_bytes());
        log.refresh();
        assert_eq!(log.visible_lines(), before);
        assert_eq!(log.cursor(), cursor);
    }
    fixture.append("build.log", "⣷ [3/4] Running C\r".as_bytes());
    log.refresh();
    assert_eq!(log.cursor(), cursor + 1);
    assert_eq!(log.visible_lines().last().unwrap(), "⣾ [3/4] Running C");
    fixture.append("build.log", b"warning: repeated\nwarning: repeated\n");
    log.refresh();
    assert_eq!(
        log.lines
            .iter()
            .filter(|s| *s == "warning: repeated")
            .count(),
        2
    );
}

#[test]
fn exactly_64_kib_append_keeps_first_complete_line() {
    let fixture = Fixture::new();
    let path = fixture.write("build.log", "initial\n");
    let mut log = LogTail::new(Some(path));
    log.refresh();
    let mut output = b"[1/2] Built first complete line\n".to_vec();
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
    assert_eq!(found[0].target.as_deref(), Some("Riemann/Zeta.lean"));
    assert_eq!(found[1].target.as_deref(), Some("generated.c"));
    assert_eq!(found[2].target.as_deref(), Some("build"));
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
    assert!(found[0].target.is_none());
    assert!(found[1].target.is_none());
}

#[test]
fn process_targets_preserve_argv_paths_and_skip_output_and_option_values() {
    let fixture = Fixture::new();
    fixture.process(10, "lean", 100,
        b"/opt/lean/bin/lean\0--root\0/srv/root.lean\0-o\0/srv/output.lean\0-i\0interface.lean\0/srv/proofs with spaces/Formula.lean\0");
    fixture.process(11, "leanc", 80,
        b"leanc\0-o\0generated.c\0-include\0prelude.c\0-I\0include.c\0/srv/native code/Formula.cpp\0");
    fixture.process(
        12,
        "lake",
        60,
        b"/opt/bin/lake\0-d\0/srv/proofs with spaces\0build\0Riemann\0",
    );
    let found = processes(&fixture.0).unwrap();
    assert_eq!(
        found[0].target.as_deref(),
        Some("/srv/proofs with spaces/Formula.lean")
    );
    assert!(found[0]
        .command
        .contains("'/srv/proofs with spaces/Formula.lean'"));
    assert_eq!(
        found[1].target.as_deref(),
        Some("/srv/native code/Formula.cpp")
    );
    assert_eq!(found[2].target.as_deref(), Some("build"));
}

#[test]
fn process_targets_do_not_fabricate_source_files_from_options_or_unrelated_args() {
    for (name, args, expected) in [
        (
            "lean",
            b"lean\0-o\0output.lean\0--root=directory.lean\0".as_slice(),
            None,
        ),
        ("lean", b"lean\0--server\0", None),
        ("lean", b"lean\0--\0-Source.lean\0", Some("-Source.lean")),
        (
            "lean",
            b"lean\0--run\0Main.lean\0argument.lean\0",
            Some("Main.lean"),
        ),
        (
            "leanc",
            b"leanc\0-DVALUE=not-a-source.c\0source.cc\0",
            Some("source.cc"),
        ),
        ("lake", b"lake\0", None),
        (
            "lake",
            b"lake\0-j\08\0-f\0lakefile.lean\0build\0",
            Some("build"),
        ),
        ("lean", b"", None),
    ] {
        assert_eq!(process_arguments(name, args).1.as_deref(), expected);
    }
}

#[test]
fn command_display_retains_empty_and_quoted_arguments_without_execution() {
    let (command, target) = process_arguments("lean", b"lean\0\0/srv/it's a proof.lean\0");
    assert_eq!(command, "lean '' '/srv/it'\\''s a proof.lean'");
    assert_eq!(target.as_deref(), Some("/srv/it's a proof.lean"));
}

#[test]
fn relative_process_source_paths_resolve_against_cwd_when_available() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fixture.process(10, "lean", 100, b"lean\0Proofs with spaces/Formula.lean\0");
    fixture.process(11, "leanc", 80, b"leanc\0.lake/build/Formula.c\0");
    fixture.process(12, "lake", 60, b"lake\0build\0");
    fixture.process(13, "lean", 40, b"lean\0/srv/absolute/Formula.lean\0");
    fixture.process(14, "lean", 20, b"lean\0Relative.lean\0");
    for pid in [10, 11, 12, 13] {
        symlink("/srv/riemann", fixture.0.join(format!("{pid}/cwd"))).unwrap();
    }
    let found = processes(&fixture.0).unwrap();
    assert_eq!(
        found[0].target.as_deref(),
        Some("/srv/riemann/Proofs with spaces/Formula.lean")
    );
    assert_eq!(
        found[1].target.as_deref(),
        Some("/srv/riemann/.lake/build/Formula.c")
    );
    assert_eq!(
        found[2].target.as_deref(),
        Some("build"),
        "Lake commands are not source paths"
    );
    assert_eq!(
        found[3].target.as_deref(),
        Some("/srv/absolute/Formula.lean")
    );
    assert_eq!(
        found[4].target.as_deref(),
        Some("Relative.lean"),
        "Unreadable cwd retains captured relative path"
    );
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
    let updated = log.updated;
    log.refresh();
    assert_eq!(
        log.updated, updated,
        "unchanged logs retain their last-read timestamp"
    );
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
    assert_eq!(progress("⣾ [0/10] Running Riemann.Zeta"), Some((0, 10)));
    assert_eq!(
        progress("✔ [3/5] (Optional) Built Riemann.Zeta:c.o"),
        Some((3, 5))
    );
    for action in [
        "Ran",
        "Running",
        "Reused",
        "Reusing",
        "Replayed",
        "Replaying",
        "Unpacked",
        "Unpacking",
        "Fetched",
        "Fetching",
        "Built",
        "Building",
        "Canceled",
    ] {
        assert_eq!(progress(&format!("[4/10] {action} target")), Some((4, 10)));
    }
    for line in [
        "[1/0] Built X",
        "[11/10] Built X",
        "[-1/10] Built X",
        "[a/10] Built X",
        "[1/10",
        "ordinary message",
        "[trace] [24/24] done",
        "warning: proof step [9/10] Built X",
        "example.lean:12: error: [9/10] Built X",
        "[3/5] arbitrary diagnostic",
        "[+1/2] Built X",
        "[1/2] BuildingSomething",
    ] {
        assert_eq!(progress(line), None, "unexpected progress in {line:?}");
    }
}

#[test]
fn lake_job_total_can_grow_without_turning_into_a_file_count() {
    let fixture = Fixture::new();
    let path = fixture.write("build.log", "✔ [1/3] Built Riemann.Zeta\n");
    let mut log = LogTail::new(Some(path));
    log.refresh();
    assert_eq!(log.progress, Some((1, 3)));
    fixture.append("build.log", "✔ [2/8] Built Riemann.Zeta:c.o\n".as_bytes());
    log.refresh();
    assert_eq!(
        log.progress,
        Some((2, 8)),
        "Lake's latest denominator is authoritative"
    );
    fixture.append("build.log", b"warning: example ratio [99/100]\n");
    log.refresh();
    assert_eq!(
        log.progress,
        Some((2, 8)),
        "diagnostic fractions must not replace Lake counters"
    );
}

#[test]
fn running_progress_is_visible_before_a_line_delimiter() {
    let fixture = Fixture::new();
    let path = fixture.write("build.log", "✔ [1/10] Built X\n⣾ [2/");
    let mut log = LogTail::new(Some(path));
    log.refresh();
    assert_eq!(log.current_progress(), Some((1, 10)));
    fixture.append("build.log", b"20] Running Y");
    log.refresh();
    assert_eq!(log.current_progress(), Some((2, 20)));
    fixture.append("build.log", b"\r");
    log.refresh();
    assert_eq!(log.current_progress(), Some((2, 20)));
}

#[test]
fn log_rotation_discards_previous_run_and_progress() {
    let fixture = Fixture::new();
    let path = fixture.write("build.log", "[9/10] Built old\nunfinished");
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
    let path = fixture.write("build.log", "[9/10] Built old output\nunfinished");
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
    fixture.write("later.log", "[1/2] Built ready\n");
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
    text.push_str("[5/8] Built latest module\n");
    let path = fixture.write("build.log", text);
    let mut log = LogTail::new(Some(path));
    log.refresh();
    assert_eq!(log.progress, Some((5, 8)));
    assert_eq!(
        log.lines.back().map(String::as_str),
        Some("[5/8] Built latest module")
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
