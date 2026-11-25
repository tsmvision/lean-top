//! Bounded, read-only estimates, never a replacement for Lake's build plan.
use crate::monitor::{LogTail, Snapshot};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Instant, SystemTime};

const MAX_MODULES: usize = 20_000;
const MAX_ENTRIES: usize = 100_000;
const HEADER_BYTES: u64 = 16_384;

#[derive(Clone, Debug, Default)]
pub struct Estimate {
    pub files: Option<usize>,
    pub completed: usize,
    pub additional: usize,
    pub remaining_seconds: Option<f64>,
    pub total_seconds: Option<f64>,
    pub scope: String,
}

struct Root {
    source: PathBuf,
    artifacts: PathBuf,
}

struct Node {
    path: PathBuf,
    stale: bool,
    imports: Vec<String>,
    limited: bool,
}

#[derive(Default)]
struct Plan {
    known: HashMap<String, PathBuf>,
    paths: HashMap<PathBuf, String>,
    pending: HashSet<String>,
    scope: String,
    partial: bool,
}

pub struct Estimator {
    project: PathBuf,
    receiver: Option<Receiver<Plan>>,
    plan: Option<Plan>,
    restart: bool,
    generation: Option<u64>,
    seen: Option<u64>,
    baseline: Option<u64>,
    display_cursor: Option<u64>,
    updated: Option<SystemTime>,
    completed: HashSet<String>,
    additional: HashSet<String>,
    frozen: bool,
    timings: VecDeque<f64>,
    observations: VecDeque<(Instant, usize)>,
}

impl Estimator {
    pub fn new(project: PathBuf) -> Self {
        let receiver = Some(start_scan(project.clone()));
        Self {
            project,
            receiver,
            plan: None,
            restart: false,
            generation: None,
            seen: None,
            baseline: None,
            display_cursor: None,
            updated: None,
            completed: HashSet::new(),
            additional: HashSet::new(),
            frozen: false,
            timings: VecDeque::new(),
            observations: VecDeque::new(),
        }
    }

    pub fn refresh(&mut self, log: &LogTail, snapshot: &Snapshot) -> Estimate {
        self.refresh_at(log, snapshot, Instant::now())
    }

