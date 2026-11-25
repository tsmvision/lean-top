//! Read-only artifact readiness tracking. Linux uses bounded inotify reads.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 2_000_000;

pub struct Tracker {
    root: PathBuf,
    backend: platform::Backend,
}

impl Tracker {
    pub fn new(root: PathBuf) -> io::Result<(Self, Vec<String>)> {
        let root = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()?.join(root)
        };
        let mut backend = platform::Backend::new()?;
        let ready = backend.initialize(&root)?;
        Ok((Self { root, backend }, ready))
    }

    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        self.backend.poll(&self.root)
    }
}

fn limit_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, message)
}

fn ready_module(root: &Path, path: &Path) -> Option<String> {
    if !matches!(path.extension()?.to_str()?, "olean" | "trace") {
        return None;
    }
    if !fs::metadata(path.with_extension("olean")).ok()?.is_file()
        || !fs::metadata(path.with_extension("trace")).ok()?.is_file()
    {
        return None;
    }
    let relative = path.strip_prefix(root).ok()?.with_extension("");
    let parts = relative
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    if parts.is_empty()
        || parts
            .iter()
            .any(|part| part.is_empty() || part.contains('.'))
    {
        return None;
    }
    Some(parts.join("."))
}

/// Watch each real directory before listing its children to cover creation races.
/// An ancestor of a missing root only follows the relevant child, not its peers.
fn scan(
    root: &Path,
    start: &Path,
    budget: &mut usize,
    mut watch: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<Vec<String>> {
    let mut ready = Vec::new();
    let mut stack = vec![start.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.is_dir() {
            if directory == root {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Artifact root must be a real directory; directory symlinks are not followed",
                ));
            }
            continue;
        }
        watch(&directory)?;
        if directory != root && root.starts_with(&directory) {
            if let Some(next) = root
                .strip_prefix(&directory)
                .ok()
                .and_then(|path| path.components().next())
            {
                stack.push(directory.join(next));
            }
            continue;
        }
        let children = match fs::read_dir(&directory) {
            Ok(children) => children,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for child in children {
            *budget += 1;
            if *budget > MAX_ENTRIES {
                return Err(limit_error(
                    "Artifact scan exceeds 2,000,000 directory entries",
                ));
            }
            let child = child?;
            let kind = match child.file_type() {
                Ok(kind) => kind,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if kind.is_dir() {
                stack.push(child.path());
            } else if kind.is_file() || kind.is_symlink() {
                // Initial scans inspect one side of each pair, avoiding duplicates.
                let path = child.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "olean")
                {
                    if let Some(module) = ready_module(root, &path) {
                        ready.push(module);
                    }
                }
            }
        }
    }
    Ok(ready)
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;
    use std::time::{Duration, Instant};

    pub struct Backend {
        last_scan: Instant,
    }

    impl Backend {
        pub fn new() -> io::Result<Self> {
            Ok(Self {
                last_scan: Instant::now(),
            })
        }

        pub fn initialize(&mut self, root: &Path) -> io::Result<Vec<String>> {
            self.last_scan = Instant::now();
            scan(root, root, &mut 0, |_| Ok(()))
        }

        pub fn poll(&mut self, root: &Path) -> io::Result<Vec<String>> {
            if self.last_scan.elapsed() < Duration::from_secs(1) {
                return Ok(Vec::new());
            }
            self.initialize(root)
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::{CString, OsString};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    const MAX_WATCHES: usize = 100_000;
    const ATTRIB: u32 = 0x4;
    const CLOSE_WRITE: u32 = 0x8;
    const MOVED_TO: u32 = 0x80;
    const CREATE: u32 = 0x100;
    const DELETE_SELF: u32 = 0x400;
    const MOVE_SELF: u32 = 0x800;
    const Q_OVERFLOW: u32 = 0x4000;
    const IGNORED: u32 = 0x8000;
    const ISDIR: u32 = 0x4000_0000;
    const MASK: u32 = ATTRIB | CLOSE_WRITE | MOVED_TO | CREATE | DELETE_SELF | MOVE_SELF;

    extern "C" {
        fn inotify_init1(flags: i32) -> i32;
        fn inotify_add_watch(fd: i32, path: *const std::os::raw::c_char, mask: u32) -> i32;
        fn inotify_rm_watch(fd: i32, wd: i32) -> i32;
        fn read(fd: i32, buffer: *mut std::ffi::c_void, count: usize) -> isize;
        fn close(fd: i32) -> i32;
    }

    pub struct Backend {
        fd: i32,
        directories: HashMap<i32, PathBuf>,
        watches: HashMap<PathBuf, i32>,
    }

    impl Drop for Backend {
        fn drop(&mut self) {
            // SAFETY: this backend owns the descriptor returned by inotify_init1.
            unsafe {
                close(self.fd);
            }
        }
    }

    impl Backend {
        pub fn new() -> io::Result<Self> {
            // Linux O_NONBLOCK and O_CLOEXEC; no blocking reads or leaked exec fds.
            let fd = unsafe { inotify_init1(0x800 | 0x80000) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                fd,
                directories: HashMap::new(),
                watches: HashMap::new(),
            })
        }

        fn watch(&mut self, directory: &Path) -> io::Result<()> {
            if self.watches.contains_key(directory) {
                return Ok(());
            }
            if self.watches.len() >= MAX_WATCHES {
                return Err(limit_error("Artifact watcher exceeds 100,000 directories"));
            }
            let path = CString::new(directory.as_os_str().as_bytes()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "Artifact path contains NUL")
            })?;
            // SAFETY: path is a live NUL-terminated CString; fd is owned here.
            let wd = unsafe { inotify_add_watch(self.fd, path.as_ptr(), MASK) };
            if wd < 0 {
                return Err(io::Error::last_os_error());
            }
            if let Some(old_path) = self.directories.insert(wd, directory.to_path_buf()) {
                self.watches.remove(&old_path);
            }
            self.watches.insert(directory.to_path_buf(), wd);
            Ok(())
        }

        fn forget(&mut self, wd: i32) {
            if let Some(path) = self.directories.remove(&wd) {
                self.watches.remove(&path);
            }
            // A deleted watch may already be gone. Removal errors need no retry.
            unsafe {
                inotify_rm_watch(self.fd, wd);
            }
        }

        fn anchor(&mut self, root: &Path, budget: &mut usize) -> io::Result<Vec<String>> {
            let mut ancestor = root.parent().unwrap_or(root);
            loop {
                match fs::symlink_metadata(ancestor) {
                    Ok(metadata) if metadata.is_dir() => break,
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "Artifact ancestor is not a real directory",
                        ))
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        ancestor = ancestor.parent().ok_or(error)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            let parent_is_new = !self.watches.contains_key(ancestor);
            let root_is_new = !self.watches.contains_key(root);
            if parent_is_new || root_is_new {
                scan(root, ancestor, budget, |path| self.watch(path))
            } else {
                Ok(Vec::new())
            }
        }

        pub fn initialize(&mut self, root: &Path) -> io::Result<Vec<String>> {
            self.anchor(root, &mut 0)
        }

        pub fn poll(&mut self, root: &Path) -> io::Result<Vec<String>> {
            let mut budget = 0;
            let mut ready = self.anchor(root, &mut budget)?;
            let mut bytes = [0u8; 65536];
            // SAFETY: bytes is writable for its full length; fd is nonblocking.
            let size = unsafe { read(self.fd, bytes.as_mut_ptr().cast(), bytes.len()) };
            if size < 0 {
                let error = io::Error::last_os_error();
                return if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) {
                    Ok(ready)
                } else {
                    Err(error)
                };
            }
            let size = size as usize;
            let mut index = 0;
            let mut rebuild = false;
            while index < size {
                if size - index < 16 {
                    return Err(limit_error("Truncated artifact event header"));
                }
                let wd = i32::from_ne_bytes(bytes[index..index + 4].try_into().unwrap());
                let mask = u32::from_ne_bytes(bytes[index + 4..index + 8].try_into().unwrap());
                let length =
                    u32::from_ne_bytes(bytes[index + 12..index + 16].try_into().unwrap()) as usize;
                let end = index
                    .checked_add(16)
                    .and_then(|start| start.checked_add(length))
                    .filter(|end| *end <= size)
                    .ok_or_else(|| limit_error("Invalid artifact event length"))?;
                if mask & Q_OVERFLOW != 0 {
                    return Err(limit_error(
                        "Artifact event queue overflow; restart monitoring to rescan",
                    ));
                }
                let directory = self.directories.get(&wd).cloned();
                if mask & (DELETE_SELF | MOVE_SELF | IGNORED) != 0 {
                    self.forget(wd);
                    rebuild = true;
                } else if let Some(directory) = directory {
                    let name = &bytes[index + 16..end];
                    let length = name
                        .iter()
                        .position(|byte| *byte == 0)
                        .unwrap_or(name.len());
                    let path = directory.join(OsString::from_vec(name[..length].to_vec()));
                    if path.starts_with(root) || root.starts_with(&path) {
                        if mask & ISDIR != 0 {
                            if mask & (CREATE | MOVED_TO) != 0 {
                                ready.extend(scan(root, &path, &mut budget, |path| {
                                    self.watch(path)
                                })?);
                            }
                        } else if mask & (CLOSE_WRITE | MOVED_TO | ATTRIB) != 0
                            || (mask & CREATE != 0
                                && fs::symlink_metadata(&path)
                                    .is_ok_and(|metadata| metadata.file_type().is_symlink()))
                        {
                            if let Some(module) = ready_module(root, &path) {
                                ready.push(module);
                            }
                        }
                    }
                }
                index = end;
            }
            if rebuild {
                ready.extend(scan(root, root, &mut budget, |path| self.watch(path))?);
                ready.extend(self.anchor(root, &mut budget)?);
            }
            Ok(ready)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "lean-artifacts-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, path: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"ready").unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn await_module(tracker: &mut Tracker, module: &str) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut observed = Vec::new();
        while Instant::now() < deadline {
            observed.extend(tracker.poll().unwrap());
            if observed.iter().any(|value| value == module) {
                return observed;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("Artifact module {module} not observed: {observed:?}");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_regular_trace_creation_waits_for_close_write() {
        use std::io::Write;
        let fixture = Fixture::new();
        fixture.write("arts/A.olean");
        let (mut tracker, _) = Tracker::new(fixture.0.join("arts")).unwrap();
        let mut trace = fs::File::create(fixture.0.join("arts/A.trace")).unwrap();
        trace.write_all(b"still open").unwrap();
        assert!(!tracker.poll().unwrap().iter().any(|module| module == "A"));
        drop(trace);
        assert!(await_module(&mut tracker, "A")
            .iter()
            .any(|module| module == "A"));
    }

    #[test]
    fn initial_scan_requires_a_matching_olean_and_trace_pair() {
        let fixture = Fixture::new();
        fixture.write("arts/Proof/A.olean");
        fixture.write("arts/Proof/A.trace");
        fixture.write("arts/Proof/Incomplete.olean");
        fixture.write("arts/Proof/Native.c.o");
        let (_, modules) = Tracker::new(fixture.0.join("arts")).unwrap();
        assert_eq!(modules, ["Proof.A"]);
    }

    #[test]
    fn a_new_trace_completes_an_existing_olean() {
        let fixture = Fixture::new();
        fixture.write("arts/Proof/A.olean");
        let (mut tracker, initial) = Tracker::new(fixture.0.join("arts")).unwrap();
        assert!(initial.is_empty());
        fixture.write("arts/Proof/A.trace");
        await_module(&mut tracker, "Proof.A");
        fixture.write("arts/Proof/A.trace");
        let repeated = await_module(&mut tracker, "Proof.A");
        assert!(repeated.into_iter().all(|module| module == "Proof.A"));
    }

    #[test]
    fn missing_root_and_nested_directories_are_detected_after_creation() {
        let fixture = Fixture::new();
        let (mut tracker, initial) = Tracker::new(fixture.0.join("build/lib/lean")).unwrap();
        assert!(initial.is_empty());
        fixture.write("build/lib/lean/Deep/Proof.olean");
        fixture.write("build/lib/lean/Deep/Proof.trace");
        let modules = await_module(&mut tracker, "Deep.Proof");
        assert_eq!(
            modules.into_iter().collect::<HashSet<_>>(),
            ["Deep.Proof".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn unrelated_extensions_do_not_look_like_ready_lean_modules() {
        let fixture = Fixture::new();
        fixture.write("arts/A.ir.trace");
        fixture.write("arts/A.olean");
        fixture.write("arts/A.c.o");
        let (_, initial) = Tracker::new(fixture.0.join("arts")).unwrap();
        assert!(initial.is_empty());
    }

    #[test]
    fn deleted_artifact_root_can_be_recreated() {
        let fixture = Fixture::new();
        fixture.write("arts/Old.olean");
        fixture.write("arts/Old.trace");
        let (mut tracker, initial) = Tracker::new(fixture.0.join("arts")).unwrap();
        assert_eq!(initial, ["Old"]);
        fs::remove_dir_all(fixture.0.join("arts")).unwrap();
        fixture.write("arts/New.olean");
        fixture.write("arts/New.trace");
        await_module(&mut tracker, "New");
    }

    #[test]
    fn file_symlinks_are_usable_but_directory_symlinks_are_not_traversed() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        fixture.write("cached.olean");
        fixture.write("cached.trace");
        fs::create_dir(fixture.0.join("arts")).unwrap();
        symlink(
            fixture.0.join("cached.olean"),
            fixture.0.join("arts/A.olean"),
        )
        .unwrap();
        symlink(
            fixture.0.join("cached.trace"),
            fixture.0.join("arts/A.trace"),
        )
        .unwrap();
        symlink(&fixture.0, fixture.0.join("arts/cycle")).unwrap();
        let (_, initial) = Tracker::new(fixture.0.join("arts")).unwrap();
        assert_eq!(initial, ["A"]);
        symlink(fixture.0.join("arts"), fixture.0.join("linked-root")).unwrap();
        assert!(Tracker::new(fixture.0.join("linked-root")).is_err());
    }

    #[test]
    fn newly_created_cached_file_symlinks_are_detected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        fixture.write("cached.olean");
        fixture.write("cached.trace");
        fs::create_dir(fixture.0.join("arts")).unwrap();
        let (mut tracker, initial) = Tracker::new(fixture.0.join("arts")).unwrap();
        assert!(initial.is_empty());
        symlink(
            fixture.0.join("cached.olean"),
            fixture.0.join("arts/A.olean"),
        )
        .unwrap();
        symlink(
            fixture.0.join("cached.trace"),
            fixture.0.join("arts/A.trace"),
        )
        .unwrap();
        await_module(&mut tracker, "A");
    }
}
