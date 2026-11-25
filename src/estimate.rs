//! Fixed target totals and replayable, deduplicated Lean-module completions.
use crate::artifacts::Tracker;
use crate::monitor::{clean, progress, LogTail};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

const MAX_MODULES: usize = 1_000_000;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 2_000_000;

#[derive(Clone, Debug, Default)]
pub struct Estimate {
    pub files: Option<usize>,
    pub completed: Option<usize>,
    pub remaining_seconds: Option<f64>,
    pub total_seconds: Option<f64>,
    pub error: Option<String>,
}

struct Membership {
    modules: Arc<HashMap<String, usize>>,
    completed: Vec<u64>,
}

impl Membership {
    fn record(&mut self, module: &str) -> bool {
        let Some(&index) = self.modules.get(module) else {
            return false;
        };
        let bit = 1u64 << (index % 64);
        let word = &mut self.completed[index / 64];
        let fresh = *word & bit == 0;
        *word |= bit;
        fresh
    }
}

struct Replay {
    file: File,
    position: u64,
    pending: Vec<u8>,
}

impl Replay {
    fn read_to(
        &mut self,
        end: u64,
        budget: u64,
        mut record: impl FnMut(&str),
    ) -> std::io::Result<()> {
        let limit = end.min(self.position.saturating_add(budget));
        let mut bytes = [0u8; 16_384];
        while self.position < limit {
            let length = ((limit - self.position) as usize).min(bytes.len());
            let read = self.file.read(&mut bytes[..length])?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "log shortened during replay",
                ));
            }
            self.position += read as u64;
            for byte in &bytes[..read] {
                if *byte == b'\n' || *byte == b'\r' {
                    record(&clean(&String::from_utf8_lossy(&self.pending)));
                    self.pending.clear();
                } else if self.pending.len() < 4096 {
                    self.pending.push(*byte);
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct State {
    membership: Option<Membership>,
    total: Option<usize>,
    count: usize,
    available: bool,
    replay: Option<Replay>,
    error: Option<String>,
    artifacts: Option<Tracker>,
    artifact_error: Option<String>,
    unmatched: Option<String>,
}

impl State {
    fn record(&mut self, line: &str) {
        let Some(module) = built_module(line) else {
            if progress(line).is_some() && self.unmatched.is_none() {
                if let Some((_, status)) = line.split_once(']') {
                    let status = status
                        .trim()
                        .strip_prefix("(Optional)")
                        .unwrap_or(status.trim())
                        .trim();
                    if let Some(module) = status.split_whitespace().nth(1) {
                        if valid_module(module)
                            && self
                                .membership
                                .as_ref()
                                .is_some_and(|members| !members.modules.contains_key(module))
                        {
                            self.unmatched = Some(module.to_string());
                        }
                    }
                }
            }
            return;
        };
        let Some(membership) = &mut self.membership else {
            return;
        };
        if !membership.modules.contains_key(module) && self.unmatched.is_none() {
            self.unmatched = Some(module.to_string());
        }
        if membership.record(module) {
            self.count += 1;
        }
    }

    fn artifact(&mut self, module: &str) {
        if let Some(membership) = &mut self.membership {
            if !membership.modules.contains_key(module) && self.unmatched.is_none() {
                self.unmatched = Some(module.to_string());
            }
            if membership.record(module) {
                self.count += 1;
            }
        }
    }

    fn poll_artifacts(&mut self) {
        let result = self.artifacts.as_mut().map(Tracker::poll);
        match result {
            Some(Ok(modules)) => {
                for module in modules {
                    self.artifact(&module);
                }
            }
            Some(Err(error)) => self.artifact_error = Some(format!("Artifact tracking: {error}")),
            None => {}
        }
    }

    fn read(&mut self, end: u64, budget: u64) -> std::io::Result<()> {
        let Some(mut replay) = self.replay.take() else {
            return Ok(());
        };
        let result = replay.read_to(end, budget, |line| self.record(line));
        self.replay = Some(replay);
        result
    }
}

pub struct Estimator {
    project: PathBuf,
    manifest: Option<PathBuf>,
    catalog: Option<Arc<HashMap<String, usize>>>,
    generation: Option<u64>,
    receiver: Option<Receiver<State>>,
    restart: bool,
    state: Option<State>,
    samples: VecDeque<(Instant, usize)>,
    read_error: Option<String>,
}

impl Estimator {
    pub fn new(project: PathBuf, manifest: Option<PathBuf>) -> Self {
        Self {
            project,
            manifest,
            catalog: None,
            generation: None,
            receiver: None,
            restart: false,
            state: None,
            samples: VecDeque::new(),
            read_error: None,
        }
    }

    fn start(&mut self, log: &LogTail) {
        self.receiver = Some(start_worker(
            self.project.clone(),
            self.manifest.clone(),
            self.catalog.clone(),
            log.replay_file(),
            log.position(),
        ));
    }

    pub fn refresh(&mut self, log: &LogTail) -> Estimate {
        self.refresh_at(log, Instant::now())
    }

    fn refresh_at(&mut self, log: &LogTail, now: Instant) -> Estimate {
        let old = self.generation.replace(log.generation);
        if old != Some(log.generation) {
            self.state = None;
            self.samples.clear();
            self.read_error = None;
            if self.receiver.is_some() {
                self.restart = true;
            } else {
                self.start(log);
            }
        }
        if let Some(receiver) = &self.receiver {
            match receiver.try_recv() {
                Ok(state) => {
                    self.receiver = None;
                    if self.catalog.is_none() {
                        self.catalog = state
                            .membership
                            .as_ref()
                            .map(|members| members.modules.clone());
                    }
                    if self.restart {
                        self.restart = false;
                        self.start(log);
                    } else {
                        if state.available
                            && state
                                .replay
                                .as_ref()
                                .is_some_and(|r| r.position >= log.position())
                        {
                            self.samples.push_back((now, state.count));
                        }
                        self.state = Some(state);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.receiver = None;
                    if self.restart {
                        self.restart = false;
                        self.start(log);
                    } else {
                        self.read_error =
                            Some("Progress replay worker stopped unexpectedly".into());
                    }
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let Some(state) = &mut self.state else {
            return Estimate {
                files: self.catalog.as_ref().map(|catalog| catalog.len()),
                error: self.read_error.clone(),
                ..Estimate::default()
            };
        };
        if state.available {
            self.read_error = state
                .read(log.position(), 65536)
                .err()
                .map(|error| format!("Cannot update completed files: {error}"));
        }
        state.poll_artifacts();
        let caught_up = state
            .replay
            .as_ref()
            .map(|r| r.position >= log.position())
            .unwrap_or(state.available);
        if !caught_up {
            self.samples.clear();
        } else if state.available
            && self.samples.back().map(|(_, value)| *value) != Some(state.count)
        {
            if self.samples.len() == 32 {
                self.samples.pop_front();
            }
            self.samples.push_back((now, state.count));
        }
        let error = state.error.clone().or_else(|| self.read_error.clone()).or_else(|| state.artifact_error.clone()).or_else(|| (state.total == Some(0)).then(|| "No Lean source modules found; check --project".into())).or_else(|| {
            state.total.filter(|total| state.count > *total)
                .map(|_| "Completed modules exceed the configured total; check the total and build log".into())
        }).or_else(|| {
            if state.count == 0 { state.unmatched.as_ref().map(|module|
                format!("No project match for {module}; check --project/source layout")) } else { None }
        });
        let rate = self.samples.front().and_then(|&(start, first)| {
            let elapsed = now.saturating_duration_since(start).as_secs_f64();
            (caught_up && elapsed >= 1.0 && state.count > first)
                .then(|| (state.count - first) as f64 / elapsed)
        });
        let remaining = state
            .total
            .filter(|total| state.available && state.count <= *total)
            .map(|total| total - state.count);
        Estimate {
            files: state.total,
            completed: state.available.then_some(state.count),
            remaining_seconds: if error.is_some() {
                None
            } else {
                remaining.and_then(|left| {
                    if left == 0 {
                        Some(0.0)
                    } else {
                        rate.map(|rate| left as f64 / rate)
                    }
                })
            },
            total_seconds: if error.is_some() {
                None
            } else {
                state.total.and_then(|total| {
                    if total == 0 && state.available {
                        Some(0.0)
                    } else {
                        rate.map(|rate| total as f64 / rate)
                    }
                })
            },
            error,
        }
    }
}

fn start_worker(
    project: PathBuf,
    manifest: Option<PathBuf>,
    catalog: Option<Arc<HashMap<String, usize>>>,
    source: std::io::Result<Option<File>>,
    end: u64,
) -> Receiver<State> {
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut state = State {
            membership: None,
            total: None,
            count: 0,
            available: false,
            replay: None,
            error: None,
            artifacts: None,
            artifact_error: None,
            unmatched: None,
        };
        let membership = match catalog {
            Some(modules) => {
                let count = modules.len();
                Ok((
                    Membership {
                        modules,
                        completed: vec![0; count.saturating_add(63) / 64],
                    },
                    Some(count),
                ))
            }
            None => load_membership(project.clone(), manifest.as_deref()),
        };
        match membership {
            Ok((membership, total)) => {
                state.membership = Some(membership);
                state.total = total;
            }
            Err(error) => {
                state.error = Some(error);
                let _ = send.send(state);
                return;
            }
        }
        match package_layout(&project)
            .and_then(|(_, artifacts)| Tracker::new(artifacts).map_err(|error| error.to_string()))
        {
            Ok((tracker, modules)) => {
                state.artifacts = Some(tracker);
                state.available = true;
                for module in modules {
                    state.artifact(&module);
                }
            }
            Err(error) => state.artifact_error = Some(format!("Artifact tracking: {error}")),
        }
        match source {
            Ok(Some(file)) => {
                state.replay = Some(Replay {
                    file,
                    position: 0,
                    pending: Vec::new(),
                });
                match state.read(end, u64::MAX) {
                    Ok(()) => state.available = true,
                    Err(error) => {
                        state.error = Some(format!("Cannot restore completed files: {error}"))
                    }
                }
            }
            Ok(None) if state.artifacts.is_some() => {}
            Ok(None) => state.error = Some("No build log selected".into()),
            Err(error) => state.error = Some(format!("Cannot open build log for replay: {error}")),
        }
        let _ = send.send(state);
    });
    receive
}

fn load_membership(
    project: PathBuf,
    manifest: Option<&Path>,
) -> Result<(Membership, Option<usize>), String> {
    let modules = if let Some(path) = manifest {
        manifest_modules(path)?
    } else {
        inventory_modules(&project)?
    };
    let count = modules.len();
    Ok((
        Membership {
            modules: Arc::new(modules),
            completed: vec![0; count.saturating_add(63) / 64],
        },
        Some(count),
    ))
}

fn package_layout(project: &Path) -> Result<(PathBuf, PathBuf), String> {
    let mut source = PathBuf::from(".");
    let mut build = PathBuf::from(".lake/build");
    let mut library = PathBuf::from("lib/lean");
    // Lean configurations are executable code; never evaluate them here.
    if !project.join("lakefile.lean").is_file() {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(target_os = "linux")]
        const NONBLOCK: i32 = 0x800;
        #[cfg(not(target_os = "linux"))]
        const NONBLOCK: i32 = 0x4;
        match fs::OpenOptions::new()
            .read(true)
            .custom_flags(NONBLOCK)
            .open(project.join("lakefile.toml"))
        {
            Ok(file) => {
                if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                    return Err("Lake config must be a regular file".into());
                }
                let mut text = String::new();
                file.take(65537)
                    .read_to_string(&mut text)
                    .map_err(|e| e.to_string())?;
                if text.len() > 65536 {
                    return Err("Lake config exceeds 64 KiB; use a module list".into());
                }
                for line in text.lines().map(str::trim) {
                    if line.starts_with('[') {
                        break;
                    }
                    let Some((key, value)) = line.split_once('=') else {
                        continue;
                    };
                    let target = match key.trim().trim_matches(|c| c == '\'' || c == '"') {
                        "srcDir" => &mut source,
                        "buildDir" => &mut build,
                        "leanLibDir" => &mut library,
                        _ => continue,
                    };
                    let value = value.trim();
                    let Some(quote @ ('\'' | '"')) = value.chars().next() else {
                        return Err("Lake paths must be literal strings".into());
                    };
                    let Some(end) = value[1..].find(quote) else {
                        return Err("Unterminated Lake path".into());
                    };
                    let path = &value[1..end + 1];
                    let rest = value[end + 2..].trim();
                    if path.contains('\\') || (!rest.is_empty() && !rest.starts_with('#')) {
                        return Err("Unsupported Lake path syntax".into());
                    }
                    let path = PathBuf::from(path);
                    if path.is_absolute()
                        || path
                            .components()
                            .any(|p| matches!(p, std::path::Component::ParentDir))
                    {
                        return Err("Lake paths must stay within the project".into());
                    }
                    *target = path;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot read Lake paths: {error}")),
        }
    }
    Ok((project.join(source), project.join(build).join(library)))
}

fn inventory_modules(project: &Path) -> Result<HashMap<String, usize>, String> {
    let (source, _) = package_layout(project)?;
    let project = source.as_path();
    if !fs::symlink_metadata(project)
        .map_err(|error| format!("Cannot inspect source directory: {error}"))?
        .is_dir()
    {
        return Err("Project must be a real directory".into());
    }
    let mut modules = HashMap::new();
    let mut stack = vec![project.to_path_buf()];
    let mut entries = 0usize;
    while let Some(directory) = stack.pop() {
        let children = fs::read_dir(&directory)
            .map_err(|error| format!("Cannot list {}: {error}", directory.display()))?;
        for child in children {
            let child = child.map_err(|error| format!("Cannot read directory entry: {error}"))?;
            entries += 1;
            if entries > MAX_ENTRIES {
                return Err("Project inventory exceeds 2,000,000 directory entries".into());
            }
            let kind = child
                .file_type()
                .map_err(|error| format!("Cannot inspect {}: {error}", child.path().display()))?;
            if kind.is_dir() {
                if !child.file_name().to_string_lossy().starts_with('.') {
                    stack.push(child.path());
                }
            } else if kind.is_file()
                && child.file_name() != "lakefile.lean"
                && child.path().extension().is_some_and(|ext| ext == "lean")
            {
                let path = child.path();
                let relative = path
                    .strip_prefix(project)
                    .map_err(|error| error.to_string())?
                    .with_extension("");
                let components = relative
                    .components()
                    .map(|part| part.as_os_str().to_str())
                    .collect::<Option<Vec<_>>>()
                    .ok_or("Project module paths must be UTF-8")?;
                if components
                    .iter()
                    .any(|part| part.contains('.') || *part != part.trim())
                {
                    return Err(format!(
                        "Project module path has an ambiguous dotted component or surrounding whitespace: {}",
                        path.display()
                    ));
                }
                let module = components.join(".");
                insert_module(&mut modules, module.as_bytes()).map_err(|error| {
                    format!("Invalid project module {}: {error}", path.display())
                })?;
            }
        }
    }
    Ok(modules)
}

fn manifest_modules(path: &Path) -> Result<HashMap<String, usize>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    #[cfg(target_os = "linux")]
    const NONBLOCK: i32 = 0x800;
    #[cfg(not(target_os = "linux"))]
    const NONBLOCK: i32 = 0x4;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(NONBLOCK)
        .open(path)
        .map_err(|error| format!("Cannot read module list: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        return Err("Module list must be a regular file no larger than 64 MiB".into());
    }
    let mut modules = HashMap::new();
    let mut pending = Vec::new();
    let mut comment = false;
    let mut bytes = [0u8; 16384];
    let mut consumed = 0u64;
    loop {
        let read = file
            .read(&mut bytes)
            .map_err(|error| format!("Cannot read module list: {error}"))?;
        if read == 0 {
            break;
        }
        consumed += read as u64;
        if consumed > MAX_MANIFEST_BYTES {
            return Err("Module list exceeds 64 MiB".into());
        }
        for byte in &bytes[..read] {
            if *byte == b'\n' {
                insert_module(&mut modules, &pending)?;
                pending.clear();
                comment = false;
            } else if *byte == b'#' {
                comment = true;
            } else if !comment {
                if pending.len() >= 1024 {
                    return Err("Module list contains an overlong line".into());
                }
                pending.push(*byte);
            }
        }
    }
    insert_module(&mut modules, &pending)?;
    Ok(modules)
}

fn insert_module(modules: &mut HashMap<String, usize>, bytes: &[u8]) -> Result<(), String> {
    let module = std::str::from_utf8(bytes)
        .map_err(|_| "Module list must be valid UTF-8")?
        .trim();
    if module.is_empty() {
        return Ok(());
    }
    if !valid_module(module) {
        return Err(format!(
            "Invalid module name: {}",
            module.chars().take(80).collect::<String>()
        ));
    }
    let next = modules.len();
    if !modules.contains_key(module) {
        if next >= MAX_MODULES {
            return Err("Target inventory exceeds 1,000,000 unique modules".into());
        }
        modules.insert(module.to_string(), next);
    }
    Ok(())
}

fn valid_module(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 512
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '\'')
        })
}

fn built_module(line: &str) -> Option<&str> {
    if line.trim_start().starts_with('✖') {
        return None;
    }
    progress(line)?;
    let tail = line.split_once(']')?.1.trim();
    let tail = tail
        .strip_prefix("(Optional)")
        .unwrap_or(tail)
        .trim()
        .strip_prefix("Built ")?;
    let module = tail.split_whitespace().next()?;
    valid_module(module).then_some(module)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    #[test]
    fn source_filename_whitespace_is_not_silently_normalized_into_another_module() {
        let fixture = Fixture::new();
        fixture.write(" Main.lean", b"contents are not read");
        assert!(inventory_modules(&fixture.0).is_err());
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "lean-module-progress-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn write(&self, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }
        fn append(&self, name: &str, contents: &str) {
            fs::OpenOptions::new()
                .append(true)
                .open(self.0.join(name))
                .unwrap()
                .write_all(contents.as_bytes())
                .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn settle(estimator: &mut Estimator, log: &LogTail) -> Estimate {
        for _ in 0..5000 {
            let result = estimator.refresh(log);
            if estimator.receiver.is_none()
                && estimator.state.as_ref().is_some_and(|state| {
                    state
                        .replay
                        .as_ref()
                        .map(|r| r.position >= log.position())
                        .unwrap_or(true)
                })
            {
                return result;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("Progress replay did not settle");
    }

    #[test]
    fn manifests_count_unique_modules_and_use_a_bitset() {
        let fixture = Fixture::new();
        let path = fixture.write("modules", "# targets\nProof.A\nProof.B # note\nProof.A\n");
        let (mut members, total) = load_membership(fixture.0.clone(), Some(&path)).unwrap();
        assert_eq!(total, Some(2));
        assert!(members.record("Proof.A"));
        assert!(!members.record("Proof.A"));
        assert!(!members.record("Other"));
        assert!(members.record("Proof.B"));
        assert_eq!(members.completed, [3]);
    }

    #[test]
    fn manifest_supports_175187_targets_without_a_20000_module_cutoff() {
        let fixture = Fixture::new();
        let path = fixture.write(
            "modules",
            (0..175187)
                .map(|i| format!("Proof.M{i}\n"))
                .collect::<String>(),
        );
        let (mut members, total) = load_membership(fixture.0.clone(), Some(&path)).unwrap();
        assert_eq!(total, Some(175187));
        assert!(members.record("Proof.M175186"));
        assert_eq!(members.completed.len(), 2738);
    }

    #[test]
    fn invalid_manifests_and_nonregular_files_fail_instead_of_truncating() {
        let fixture = Fixture::new();
        for contents in ["../A\n", "A:c.o\n", "A..B\n", "A/B\n", " A B\n"] {
            let path = fixture.write("modules", contents);
            assert!(load_membership(fixture.0.clone(), Some(&path)).is_err());
        }
        let path = fixture.write("modules", "A".repeat(513));
        assert!(load_membership(fixture.0.clone(), Some(&path)).is_err());
        let path = fixture.write("large", "");
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_MANIFEST_BYTES + 1)
            .unwrap();
        assert!(load_membership(fixture.0.clone(), Some(&path)).is_err());
        assert!(load_membership(fixture.0.clone(), Some(&fixture.0)).is_err());
        let fifo = fixture.0.join("module-fifo");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(load_membership(fixture.0.clone(), Some(&fifo)).is_err());
    }

    #[test]
    fn only_successful_source_module_records_count() {
        assert_eq!(built_module("✔ [2/9] Built A.B (1s)"), Some("A.B"));
        assert_eq!(
            built_module("✔ [2/9] (Optional) Built A.B (1s)"),
            Some("A.B")
        );
        assert_eq!(built_module("⚠ [2/9] Built A.B (1s)"), Some("A.B"));
        for line in [
            "warning: [2/9] Built A",
            "✔ [2/9] Built A:c.o",
            "✔ [2/9] Replayed A",
            "[2/9] Running A",
            "[2/9] Reused A",
            "✖ [2/9] Built A",
            "✖️ [2/9] Built A",
        ] {
            assert_eq!(built_module(line), None, "{line}");
        }
        let fixture = Fixture::new();
        fixture.write("A.lean", "");
        let (mut members, total) = load_membership(fixture.0.clone(), None).unwrap();
        assert_eq!(total, Some(1));
        assert!(members.record("A"));
        assert!(!members.record("A"));
        assert!(!members.record("app"));
        assert!(!members.record("Mathlib.External"));
    }

    #[test]
    fn full_history_and_restarts_restore_counts_beyond_the_display_ring() {
        let fixture = Fixture::new();
        let manifest = fixture.write(
            "modules",
            (0..620).map(|i| format!("M{i}\n")).collect::<String>(),
        );
        let history = (0..600)
            .map(|i| format!("✔ [{i}/50000] Built M{i} {}(1s)\n", "history ".repeat(20)))
            .collect::<String>();
        let path = fixture.write("build.log", format!("{history}✔ [700/50000] Built M0 (1s)\n✔ [701/50000] Built M1:c.o (1s)\n✔ [702/50000] Replayed M2\n✔ [703/50000] Built app\n"));
        let mut log = LogTail::new(Some(path));
        log.refresh();
        assert!(log.lines.len() <= 200);
        assert!(log.position() > 65536);
        let mut first = Estimator::new(fixture.0.clone(), Some(manifest.clone()));
        let estimate = settle(&mut first, &log);
        assert_eq!((estimate.completed, estimate.files), (Some(600), Some(620)));
        assert_eq!(estimate.total_seconds, None);
        assert_eq!(first.samples.len(), 1);
        let mut second = Estimator::new(fixture.0.clone(), Some(manifest.clone()));
        assert_eq!(settle(&mut second, &log).completed, Some(600));
        fixture.append("build.log", "✔ [704/50000] Built M600 (1s)\n");
        log.refresh();
        assert_eq!(settle(&mut first, &log).completed, Some(601));
        let mut third = Estimator::new(fixture.0.clone(), Some(manifest));
        let restored = settle(&mut third, &log);
        assert_eq!((restored.completed, restored.files), (Some(601), Some(620)));
        assert!(!fixture.0.join(".lake").exists());
    }

    #[test]
    fn bursts_and_partial_records_survive_the_replay_handoff_and_rotation() {
        let fixture = Fixture::new();
        let manifest = fixture.write(
            "modules",
            (0..600).map(|i| format!("M{i}\n")).collect::<String>(),
        );
        let path = fixture.write("build.log", "✔ [1/900] Built M0");
        let mut log = LogTail::new(Some(path));
        log.refresh();
        let mut estimator = Estimator::new(fixture.0.clone(), Some(manifest));
        assert_eq!(settle(&mut estimator, &log).completed, Some(0));
        let history = (1..600)
            .map(|i| format!("✔ [{i}/900] Built M{i} {}(1s)\n", "burst ".repeat(30)))
            .collect::<String>();
        fixture.append("build.log", &format!("\n{history}"));
        log.refresh();
        let estimate = settle(&mut estimator, &log);
        assert_eq!(estimate.completed, Some(600));
        assert!(
            estimator.samples.len() <= 1,
            "Disk catch-up must not act as compiler throughput"
        );
        fs::rename(fixture.0.join("build.log"), fixture.0.join("old.log")).unwrap();
        fixture.write("build.log", "✔ [1/9] Built M2\n");
        log.refresh();
        assert_eq!(settle(&mut estimator, &log).completed, Some(1));
    }

    #[test]
    fn completions_written_before_the_worker_result_is_published_are_not_lost() {
        let fixture = Fixture::new();
        let manifest = fixture.write(
            "modules",
            (0..600).map(|i| format!("M{i}\n")).collect::<String>(),
        );
        let path = fixture.write("build.log", "");
        let mut log = LogTail::new(Some(path));
        log.refresh();
        let file = log.replay_file().unwrap().unwrap();
        let (members, total) = load_membership(fixture.0.clone(), Some(&manifest)).unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        let mut estimator = Estimator::new(fixture.0.clone(), Some(manifest));
        estimator.generation = Some(log.generation);
        estimator.receiver = Some(receive);
        estimator.refresh(&log);
        fixture.append(
            "build.log",
            &(0..600)
                .map(|i| format!("✔ [{i}/10000] Built M{i} {}\n", "handoff ".repeat(25)))
                .collect::<String>(),
        );
        log.refresh();
        assert!(log.position() > 65536);
        send.send(State {
            membership: Some(members),
            total,
            count: 0,
            available: true,
            replay: Some(Replay {
                file,
                position: 0,
                pending: Vec::new(),
            }),
            error: None,
            ..State::default()
        })
        .unwrap();
        let estimate = settle(&mut estimator, &log);
        assert_eq!((estimate.completed, estimate.files), (Some(600), Some(600)));
        assert!(estimator.samples.len() <= 1);
    }

    #[test]
    fn automatic_inventory_reads_names_and_excludes_only_nonproject_sources() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fixture = Fixture::new();
        let external = Fixture::new();
        let unreadable = fixture.write("A.lean", "invalid Lean is not parsed");
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o0)).unwrap();
        fixture.write("Ns/B.lean", [0xff, 0xfe, 0x00]);
        fixture.write("target/T.lean", "");
        fixture.write("node_modules/N.lean", "");
        fixture.write(".hidden/Hidden.lean", "");
        fixture.write(".lake/packages/External.lean", "");
        fixture.write(".git/Git.lean", "");
        fixture.write("Ns/.hidden/Hidden.lean", "");
        fixture.write("lakefile.lean", "not a target source");
        let outside = external.write("Outside.lean", "");
        symlink(outside, fixture.0.join("Link.lean")).unwrap();
        symlink(&external.0, fixture.0.join("linked_directory")).unwrap();
        symlink(&fixture.0, fixture.0.join("loop")).unwrap();
        fs::create_dir(fixture.0.join("Directory.lean")).unwrap();
        let fifo = fixture.0.join("Pipe.lean");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let (mut members, total) = load_membership(fixture.0.clone(), None).unwrap();
        assert_eq!(total, Some(4));
        for module in ["A", "Ns.B", "target.T", "node_modules.N"] {
            assert!(members.record(module));
        }
        for module in ["lakefile", "Link", "Outside", "Pipe", "Hidden"] {
            assert!(!members.record(module));
        }
        fs::set_permissions(unreadable, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn invalid_or_unreadable_inventory_is_an_error_instead_of_a_partial_total() {
        let fixture = Fixture::new();
        fixture.write("Valid.lean", "");
        fixture.write("invalid-name.lean", "");
        assert!(load_membership(fixture.0.clone(), None)
            .err()
            .unwrap()
            .contains("Invalid project module"));
        assert!(load_membership(fixture.0.join("missing"), None).is_err());
    }

    #[test]
    fn dotted_filename_components_do_not_silently_collapse_distinct_source_files() {
        let fixture = Fixture::new();
        fixture.write("Foo.Bar.lean", "");
        fixture.write("Foo/Bar.lean", "");
        let error = load_membership(fixture.0.clone(), None).err().unwrap();
        assert!(error.contains("ambiguous dotted component"));
    }

    #[test]
    fn automatic_inventory_stays_fixed_until_a_new_estimator() {
        let fixture = Fixture::new();
        fixture.write("A.lean", "not even valid Lean");
        fixture.write("B.lean", [0xff, 0xfe, 0x00]);
        let path = fixture.write("build.log", "[1/100] Built A\n");
        let mut log = LogTail::new(Some(path));
        log.refresh();
        let mut estimator = Estimator::new(fixture.0.clone(), None);
        let estimate = settle(&mut estimator, &log);
        assert_eq!((estimate.completed, estimate.files), (Some(1), Some(2)));
        fixture.write("C.lean", "");
        fixture.append("build.log", "[2/200] Built C\n[3/99999] Built B\n");
        log.refresh();
        let estimate = settle(&mut estimator, &log);
        assert_eq!((estimate.completed, estimate.files), (Some(2), Some(2)));
        let mut restarted = Estimator::new(fixture.0.clone(), None);
        let estimate = settle(&mut restarted, &log);
        assert_eq!((estimate.completed, estimate.files), (Some(3), Some(3)));
        fs::rename(fixture.0.join("build.log"), fixture.0.join("old.log")).unwrap();
        fixture.write("build.log", "[1/100] Built A\n[2/100] Built C\n");
        log.refresh();
        let original_catalog = settle(&mut estimator, &log);
        assert_eq!(
            (original_catalog.completed, original_catalog.files),
            (Some(1), Some(2))
        );
        let newer_catalog = settle(&mut restarted, &log);
        assert_eq!(
            (newer_catalog.completed, newer_catalog.files),
            (Some(2), Some(3))
        );
    }

    #[test]
    fn a_live_read_error_preserves_the_confirmed_count() {
        let fixture = Fixture::new();
        let path = fixture.write("build.log", "some existing output\n");
        let mut log = LogTail::new(Some(path));
        log.refresh();
        let file = File::create(fixture.0.join("write-only.log")).unwrap();
        let mut estimator = Estimator::new(fixture.0.clone(), None);
        estimator.generation = Some(log.generation);
        estimator.state = Some(State {
            membership: Some(Membership {
                modules: Arc::new(HashMap::new()),
                completed: Vec::new(),
            }),
            total: Some(5),
            count: 3,
            available: true,
            replay: Some(Replay {
                file,
                position: 0,
                pending: Vec::new(),
            }),
            error: None,
            ..State::default()
        });
        let estimate = estimator.refresh(&log);
        assert_eq!((estimate.completed, estimate.files), (Some(3), Some(5)));
        assert!(estimate.error.unwrap().contains("Cannot update"));
        assert_eq!(estimate.remaining_seconds, None);
    }

    #[test]
    fn a_worker_result_from_an_old_log_is_discarded_after_rotation() {
        let fixture = Fixture::new();
        let manifest = fixture.write("modules", "A\nB\n");
        let path = fixture.write("build.log", "[1/8] Built A\n[2/8] Built B\n");
        let mut log = LogTail::new(Some(path));
        log.refresh();
        let old_file = log.replay_file().unwrap().unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        let mut estimator = Estimator::new(fixture.0.clone(), Some(manifest));
        estimator.generation = Some(log.generation);
        estimator.receiver = Some(receive);
        send.send(State {
            membership: None,
            total: Some(2),
            count: 2,
            available: true,
            replay: Some(Replay {
                file: old_file,
                position: log.position(),
                pending: Vec::new(),
            }),
            error: None,
            ..State::default()
        })
        .unwrap();
        fs::rename(fixture.0.join("build.log"), fixture.0.join("old.log")).unwrap();
        fixture.write("build.log", "[1/8] Built B\n");
        log.refresh();
        assert_eq!(estimator.refresh(&log).completed, None);
        assert_eq!(settle(&mut estimator, &log).completed, Some(1));
    }

    #[test]
    fn one_live_completion_after_a_second_can_supply_time_without_counting_history_as_speed() {
        let fixture = Fixture::new();
        let manifest = fixture.write("modules", "A\nB\nC\n");
        let path = fixture.write("build.log", "[1/8] Built A\n");
        let mut log = LogTail::new(Some(path));
        log.refresh();
        let mut estimator = Estimator::new(fixture.0.clone(), Some(manifest));
        assert_eq!(settle(&mut estimator, &log).completed, Some(1));
        let now = estimator.samples[0].0;
        fixture.append("build.log", "[2/9] Built B\n");
        log.refresh();
        let estimate = estimator.refresh_at(&log, now + Duration::from_secs(2));
        assert_eq!(estimate.total_seconds, Some(6.0));
        assert_eq!(estimate.remaining_seconds, Some(2.0));
        fixture.append("build.log", "[3/50000] Built C\n");
        log.refresh();
        let estimate = estimator.refresh_at(&log, now + Duration::from_secs(4));
        assert_eq!(estimate.files, Some(3));
        assert_eq!(estimate.remaining_seconds, Some(0.0));
    }

    #[test]
    fn no_log_uses_the_independently_scanned_artifact_baseline() {
        let fixture = Fixture::new();
        fixture.write("A.lean", "");
        fixture.write("B.lean", "");
        let mut estimator = Estimator::new(fixture.0.clone(), None);
        let estimate = settle(&mut estimator, &LogTail::default());
        assert_eq!(estimate.files, Some(2));
        assert_eq!(estimate.completed, Some(0));
        assert!(estimate.error.is_none());
    }

    #[test]
    fn ansi_progress_without_built_records_recovers_existing_completed_artifacts() {
        let fixture = Fixture::new();
        fixture.write("A.lean", "");
        fixture.write("B.lean", "");
        fixture.write(".lake/build/lib/lean/A.olean", "output");
        fixture.write(".lake/build/lib/lean/A.trace", "completed");
        let path = fixture.write("build.log", "⣾ [11535/11593] Running B\r");
        let mut log = LogTail::new(Some(path));
        log.refresh();
        let mut estimator = Estimator::new(fixture.0.clone(), None);
        let estimate = settle(&mut estimator, &log);
        assert_eq!((estimate.completed, estimate.files), (Some(1), Some(2)));
        fixture.append("build.log", "✔ [11536/11593] Built A\n");
        log.refresh();
        assert_eq!(
            estimator.refresh(&log).completed,
            Some(1),
            "Log plus artifact must count only once"
        );
        let mut restarted = Estimator::new(fixture.0.clone(), None);
        assert_eq!(settle(&mut restarted, &log).completed, Some(1));
    }

    #[test]
    fn toml_source_and_output_directories_match_lake_module_names() {
        let fixture = Fixture::new();
        fixture.write("lakefile.toml", "name = \"fixture\"\nsrcDir = \"src\"\nbuildDir = \"output\"\nleanLibDir = \"objects\"\n");
        fixture.write("src/Main.lean", "");
        fixture.write("src/Proof/A.lean", "");
        fixture.write("Unused.lean", "outside the configured source root");
        fixture.write("output/objects/Main.olean", "output");
        fixture.write("output/objects/Main.trace", "completed");
        let path = fixture.write("build.log", "✔ [1000/2000] Built Proof.A\n");
        let mut log = LogTail::new(Some(path));
        log.refresh();
        let mut estimator = Estimator::new(fixture.0.clone(), None);
        let estimate = settle(&mut estimator, &log);
        assert_eq!((estimate.completed, estimate.files), (Some(2), Some(2)));
        assert!(estimate.error.is_none(), "{:?}", estimate.error);
    }

    #[test]
    fn unmatched_dependency_activity_explains_why_project_count_is_zero() {
        let fixture = Fixture::new();
        fixture.write("Main.lean", "");
        let path = fixture.write("build.log", "⣾ [11535/11593] Running Mathlib.Other\r");
        let mut log = LogTail::new(Some(path));
        log.refresh();
        let mut estimator = Estimator::new(fixture.0.clone(), None);
        let estimate = settle(&mut estimator, &log);
        assert_eq!(estimate.completed, Some(0));
        assert!(estimate.error.unwrap().contains("Mathlib.Other"));
    }

    #[test]
    fn automatic_inventory_uses_source_data_not_lake_job_totals() {
        let fixture = Fixture::new();
        fixture.write("A.lean", "");
        let path = fixture.write(
            "build.log",
            "✔ [16000/50000] Built A\n✖ [16001/50000] Built A\n",
        );
        let mut log = LogTail::new(Some(path));
        log.refresh();
        let mut estimator = Estimator::new(fixture.0.clone(), None);
        let estimate = settle(&mut estimator, &log);
        assert_eq!(estimate.completed, Some(1));
        assert_eq!(estimate.files, Some(1));
        assert_eq!(estimate.total_seconds, None);
    }
}
