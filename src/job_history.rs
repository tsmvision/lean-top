//! Small, context-specific cache of totals from completed Lake builds.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_CONFIG: u64 = 1024 * 1024;
const MAX_CACHE: u64 = 512;
const HEADER: &str = "lean-top-job-history-v1";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub struct History {
    path: Option<PathBuf>,
    context: String,
    total: Option<u64>,
    error: Option<String>,
}

impl History {
    #[cfg(test)]
    pub(crate) fn isolated(project: &Path, build_key: &str, cache_root: &Path) -> Self {
        Self::with_cache_dir(project, build_key, cache_root)
    }

    pub fn new(project: &Path, build_key: &str) -> Self {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".cache"))
            });
        match base {
            Some(base) if base.is_absolute() => {
                Self::with_cache_dir(project, build_key, &base.join("lean-top/jobs"))
            }
            _ => Self::disabled("No absolute XDG_CACHE_HOME or HOME cache directory available"),
        }
    }

    fn disabled(error: impl ToString) -> Self {
        Self {
            path: None,
            context: String::new(),
            total: None,
            error: Some(error.to_string()),
        }
    }

    fn with_cache_dir(project: &Path, build_key: &str, cache_dir: &Path) -> Self {
        let context = match context_id(project, build_key) {
            Ok(context) => context,
            Err(error) => return Self::disabled(error),
        };
        let path = cache_dir.join(format!("{context}.jobs"));
        let (total, error) = match read_total(&path, &context) {
            Ok(total) => (Some(total), None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (None, None),
            Err(error) => (None, Some(format!("{}: {error}", path.display()))),
        };
        Self {
            path: Some(path),
            context,
            total,
            error,
        }
    }

    pub fn total(&self) -> Option<u64> {
        self.total
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn store(&mut self, total: u64) -> io::Result<()> {
        let path = self.path.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                self.error.as_deref().unwrap_or("History disabled"),
            )
        })?;
        let parent = path.parent().expect("cache file has a parent");
        fs::create_dir_all(parent)?;
        let (temp, mut file) = loop {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let temp = parent.join(format!(
                ".{}.{}.{sequence}.tmp",
                self.context,
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&temp) {
                Ok(file) => break (temp, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        let result = (|| {
            write!(file, "{HEADER}\n{}\n{total}\n", self.context)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result?;
        self.total = Some(total);
        self.error = None;
        Ok(())
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

// O_NONBLOCK ensures opening a FIFO cannot block before checking its file type.
fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const NONBLOCK: i32 = 0x800;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        const NONBLOCK: i32 = 0x4;
        options.custom_flags(NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("Expected a regular file"));
    }
    Ok(file)
}

fn bounded_read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut file = open_regular(path)?;
    if file.metadata()?.len() > limit {
        return Err(invalid("File exceeds history size limit"));
    }
    let mut result = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        // Read one extra byte to catch growth during the read without loading it all.
        let remaining = (limit + 1 - result.len() as u64).min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..remaining])?;
        if count == 0 {
            return Ok(result);
        }
        result.extend_from_slice(&buffer[..count]);
        if result.len() as u64 > limit {
            return Err(invalid("File exceeds history size limit"));
        }
    }
}

fn context_id(project: &Path, build_key: &str) -> io::Result<String> {
    let project = fs::canonicalize(project)?;
    if !project.is_dir() {
        return Err(invalid("Project is not a directory"));
    }
    let mut hash = 0xcbf29ce484222325u64;
    fn field(hash: &mut u64, bytes: &[u8]) {
        for byte in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    field(&mut hash, HEADER.as_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        field(&mut hash, project.as_os_str().as_bytes());
    }
    #[cfg(not(unix))]
    field(&mut hash, project.to_string_lossy().as_bytes());
    field(&mut hash, build_key.as_bytes());
    for name in [
        "lean-toolchain",
        "lakefile.lean",
        "lakefile.toml",
        "lake-manifest.json",
    ] {
        field(&mut hash, name.as_bytes());
        match bounded_read(&project.join(name), MAX_CONFIG) {
            Ok(bytes) => {
                field(&mut hash, b"present");
                field(&mut hash, &bytes);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A dangling link is an unreadable configuration, not an absent one.
                match fs::symlink_metadata(project.join(name)) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => {
                        field(&mut hash, b"missing")
                    }
                    _ => return Err(io::Error::new(error.kind(), format!("{name}: {error}"))),
                }
            }
            Err(error) => return Err(io::Error::new(error.kind(), format!("{name}: {error}"))),
        }
    }
    Ok(format!("{hash:016x}"))
}