    fn refresh_at(&mut self, log: &LogTail, snapshot: &Snapshot, now: Instant) -> Estimate {
        if self
            .generation
            .replace(log.generation)
            .is_some_and(|old| old != log.generation)
        {
            self.plan = None;
            self.seen = None;
            self.baseline = None;
            self.display_cursor = None;
            self.updated = None;
            self.completed.clear();
            self.additional.clear();
            self.frozen = false;
            self.observations.clear();
            if self.receiver.is_some() {
                self.restart = true;
            } else {
                self.receiver = Some(start_scan(self.project.clone()));
            }
        }
        if self.baseline.is_none() {
            let partial = u64::from(log.visible_lines().len() > log.lines.len());
            self.baseline = Some(log.cursor().saturating_sub(partial));
        }
        if let Some(receiver) = &self.receiver {
            match receiver.try_recv() {
                Ok(plan) => {
                    self.receiver = None;
                    if self.restart {
                        self.restart = false;
                        self.receiver = Some(start_scan(self.project.clone()));
                    } else {
                        self.plan = Some(plan);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.receiver = None;
                    if self.restart {
                        self.restart = false;
                        self.receiver = Some(start_scan(self.project.clone()));
                    } else {
                        self.plan = Some(Plan {
                            scope: "source scan unavailable".into(),
                            ..Plan::default()
                        });
                    }
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if self.plan.is_none() {
            return Estimate {
                scope: "scanning local sources/imports".into(),
                ..Estimate::default()
            };
        }
        let plan = self.plan.as_mut().unwrap();
        for process in snapshot
            .processes
            .iter()
            .filter(|process| process.name == "lean")
        {
            if let Some(module) = process
                .target
                .as_ref()
                .and_then(|target| plan.paths.get(Path::new(target)))
            {
                if !self.frozen {
                    plan.pending.insert(module.clone());
                } else if !plan.pending.contains(module) {
                    self.additional.insert(module.clone());
                }
            }
        }
        self.frozen = true;
        if self.seen.is_none()
            || self.updated != log.updated
            || self.display_cursor != Some(log.cursor())
        {
            self.observe(log, now);
        }
        self.result(snapshot, now)
    }

    fn observe(&mut self, log: &LogTail, now: Instant) {
        let visible = log.visible_lines();
        let partial = u64::from(visible.len() > log.lines.len());
        let cursor = log.cursor().saturating_sub(partial);
        let first = cursor.saturating_sub(log.lines.len() as u64);
        let previous = self.seen;
        let plan = self.plan.as_mut().unwrap();
        if previous.is_some_and(|old| first > old) && !plan.scope.contains("partial log") {
            plan.scope.push_str("; partial log history");
        }
        for (index, line) in log.lines.iter().enumerate() {
            let sequence = first + index as u64 + 1;
            if previous.is_some_and(|old| sequence <= old) {
                continue;
            }
            let Some((module, duration)) = built_module(line) else {
                continue;
            };
            if !plan.known.contains_key(module) {
                continue;
            }
            if let Some(duration) = duration {
                if self.timings.len() == 128 {
                    self.timings.pop_front();
                }
                self.timings.push_back(duration);
            }
            // Initial retained output calibrates timings only: it may predate
            // this monitor or refer to an earlier run of the same log file.
            if sequence > previous.or(self.baseline).unwrap_or(cursor)
                && self.completed.insert(module.to_string())
            {
                if !plan.pending.contains(module) {
                    self.additional.insert(module.to_string());
                }
                if self.observations.len() == 32 {
                    self.observations.pop_front();
                }
                self.observations.push_back((now, self.completed.len()));
            }
        }
        self.seen = Some(cursor);
        self.display_cursor = Some(log.cursor());
        self.updated = log.updated;
    }

    fn result(&self, snapshot: &Snapshot, now: Instant) -> Estimate {
        let plan = self.plan.as_ref().unwrap();
        if plan.known.is_empty() || (plan.pending.is_empty() && plan.partial) {
            return Estimate {
                scope: plan.scope.clone(),
                ..Estimate::default()
            };
        }
        let files = plan.pending.len();
        let completed = self
            .completed
            .iter()
            .filter(|module| plan.pending.contains(*module))
            .count();
        let remaining = files.saturating_sub(completed)
            + self
                .additional
                .iter()
                .filter(|module| !self.completed.contains(*module))
                .count();
        let work = files.saturating_add(self.additional.len());
        let observed_rate = self
            .observations
            .front()
            .zip(self.observations.back())
            .and_then(|(&(first_time, first_count), &(last_time, last_count))| {
                let span = now.saturating_duration_since(first_time).as_secs_f64();
                (last_count > first_count
                    && last_time
                        .saturating_duration_since(first_time)
                        .as_secs_f64()
                        >= 0.5
                    && span >= 1.0)
                    .then_some((last_count - first_count) as f64 / span)
            });
        let seed_rate = if self.timings.len() >= 3 {
            let mean = self.timings.iter().sum::<f64>() / self.timings.len() as f64;
            let workers = snapshot
                .processes
                .iter()
                .filter(|process| {
                    process.name == "lean"
                        && process
                            .target
                            .as_ref()
                            .is_some_and(|target| plan.paths.contains_key(Path::new(target)))
                })
                .count()
                .max(1);
            Some(workers as f64 / mean)
        } else {
            None
        };
        let rate = observed_rate
            .or(seed_rate)
            .filter(|rate| rate.is_finite() && *rate > 0.0);
        Estimate {
            files: Some(files),
            completed,
            additional: self.additional.len(),
            remaining_seconds: if remaining == 0 {
                Some(0.0)
            } else {
                rate.map(|r| remaining as f64 / r)
            },
            total_seconds: if work == 0 {
                Some(0.0)
            } else {
                rate.map(|r| work as f64 / r)
            },
            scope: plan.scope.clone(),
        }
    }
}

fn start_scan(project: PathBuf) -> Receiver<Plan> {
    let (send, receive) = mpsc::sync_channel(1);
    let _ = std::thread::Builder::new()
        .name("lean-estimate".into())
        .spawn(move || {
            let _ = send.send(scan(&project));
        });
    receive
}

fn root(package: &Path) -> Root {
    let src = read_bounded(&package.join("lakefile.toml"))
        .and_then(|text| {
            text.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                if key.trim() != "srcDir" {
                    return None;
                }
                let value = value.trim().strip_prefix('"')?.split_once('"')?.0;
                let path = Path::new(value);
                (!path.is_absolute()
                    && !path
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir)))
                .then(|| package.join(path))
            })
        })
        .unwrap_or_else(|| package.to_path_buf());
    Root {
        source: src,
        artifacts: package.join(".lake/build/lib/lean"),
    }
}

