use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: std::os::raw::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

pub fn stopping() -> bool {
    STOP.load(Ordering::Relaxed)
}

pub struct Terminal {
    mode: String,
}

impl Terminal {
    pub fn enter() -> io::Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Interactive mode requires a terminal; use --snapshot for plain text.",
            ));
        }
        let saved = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()?;
        if !saved.status.success() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Cannot read terminal settings with stty.",
            ));
        }
        let terminal = Self {
            mode: String::from_utf8_lossy(&saved.stdout).trim().into(),
        };
        let status = Command::new("stty")
            .args(["raw", "-echo"])
            .stdin(Stdio::inherit())
            .status()?;
        if !status.success() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Cannot enable raw terminal mode.",
            ));
        }
        // POSIX signal handlers only set an atomic flag. All cleanup happens in Drop.
        extern "C" {
            fn signal(
                sig: std::os::raw::c_int,
                handler: extern "C" fn(std::os::raw::c_int),
            ) -> usize;
        }
        // SAFETY: these POSIX signal numbers and this C handler apply on supported Unix hosts.
        unsafe {
            signal(1, stop);
            signal(2, stop);
            signal(15, stop);
        }
        print!("\x1b[?1049h\x1b[?25l\x1b[2J");
        io::stdout().flush()?;
        Ok(terminal)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = Command::new("stty")
            .arg(&self.mode)
            .stdin(Stdio::inherit())
            .status();
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = stdout.flush();
    }
}

pub fn size() -> (usize, usize) {
    #[repr(C)]
    #[derive(Default)]
    struct WindowSize {
        rows: u16,
        columns: u16,
        xpixel: u16,
        ypixel: u16,
    }
    #[cfg(target_os = "linux")]
    const REQUEST: std::os::raw::c_ulong = 0x5413;
    #[cfg(target_os = "macos")]
    const REQUEST: std::os::raw::c_ulong = 0x40087468;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        extern "C" {
            fn ioctl(
                fd: std::os::raw::c_int,
                request: std::os::raw::c_ulong,
                ...
            ) -> std::os::raw::c_int;
        }
        let mut size = WindowSize::default();
        // SAFETY: TIOCGWINSZ writes a POSIX winsize to a valid, aligned output.
        if unsafe { ioctl(1, REQUEST, &mut size as *mut WindowSize) } == 0
            && size.columns > 0
            && size.rows > 0
        {
            // Reserve the last column to prevent automatic wrapping on the last row.
            return (
                (size.columns as usize).saturating_sub(1).clamp(1, 300),
                (size.rows as usize).clamp(1, 120),
            );
        }
    }
    (109, 38)
}
