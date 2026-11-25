use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug, Default)]
pub struct Process {
    pub pid: u32,
    pub name: String,
    pub rss: u64,
    pub command: String,
    pub target: Option<String>,
}

#[derive(Default)]
pub struct Snapshot {
    pub cpu: Option<f64>,
    pub memory: Option<(u64, u64)>,
    pub swap: Option<(u64, u64)>,
    pub disk: Option<(u64, u64, u64)>,
    pub load: String,
    pub processes: Vec<Process>,
    pub warnings: Vec<String>,
}

pub fn cpu_ticks(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().next()?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if !line.starts_with("cpu ") || values.len() < 4 {
        return None;
    }
    Some((
        values.iter().sum(),
        values[3] + values.get(4).copied().unwrap_or(0),
    ))
}

pub fn memory(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == key)
            .then(|| {
                value
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()?
                    .checked_mul(1024)
            })
            .flatten()
    })
}

pub fn processes(proc: &Path) -> std::io::Result<Vec<Process>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(proc)?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse() else {
            continue;
        };
        let path = entry.path();
        let Ok(name) = fs::read_to_string(path.join("comm")) else {
            continue;
        };
        let name = name.trim();
        if !matches!(name, "lean" | "lake" | "leanc") {
            continue;
        }
        let rss = fs::read_to_string(path.join("status"))
            .ok()
            .and_then(|text| memory(&text, "VmRSS"))
            .unwrap_or(0);
        // Bound command reads, even for unusually large argument lists.
        let mut args = Vec::new();
        if let Ok(file) = File::open(path.join("cmdline")) {
            let _ = file.take(4096).read_to_end(&mut args);
        }
        let (command, target) = process_arguments(name, &args);
        let target = target.map(|target| {
            if name != "lake" && Path::new(&target).is_relative() {
                if let Ok(cwd) = fs::read_link(path.join("cwd")) {
                    if cwd.is_absolute() {
                        return cwd.join(&target).to_string_lossy().into_owned();
                    }
                }
            }
            target
        });
        result.push(Process {
            pid,
            name: name.into(),
            rss,
            command,
            target,
        });
    }
    result.sort_unstable_by_key(|p| std::cmp::Reverse(p.rss));
    Ok(result)
}