fn read_bounded(path: &Path) -> Option<String> {
    use std::os::unix::fs::OpenOptionsExt;
    if !plain_file(path) {
        return None;
    }
    #[cfg(target_os = "linux")]
    const NONBLOCK: i32 = 0x800;
    #[cfg(not(target_os = "linux"))]
    const NONBLOCK: i32 = 0x4;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(HEADER_BYTES).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn plain_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn local_sources(source: &Path) -> (Vec<PathBuf>, bool) {
    if !fs::symlink_metadata(source).is_ok_and(|metadata| metadata.is_dir()) {
        return (Vec::new(), true);
    }
    let mut stack = vec![source.to_path_buf()];
    let mut files = Vec::new();
    let mut entries = 0;
    let mut partial = false;
    while let Some(directory) = stack.pop() {
        let Ok(children) = fs::read_dir(directory) else {
            partial = true;
            continue;
        };
        for child in children.flatten() {
            entries += 1;
            if entries > MAX_ENTRIES || files.len() == MAX_MODULES {
                return (files, true);
            }
            let Ok(kind) = child.file_type() else {
                continue;
            };
            let name = child.file_name();
            if kind.is_dir()
                && !name.to_string_lossy().starts_with('.')
                && !matches!(name.to_str(), Some("target" | "node_modules"))
            {
                stack.push(child.path());
            } else if kind.is_file()
                && child.path().extension().is_some_and(|ext| ext == "lean")
                && name != "lakefile.lean"
            {
                files.push(child.path());
            }
        }
    }
    files.sort();
    (files, partial)
}

fn module_name(path: &Path, source: &Path) -> Option<String> {
    let relative = path.strip_prefix(source).ok()?.with_extension("");
    let name = relative
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join(".");
    valid_module(&name).then_some(name)
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

fn imports(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut output = Vec::new();
    let (mut index, mut depth) = (0, 0usize);
    while index < bytes.len() {
        if bytes[index..].starts_with(b"/-") {
            depth += 1;
            index += 2;
            output.push(b' ');
        } else if depth > 0 && bytes[index..].starts_with(b"-/") {
            depth -= 1;
            index += 2;
        } else if depth > 0 {
            index += 1;
        } else if bytes[index..].starts_with(b"--") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            output.push(b' ');
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    let text = String::from_utf8_lossy(&output);
    let tokens: Vec<_> = text.split_whitespace().collect();
    let mut result = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if matches!(tokens[index], "prelude" | "module" | "public" | "meta") {
            index += 1;
            continue;
        }
        if tokens[index] != "import" {
            break;
        }
        index += 1;
        if tokens.get(index) == Some(&"all") {
            index += 1;
        }
        while index < tokens.len()
            && valid_module(tokens[index])
            && !matches!(
                tokens[index],
                "import"
                    | "public"
                    | "meta"
                    | "namespace"
                    | "open"
                    | "section"
                    | "def"
                    | "theorem"
                    | "lemma"
                    | "set_option"
                    | "variable"
                    | "universe"
                    | "structure"
                    | "class"
                    | "inductive"
                    | "noncomputable"
                    | "attribute"
                    | "axiom"
                    | "abbrev"
                    | "example"
                    | "private"
                    | "unsafe"
                    | "protected"
                    | "end"
                    | "syntax"
                    | "macro"
                    | "elab"
                    | "instance"
            )
        {
            result.push(tokens[index].to_string());
            index += 1;
            if result.len() > 256 {
                return result;
            }
        }
    }
    result
}

fn node(path: PathBuf, artifacts: &Path, module: &str) -> Node {
    let artifact = artifacts
        .join(module.replace('.', "/"))
        .with_extension("olean");
    let stale = needs_rebuild(
        fs::metadata(&path).and_then(|meta| meta.modified()).ok(),
        fs::metadata(&artifact)
            .and_then(|meta| meta.modified())
            .ok(),
    );
    let source = read_bounded(&path);
    let mut imports = source
        .as_ref()
        .map(|text| imports(text))
        .unwrap_or_default();
    let limited = source.is_none() || imports.len() > 256;
    imports.truncate(256);
    Node {
        path,
        stale,
        imports,
        limited,
    }
}

fn needs_rebuild(source: Option<SystemTime>, artifact: Option<SystemTime>) -> bool {
    source
        .zip(artifact)
        .map(|(source, artifact)| source > artifact)
        .unwrap_or(true)
}

fn scan(project: &Path) -> Plan {
    let mut roots = vec![root(project)];
    if let Ok(packages) = fs::read_dir(project.join(".lake/packages")) {
        let mut packages: Vec<_> = packages
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .take(256)
            .map(|entry| entry.path())
            .collect();
        packages.sort();
        roots.extend(packages.iter().map(|package| root(package)));
    }
    let (local, mut partial) = local_sources(&roots[0].source);
    if local.is_empty() {
        return Plan {
            scope: "no local Lean sources found".into(),
            ..Plan::default()
        };
    }
    let mut nodes = HashMap::new();
    let mut queue = VecDeque::new();
    let mut scheduled = HashSet::new();
    for path in local {
        if let Some(module) = module_name(&path, &roots[0].source) {
            let entry = node(path, &roots[0].artifacts, &module);
            partial |= entry.limited;
            for imported in &entry.imports {
                if scheduled.len() >= MAX_MODULES {
                    partial = true;
                    break;
                }
                if scheduled.insert(imported.clone()) {
                    queue.push_back(imported.clone());
                }
            }
            nodes.insert(module, entry);
        } else {
            partial = true;
        }
    }
    while let Some(module) = queue.pop_front() {
        if nodes.contains_key(&module) {
            continue;
        }
        if nodes.len() >= MAX_MODULES {
            partial = true;
            break;
        }
        let found = roots.iter().find_map(|root| {
            let path = root
                .source
                .join(module.replace('.', "/"))
                .with_extension("lean");
            plain_file(&path).then(|| node(path, &root.artifacts, &module))
        });
        if let Some(entry) = found {
            partial |= entry.limited;
            for imported in &entry.imports {
                if scheduled.len() >= MAX_MODULES {
                    partial = true;
                    break;
                }
                if scheduled.insert(imported.clone()) {
                    queue.push_back(imported.clone());
                }
            }
            nodes.insert(module, entry);
        } else if !matches!(module.split('.').next(), Some("Init" | "Lean" | "Std")) {
            partial = true;
        }
    }
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    for (module, entry) in &nodes {
        for imported in &entry.imports {
            reverse
                .entry(imported.clone())
                .or_default()
                .push(module.clone());
        }
    }
    let mut pending: HashSet<_> = nodes
        .iter()
        .filter(|(_, entry)| entry.stale)
        .map(|(module, _)| module.clone())
        .collect();
    let mut queue: VecDeque<_> = pending.iter().cloned().collect();
    while let Some(module) = queue.pop_front() {
        if let Some(importers) = reverse.get(&module) {
            for importer in importers {
                if pending.insert(importer.clone()) {
                    queue.push_back(importer.clone());
                }
            }
        }
    }
    let known: HashMap<_, _> = nodes
        .into_iter()
        .map(|(name, node)| (name, node.path))
        .collect();
    let configured = project.join("lakefile.lean").is_file()
        || read_bounded(&project.join("lakefile.toml")).is_some_and(|text| {
            ["buildDir", "leanLibDir", "srcDir", "roots", "globs"]
                .iter()
                .any(|key| text.contains(key))
        });
    Plan {
        paths: known
            .iter()
            .map(|(module, path)| (path.clone(), module.clone()))
            .collect(),
        known,
        pending,
        partial,
        scope: format!(
            "local/import estimate, low confidence; from monitor scan{}{}",
            if partial { "; partial graph/scan" } else { "" },
            if configured {
                "; configured targets approximated"
            } else {
                ""
            }
        ),
    }
}

fn built_module(line: &str) -> Option<(&str, Option<f64>)> {
    crate::monitor::progress(line)?;
    let tail = line.split_once(']')?.1.trim();
    let tail = tail
        .strip_prefix("(Optional)")
        .unwrap_or(tail)
        .trim()
        .strip_prefix("Built ")?;
    let module = tail.split_whitespace().next()?;
    if !valid_module(module) {
        return None;
    }
    let seconds = tail.rsplit_once('(').and_then(|(_, value)| {
        let value = value.trim().strip_suffix(')')?.trim();
        let (number, factor) = if let Some(number) = value.strip_suffix("ms") {
            (number, 0.001)
        } else {
            (value.strip_suffix('s')?, 1.0)
        };
        let seconds = number.trim().parse::<f64>().ok()? * factor;
        (seconds.is_finite() && seconds > 0.0 && seconds <= 604800.0).then_some(seconds)
    });
    Some((module, seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, UNIX_EPOCH};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "lean-estimate-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn plan() -> Plan {
        let known: HashMap<_, _> = ["A", "B", "C"]
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    PathBuf::from(format!("/project/{name}.lean")),
                )
            })
            .collect();
        Plan {
            paths: known
                .iter()
                .map(|(module, path)| (path.clone(), module.clone()))
                .collect(),
            known,
            pending: ["A", "B"].iter().map(|s| s.to_string()).collect(),
            scope: "test estimate".into(),
            partial: false,
        }
    }
    fn estimator(plan: Plan) -> Estimator {
        Estimator {
            project: "/nonexistent-lean-estimate-test-project".into(),
            receiver: None,
            plan: Some(plan),
            restart: false,
            generation: None,
            seen: None,
            baseline: None,
            display_cursor: None,
            updated: None,
            completed: HashSet::new(),
            additional: HashSet::new(),
            frozen: false,
            timings: VecDeque::new(),
            observations: VecDeque::new(),
        }
    }

    #[test]
    fn imports_handle_header_modifiers_multiline_and_nested_comments() {
        let text = "module\n/- outer /- nested -/ -/\npublic import all A.B\n  C\nmeta import D -- comment\n\ndef later := \"import Bogus\"\n";
        assert_eq!(imports(text), vec!["A.B", "C", "D"]);
        assert_eq!(
            imports("prelude\nimport Init\nnamespace Foo\nimport Ignored"),
            vec!["Init"]
        );
        assert!(imports("def x := \"import A\"").is_empty());
    }

    #[test]
    fn timestamp_rule_is_only_a_conservative_staleness_estimate() {
        let old = UNIX_EPOCH + Duration::from_secs(1);
        let new = UNIX_EPOCH + Duration::from_secs(2);
        assert!(needs_rebuild(Some(new), Some(old)));
        assert!(!needs_rebuild(Some(old), Some(new)));
        assert!(!needs_rebuild(Some(old), Some(old)));
        assert!(needs_rebuild(Some(old), None));
        assert!(needs_rebuild(None, Some(new)));
    }

    #[test]
    fn scan_finds_reachable_package_modules_and_propagates_missing_artifacts() {
        let fixture = Fixture::new();
        fixture.write("Main.lean", "import all Mathlib.Basic\ndef main := 0\n");
        fixture.write(".lake/packages/mathlib/Mathlib/Basic.lean", "import Init\n");
        fixture.write(".lake/build/lib/lean/Main.olean", "cached");
        fixture.write(".git/Ignored.lean", "");
        fixture.write("target/Ignored.lean", "");
        fixture.write("lakefile.lean", "package example");
        let plan = scan(&fixture.0);
        assert_eq!(plan.known.len(), 2);
        assert_eq!(
            plan.pending,
            ["Main", "Mathlib.Basic"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        assert!(plan.scope.contains("low confidence"));
        assert!(plan.scope.contains("configured targets approximated"));
    }

    #[test]
    fn fresh_outputs_are_not_counted_and_denominator_does_not_shrink_later() {
        let fixture = Fixture::new();
        fixture.write("A.lean", "import B\n");
        fixture.write("B.lean", "import Init\n");
        fixture.write(".lake/build/lib/lean/A.olean", "");
        fixture.write(".lake/build/lib/lean/B.olean", "");
        assert!(scan(&fixture.0).pending.is_empty());
        fs::remove_file(fixture.0.join(".lake/build/lib/lean/B.olean")).unwrap();
        let frozen = scan(&fixture.0);
        assert_eq!(frozen.pending.len(), 2);
        fixture.write(".lake/build/lib/lean/B.olean", "");
        let mut estimator = estimator(frozen);
        let result = estimator.refresh(&LogTail::default(), &Snapshot::default());
        assert_eq!(result.files, Some(2));
        assert_eq!(result.remaining_seconds, None);
    }

    #[test]
    fn source_root_hint_and_symlink_boundaries_are_respected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let external = Fixture::new();
        fixture.write("lakefile.toml", "srcDir = \"src\"\n");
        fixture.write("src/App.lean", "import Init\n");
        external.write("Secret.lean", "");
        symlink(&external.0, fixture.0.join("src/linked")).unwrap();
        let plan = scan(&fixture.0);
        assert_eq!(plan.known.len(), 1);
        assert!(plan.known.contains_key("App"));
        assert!(!plan.known.contains_key("linked.Secret"));
    }

    #[test]
    fn native_cached_and_diagnostic_jobs_do_not_calibrate_compiler_timings() {
        assert_eq!(
            built_module("✔ [1/5] Built A (123ms)"),
            Some(("A", Some(0.123)))
        );
        assert_eq!(
            built_module("✔ [1/5] (Optional) Built A (1.2s)"),
            Some(("A", Some(1.2)))
        );
        for line in [
            "✔ [1/5] Built A:c.o (1.2s)",
            "✔ [1/5] Replayed A (1.2s)",
            "warning: [1/5] Built A (1s)",
        ] {
            assert!(built_module(line).is_none(), "{line}");
        }
        assert_eq!(built_module("✔ [1/5] Built A (NaNs)"), Some(("A", None)));
    }

    #[test]
    fn initial_history_only_seeds_timings_then_actual_completions_calibrate_eta() {
        let mut estimator = estimator(plan());
        let mut log = LogTail::default();
        for line in [
            "✔ [1/8] Built A (1s)",
            "✔ [2/8] Built B (2s)",
            "✔ [3/8] Built C (3s)",
        ] {
            log.push(line.into());
        }
        let now = Instant::now();
        let result = estimator.refresh_at(&log, &Snapshot::default(), now);
        assert_eq!((result.files, result.completed), (Some(2), 0));
        assert_eq!(result.total_seconds, Some(4.0));
        log.push("✔ [4/8] Built A (2s)".into());
        assert_eq!(
            estimator
                .refresh_at(&log, &Snapshot::default(), now + Duration::from_secs(1))
                .completed,
            1
        );
        log.push("✔ [5/8] Built B (2s)".into());
        let done = estimator.refresh_at(&log, &Snapshot::default(), now + Duration::from_secs(3));
        assert_eq!(done.remaining_seconds, Some(0.0));
        assert_eq!(done.total_seconds, Some(4.0));
        log.push("✔ [6/8] Built C (2s)".into());
        let expanded =
            estimator.refresh_at(&log, &Snapshot::default(), now + Duration::from_secs(4));
        assert_eq!(
            (expanded.files, expanded.completed, expanded.additional),
            (Some(2), 2, 1)
        );
    }

    #[test]
    fn fixed_total_does_not_grow_from_lake_totals_active_processes_or_extra_completions() {
        let mut estimator = estimator(plan());
        let mut log = LogTail::default();
        let now = Instant::now();
        let initial = estimator.refresh_at(&log, &Snapshot::default(), now);
        assert_eq!(initial.files, Some(2));
        let mut snapshot = Snapshot::default();
        snapshot.processes.push(crate::monitor::Process {
            pid: 42,
            name: "lean".into(),
            target: Some("/project/C.lean".into()),
            ..crate::monitor::Process::default()
        });
        log.push("✔ [8/900] Built C (2s)".into());
        let extra = estimator.refresh_at(&log, &snapshot, now + Duration::from_secs(1));
        assert_eq!(
            (extra.files, extra.completed, extra.additional),
            (Some(2), 0, 1)
        );
        log.push("✔ [9/9000] Built A (2s)".into());
        log.push("✔ [10/99999] Built B (2s)".into());
        log.push("✔ [11/99999] Built C (2s)".into());
        let finished = estimator.refresh_at(&log, &snapshot, now + Duration::from_secs(3));
        assert_eq!(
            (finished.files, finished.completed, finished.additional),
            (Some(2), 2, 1)
        );
    }

    #[test]
    fn completions_arriving_while_scanning_are_not_classified_as_initial_history() {
        let (send, receive) = mpsc::sync_channel(1);
        let mut estimator = estimator(plan());
        estimator.plan = None;
        estimator.receiver = Some(receive);
        let mut log = LogTail::default();
        log.push("✔ [1/8] Built C (1s)".into());
        let now = Instant::now();
        assert_eq!(
            estimator.refresh_at(&log, &Snapshot::default(), now).files,
            None
        );
        log.push("✔ [2/8] Built A (1s)".into());
        send.send(plan()).unwrap();
        let result = estimator.refresh_at(&log, &Snapshot::default(), now + Duration::from_secs(1));
        assert_eq!((result.files, result.completed), (Some(2), 1));
    }

    #[test]
    fn disconnected_scan_and_empty_or_incomplete_projects_do_not_invent_totals() {
        let (send, receive) = mpsc::sync_channel(1);
        drop(send);
        let mut estimator = estimator(plan());
        estimator.plan = None;
        estimator.receiver = Some(receive);
        let result = estimator.refresh(&LogTail::default(), &Snapshot::default());
        assert_eq!(result.files, None);
        assert!(result.scope.contains("unavailable"));
        let fixture = Fixture::new();
        assert!(scan(&fixture.0).known.is_empty());
        fixture.write("A.lean", "import Missing.Dependency\n");
        fixture.write(".lake/build/lib/lean/A.olean", "");
        let mut incomplete = self::estimator(scan(&fixture.0));
        assert_eq!(
            incomplete
                .refresh(&LogTail::default(), &Snapshot::default())
                .files,
            None
        );
    }

    #[test]
    fn active_known_compiler_is_evidence_even_when_timestamp_estimate_is_fresh() {
        let mut plan = plan();
        plan.pending.clear();
        let mut estimator = estimator(plan);
        let snapshot = Snapshot {
            processes: vec![crate::monitor::Process {
                name: "lean".into(),
                target: Some("/project/A.lean".into()),
                ..crate::monitor::Process::default()
            }],
            ..Snapshot::default()
        };
        let estimate = estimator.refresh(&LogTail::default(), &snapshot);
        assert_eq!(estimate.files, Some(1));
        assert_eq!(estimate.completed, 0);
        assert_eq!(estimate.remaining_seconds, None);
    }

    #[test]
    fn header_reader_rejects_nonregular_and_symlink_files() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        fixture.write("regular", "import Init");
        symlink(fixture.0.join("regular"), fixture.0.join("link")).unwrap();
        assert!(read_bounded(&fixture.0).is_none());
        assert!(read_bounded(&fixture.0.join("link")).is_none());
        let fifo = fixture.0.join("fifo");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(read_bounded(&fifo).is_none());
    }
}