fn read_total(path: &Path, context: &str) -> io::Result<u64> {
    let bytes = bounded_read(path, MAX_CACHE)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid("Invalid history encoding"))?;
    let mut lines = text.split('\n');
    if lines.next() != Some(HEADER) || lines.next() != Some(context) {
        return Err(invalid("Invalid history header or context"));
    }
    let number = lines
        .next()
        .ok_or_else(|| invalid("Missing history total"))?;
    if number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || lines.next() != Some("")
        || lines.next().is_some()
    {
        return Err(invalid("Invalid history total or incomplete record"));
    }
    number
        .parse()
        .map_err(|_| invalid("History total exceeds u64"))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        project: PathBuf,
        cache: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "lean-top-history-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            let project = root.join("project");
            fs::create_dir_all(&project).unwrap();
            let cache = root.join("cache");
            Self {
                root,
                project,
                cache,
            }
        }
        fn history(&self, key: &str) -> History {
            History::isolated(&self.project, key, &self.cache)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn roundtrip_including_zero_and_context_separation() {
        let fixture = Fixture::new();
        let mut history = fixture.history("default");
        assert_eq!(history.total(), None);
        assert_eq!(history.error(), None);
        history.store(0).unwrap();
        assert_eq!(fixture.history("default").total(), Some(0));
        history.store(123).unwrap();
        assert_eq!(fixture.history("default").total(), Some(123));
        assert_eq!(fixture.history("other").total(), None);
        let other = fixture.root.join("other");
        fs::create_dir(&other).unwrap();
        assert_eq!(
            History::with_cache_dir(&other, "default", &fixture.cache).total(),
            None
        );
        for name in [
            "lean-toolchain",
            "lakefile.lean",
            "lakefile.toml",
            "lake-manifest.json",
        ] {
            fs::write(fixture.project.join(name), "changed").unwrap();
            assert_ne!(fixture.history("default").context, history.context);
            fs::remove_file(fixture.project.join(name)).unwrap();
        }
    }

    #[test]
    fn rejects_corrupt_or_truncated_history() {
        let fixture = Fixture::new();
        let mut history = fixture.history("default");
        history.store(42).unwrap();
        let path = history.path.clone().unwrap();
        for bytes in [
            b"garbage".to_vec(),
            format!("{HEADER}\n{}\n42", history.context).into_bytes(),
            format!("{HEADER}\nwrong-context\n42\n").into_bytes(),
            vec![b'x'; 513],
            format!("{HEADER}\n{}\n18446744073709551616\n", history.context).into_bytes(),
        ] {
            fs::write(&path, bytes).unwrap();
            let loaded = fixture.history("default");
            assert_eq!(loaded.total(), None);
            assert!(loaded.error().is_some());
        }
    }

    #[test]
    fn rejects_large_and_nonregular_config() {
        let fixture = Fixture::new();
        let path = fixture.project.join("lakefile.lean");
        fs::write(&path, vec![b'x'; MAX_CONFIG as usize + 1]).unwrap();
        assert!(fixture.history("default").error().is_some());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(fixture.history("default").error().is_some());
    }

    #[test]
    fn failed_atomic_replace_keeps_total_and_cleans_temporary_file() {
        let fixture = Fixture::new();
        let mut history = fixture.history("default");
        history.store(42).unwrap();
        let path = history.path.clone().unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(history.store(99).is_err());
        assert_eq!(history.total(), Some(42));
        assert_eq!(fs::read_dir(&fixture.cache).unwrap().count(), 1);
        assert!(fixture.history("default").error().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn replaces_symlink_without_touching_destination_and_uses_private_mode() {
        use std::os::unix::{fs::symlink, fs::PermissionsExt};
        let fixture = Fixture::new();
        let mut history = fixture.history("default");
        fs::create_dir_all(&fixture.cache).unwrap();
        let target = fixture.root.join("untouched");
        fs::write(&target, "original").unwrap();
        let path = history.path.clone().unwrap();
        symlink(&target, &path).unwrap();
        history.store(99).unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "original");
        assert!(!fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fixture.history("default").total(), Some(99));
    }

    #[cfg(unix)]
    #[test]
    fn fifo_cache_and_config_are_rejected_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        extern "C" {
            fn mkfifo(path: *const std::os::raw::c_char, mode: u32) -> i32;
        }
        let fixture = Fixture::new();
        let history = fixture.history("default");
        fs::create_dir_all(&fixture.cache).unwrap();
        for path in [history.path.unwrap(), fixture.project.join("lakefile.lean")] {
            let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { mkfifo(path_c.as_ptr(), 0o600) }, 0);
            let loaded = fixture.history("default");
            assert_eq!(loaded.total(), None);
            assert!(loaded.error().is_some());
            fs::remove_file(path).unwrap();
        }
    }
}