/// Interpret kernel-provided argv boundaries directly, without shell parsing.
fn process_arguments(name: &str, raw: &[u8]) -> (String, Option<String>) {
    let mut arguments: Vec<_> = raw
        .split(|byte| *byte == 0)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    if arguments.last().is_some_and(String::is_empty) {
        arguments.pop();
    }
    let command = arguments
        .iter()
        .map(|arg| {
            if !arg.is_empty()
                && arg
                    .chars()
                    .all(|c| c.is_alphanumeric() || "_./:-=+,@%".contains(c))
            {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut skip_value = false;
    let mut positional_only = false;
    let mut target = None;
    for argument in arguments.iter().skip(1) {
        if skip_value {
            skip_value = false;
            continue;
        }
        if !positional_only {
            if argument == "--" {
                positional_only = true;
                continue;
            }
            if matches!(
                argument.as_str(),
                "-o" | "-i"
                    | "--output"
                    | "--interface"
                    | "--root"
                    | "-R"
                    | "-I"
                    | "-L"
                    | "-l"
                    | "-D"
                    | "-isystem"
                    | "-iquote"
                    | "-include"
                    | "-imacros"
                    | "-MF"
                    | "-MT"
                    | "-MQ"
                    | "-d"
                    | "--dir"
                    | "-j"
                    | "--jobs"
                    | "-f"
                    | "--file"
            ) {
                skip_value = true;
                continue;
            }
            if argument.starts_with('-') {
                continue;
            }
        }
        let source = match name {
            "lean" => argument.ends_with(".lean"),
            "leanc" => [".c", ".cc", ".cpp", ".cxx"]
                .iter()
                .any(|suffix| argument.ends_with(suffix)),
            "lake" => !argument.is_empty(),
            _ => false,
        };
        if source {
            target = Some(argument.clone());
            break;
        }
    }
    (command, target)
}

pub struct Sampler {
    previous: Option<(u64, u64)>,
    project: PathBuf,
    proc: PathBuf,
}

impl Sampler {
    pub fn new(project: PathBuf) -> Self {
        Self {
            previous: None,
            project,
            proc: "/proc".into(),
        }
    }

    pub fn sample(&mut self) -> Snapshot {
        let mut out = Snapshot::default();
        if let Some(now) = fs::read_to_string(self.proc.join("stat"))
            .ok()
            .and_then(|s| cpu_ticks(&s))
        {
            if let Some(previous) = self.previous {
                let total = now.0.saturating_sub(previous.0);
                let idle = now.1.saturating_sub(previous.1);
                if total > 0 {
                    out.cpu = Some(
                        (100.0 * (total.saturating_sub(idle)) as f64 / total as f64)
                            .clamp(0.0, 100.0),
                    );
                }
            }
            self.previous = Some(now);
        } else {
            out.warnings
                .push("System metrics require Linux /proc; use --demo for a preview.".into());
        }
        if let Ok(text) = fs::read_to_string(self.proc.join("meminfo")) {
            out.memory = memory(&text, "MemTotal")
                .zip(memory(&text, "MemAvailable"))
                .map(|(total, available)| (total.saturating_sub(available), total));
            out.swap = memory(&text, "SwapTotal")
                .zip(memory(&text, "SwapFree"))
                .map(|(total, free)| (total.saturating_sub(free), total));
        }
        out.load = fs::read_to_string(self.proc.join("loadavg"))
            .unwrap_or_default()
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" / ");
        match processes(&self.proc) {
            Ok(p) => out.processes = p,
            Err(e) => out.warnings.push(format!("Process scan: {e}")),
        }
        // `df` is deliberately not spawned on each refresh. Linux mount capacity
        // comes directly from statvfs; the platform-specific ABI lives below.
        out.disk = disk_usage(&self.project);
        if out.disk.is_none() {
            out.warnings
                .push("Disk capacity unavailable for project filesystem.".into());
        }
        out
    }
}

#[cfg(target_os = "linux")]
fn disk_usage(path: &Path) -> Option<(u64, u64, u64)> {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int, c_ulong};
    use std::os::unix::ffi::OsStrExt;
    // Linux glibc/musl 64-bit statvfs ABI. Other targets use the safe fallback.
    #[cfg(target_pointer_width = "64")]
    {
        #[repr(C)]
        struct Statvfs {
            bsize: c_ulong,
            frsize: c_ulong,
            blocks: u64,
            bfree: u64,
            bavail: u64,
            files: u64,
            ffree: u64,
            favail: u64,
            fsid: c_ulong,
            flag: c_ulong,
            namemax: c_ulong,
            spare: [i32; 6],
        }
        extern "C" {
            fn statvfs(path: *const c_char, buf: *mut Statvfs) -> c_int;
        }
        let path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stat = std::mem::MaybeUninit::<Statvfs>::uninit();
        // SAFETY: CString is NUL-terminated and the correctly sized output is writable.
        if unsafe { statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: successful statvfs initialized the structure.
        let stat = unsafe { stat.assume_init() };
        Some((
            stat.blocks
                .saturating_sub(stat.bfree)
                .saturating_mul(stat.frsize),
            stat.blocks.saturating_mul(stat.frsize),
            stat.bavail.saturating_mul(stat.frsize),
        ))
    }
    #[cfg(not(target_pointer_width = "64"))]
    {
        let _ = path;
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn disk_usage(_path: &Path) -> Option<(u64, u64, u64)> {
    None
}

pub fn clean(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.peek() == Some(&'\\')) {
                            if c == '\x1b' {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if c == '\t' {
            output.push_str("    ");
        } else if !c.is_control() {
            output.push(c);
        }
    }
    output
}

pub fn progress(line: &str) -> Option<(u64, u64)> {
    // Lake reports job counters, not source-file counts. Match the status
    // record at the start of a sanitized line; diagnostic fractions are unrelated.
    let mut line = line.trim_start();
    if let Some(icon) = line.chars().next() {
        if matches!(icon, '✔' | 'ℹ' | '⚠' | '✖') || ('\u{2800}'..='\u{28ff}').contains(&icon)
        {
            line = line[icon.len_utf8()..]
                .trim_start_matches('\u{fe0f}')
                .trim_start();
        }
    }
    let (counts, caption) = line.strip_prefix('[')?.split_once(']')?;
    let (reported, total) = counts.split_once('/')?;
    let (reported, total) = (reported.trim(), total.trim());
    if ![reported, total]
        .iter()
        .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let caption = caption.trim_start();
    let caption = caption
        .strip_prefix("(Optional)")
        .unwrap_or(caption)
        .trim_start();
    let mut words = caption.split_whitespace();
    if !matches!(
        words.next()?,
        "Ran"
            | "Running"
            | "Reused"
            | "Reusing"
            | "Replayed"
            | "Replaying"
            | "Unpacked"
            | "Unpacking"
            | "Fetched"
            | "Fetching"
            | "Built"
            | "Building"
            | "Canceled"
    ) || words.next().is_none()
    {
        return None;
    }
    let (reported, total): (u64, u64) = (reported.parse().ok()?, total.parse().ok()?);
    (total > 0 && reported <= total).then_some((reported, total))
}

#[derive(Default)]
pub struct LogHistory {
    records: VecDeque<(String, u64, u64, bool)>,
    pub start: u64,
    pub end: u64,
    pub generation: u64,
}

impl LogHistory {
    pub fn lines(&self) -> Vec<String> {
        self.records.iter().map(|record| record.0.clone()).collect()
    }

    pub fn forward_start(&self) -> u64 {
        self.records
            .back()
            .filter(|record| !record.3)
            .map(|record| record.1)
            .unwrap_or(self.end)
    }

    // Keep a bounded cache around the reading position; older/newer data stays
    // in the original log file, rather than a second on-disk copy.
    pub fn extend(&mut self, mut page: Self, older: bool) -> Vec<String> {
        let mut removed_back = Vec::new();
        if older {
            self.start = page.start;
            page.records.append(&mut self.records);
            self.records = page.records;
        } else {
            while self
                .records
                .back()
                .is_some_and(|record| record.2 > page.start)
            {
                removed_back.push(self.records.pop_back().unwrap().0);
            }
            self.end = page.end;
            self.records.append(&mut page.records);
        }
        let removed = self.trim(older);
        if older {
            removed_back.extend(removed);
        }
        removed_back
    }

    fn trim(&mut self, older: bool) -> Vec<String> {
        let mut bytes: usize = self.records.iter().map(|record| record.0.len()).sum();
        let mut removed = Vec::new();
        while self.records.len() > 4096 || bytes > 262144 {
            let record = if older {
                self.records.pop_back().unwrap()
            } else {
                self.records.pop_front().unwrap()
            };
            bytes -= record.0.len();
            if older {
                self.end = record.1;
            } else {
                self.start = record.2;
            }
            removed.push(record.0);
        }
        removed
    }
}

#[derive(Default)]
pub struct LogTail {
    pub path: Option<PathBuf>,
    pub lines: VecDeque<String>,
    pub progress: Option<(u64, u64)>,
    pub updated: Option<SystemTime>,
    pub error: Option<String>,
    pub generation: u64,
    lines_added: u64,
    position: u64,
    identity: Option<(u64, u64)>,
    pending: Vec<u8>,
}

impl LogTail {
    pub fn history_before(&self, end: Option<u64>) -> std::io::Result<LogHistory> {
        let end = end.unwrap_or(self.position).min(self.position);
        self.history_window(end.saturating_sub(65536), end, true)
    }

    pub fn history_after(&self, start: u64) -> std::io::Result<LogHistory> {
        self.history_window(start, start.saturating_add(65536).min(self.position), false)
    }

    fn history_window(&self, start: u64, end: u64, older: bool) -> std::io::Result<LogHistory> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        #[cfg(target_os = "linux")]
        const NONBLOCK: i32 = 0x800;
        #[cfg(not(target_os = "linux"))]
        const NONBLOCK: i32 = 0x4;
        let path = self.path.as_ref().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no log file selected")
        })?;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || self.identity != Some((metadata.dev(), metadata.ino()))
            || metadata.len() < end
            || start > end
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "log changed; return to live output with f",
            ));
        }
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(end - start).read_to_end(&mut bytes)?;
        let mut first = 0;
        let mut last = bytes.len();
        if older && start > 0 {
            if let Some(delimiter) = bytes.iter().position(|b| *b == b'\n' || *b == b'\r') {
                first = delimiter + 1;
            }
        }
        if !older && end < self.position {
            if let Some(delimiter) = bytes.iter().rposition(|b| *b == b'\n' || *b == b'\r') {
                last = delimiter + 1;
            }
        }
        let mut page = LogHistory {
            start: start + first as u64,
            end: start + last as u64,
            generation: self.generation,
            ..LogHistory::default()
        };
        let mut begin = first;
        for index in first..last {
            if bytes[index] == b'\n' || bytes[index] == b'\r' {
                let line = Self::display_line(&String::from_utf8_lossy(
                    &bytes[begin..index.min(begin + 4096)],
                ));
                if !line.trim().is_empty()
                    && !(progress(&line).is_some()
                        && page.records.back().is_some_and(|record| record.0 == line))
                {
                    page.records.push_back((
                        line,
                        start + begin as u64,
                        start + index as u64 + 1,
                        true,
                    ));
                }
                begin = index + 1;
            }
        }
        if begin < last {
            let line = Self::display_line(&String::from_utf8_lossy(
                &bytes[begin..last.min(begin + 4096)],
            ));
            if !line.trim().is_empty()
                && !(progress(&line).is_some()
                    && page.records.back().is_some_and(|record| record.0 == line))
            {
                page.records.push_back((
                    line,
                    start + begin as u64,
                    page.end,
                    page.end < self.position,
                ));
            }
        }
        // The first page can contain many tiny records as well.
        page.trim(!older);
        Ok(page)
    }

    fn display_line(text: &str) -> String {
        let mut line = clean(text);
        // Lake's spinner is animation, not a new build event.
        if progress(&line).is_some() {
            let trimmed = line.trim_start();
            if let Some(icon) = trimmed.chars().next() {
                if ('\u{2800}'..='\u{28ff}').contains(&icon) {
                    line = format!("⣾{}", &trimmed[icon.len_utf8()..]);
                }
            }
        }
        line
    }

    fn pending_line(&self) -> Option<String> {
        let line = Self::display_line(&String::from_utf8_lossy(&self.pending));
        (!line.trim().is_empty()
            && !(progress(&line).is_some() && self.lines.back() == Some(&line)))
        .then_some(line)
    }

    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            ..Self::default()
        }
    }

    pub fn refresh(&mut self) {
        if self.path.is_none() {
            return;
        }
        self.error = self.read().err().map(|e| e.to_string());
    }

    fn read(&mut self) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(target_os = "linux")]
        const NONBLOCK: i32 = 0x800;
        #[cfg(not(target_os = "linux"))]
        const NONBLOCK: i32 = 0x4;
        // Do not let an accidental FIFO/device path block sampling or quitting.
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(NONBLOCK)
            .open(self.path.as_ref().unwrap())?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "log must be a regular file",
            ));
        }
        let identity = (metadata.dev(), metadata.ino());
        let reset = self.identity != Some(identity) || metadata.len() < self.position;
        if reset {
            self.generation = self.generation.wrapping_add(1);
            self.lines.clear();
            self.pending.clear();
            self.progress = None;
            self.updated = None;
            self.position = 0;
            self.identity = Some(identity);
        }
        let skip_partial = metadata.len().saturating_sub(self.position) > 65536;
        if skip_partial {
            self.position = metadata.len() - 65536;
            self.pending.clear();
            self.push("... older output skipped (bounded log reader)".into());
        }
        file.seek(SeekFrom::Start(self.position))?;
        let mut bytes = Vec::with_capacity(8192);
        file.take(65536).read_to_end(&mut bytes)?;
        self.position += bytes.len() as u64;
        if !bytes.is_empty() {
            self.updated = Some(SystemTime::now());
        }
        // Discard a partial first line in memory, rather than issuing one read
        // syscall per byte when attaching to a large or rapidly growing log.
        let start = if skip_partial {
            bytes
                .iter()
                .position(|b| *b == b'\n' || *b == b'\r')
                .map(|i| i + 1)
                .unwrap_or(bytes.len())
        } else {
            0
        };
        for byte in bytes.into_iter().skip(start) {
            if byte == b'\n' || byte == b'\r' {
                let line = Self::display_line(&String::from_utf8_lossy(&self.pending));
                if let Some(value) = progress(&line) {
                    self.progress = Some(value);
                }
                if !line.trim().is_empty() {
                    self.push(line);
                }
                self.pending.clear();
            } else if self.pending.len() < 4096 {
                self.pending.push(byte);
            }
        }
        Ok(())
    }

    pub fn push(&mut self, line: String) {
        let line = Self::display_line(&line);
        if progress(&line).is_some() && self.lines.back() == Some(&line) {
            return;
        }
        if self.lines.len() >= 200 {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        self.lines_added = self.lines_added.saturating_add(1);
    }

    pub fn cursor(&self) -> u64 {
        self.lines_added
            .saturating_add(u64::from(self.pending_line().is_some()))
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    pub fn current_progress(&self) -> Option<(u64, u64)> {
        // A live spinner may not have emitted its trailing CR/newline yet.
        progress(&clean(&String::from_utf8_lossy(&self.pending))).or(self.progress)
    }

    pub fn visible_lines(&self) -> Vec<String> {
        let mut lines: Vec<_> = self.lines.iter().cloned().collect();
        if let Some(line) = self.pending_line() {
            lines.push(line);
        }
        lines
    }
}

pub fn demo(tick: u64) -> Snapshot {
    let gib = 1024 * 1024 * 1024;
    Snapshot {
        cpu: Some(76.0 + 12.0 * (tick as f64 / 5.0).sin()),
        memory: Some(((11.6 * gib as f64) as u64, 32 * gib)),
        swap: Some((0, 4 * gib)),
        disk: Some((84 * gib, 240 * gib, 156 * gib)),
        load: "6.42 / 5.81 / 4.29".into(),
        processes: vec![
            Process {
                pid: 28416,
                name: "lean".into(),
                rss: (1.8 * gib as f64) as u64,
                command: "lean RiemannHypothesis/ExplicitFormula.lean".into(),
                target: Some("RiemannHypothesis/ExplicitFormula.lean".into()),
            },
            Process {
                pid: 28421,
                name: "lean".into(),
                rss: (1.2 * gib as f64) as u64,
                command: "lean RiemannHypothesis/ZetaBounds.lean".into(),
                target: Some("RiemannHypothesis/ZetaBounds.lean".into()),
            },
            Process {
                pid: 28431,
                name: "lean".into(),
                rss: 800 * 1024 * 1024,
                command: "lean RiemannHypothesis/ZeroFreeRegion.lean".into(),
                target: Some("RiemannHypothesis/ZeroFreeRegion.lean".into()),
            },
            Process {
                pid: 28441,
                name: "lean".into(),
                rss: 600 * 1024 * 1024,
                command: "lean RiemannHypothesis/CriticalStrip.lean".into(),
                target: Some("RiemannHypothesis/CriticalStrip.lean".into()),
            },
            Process {
                pid: 28451,
                name: "leanc".into(),
                rss: 96 * 1024 * 1024,
                command: "leanc .lake/build/ir/RiemannHypothesis/ZetaBounds.c".into(),
                target: Some(".lake/build/ir/RiemannHypothesis/ZetaBounds.c".into()),
            },
            Process {
                pid: 28390,
                name: "lake".into(),
                rss: 84 * 1024 * 1024,
                command: "lake build".into(),
                target: Some("build".into()),
            },
        ],
        warnings: vec![],
    }
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
