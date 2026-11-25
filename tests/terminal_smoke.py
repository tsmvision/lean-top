#!/usr/bin/env python3
"""Exercise the actual binary through a Unix PTY; Python stdlib only."""
import errno
import fcntl
import os
from pathlib import Path
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


def exercise(binary, quit_key=None):
    master, slave = os.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 38, 111, 0, 0))
    process = subprocess.Popen(
        [binary, "--demo", "--interval-ms", "100"],
        stdin=slave, stdout=slave, stderr=slave,
    )

    def expect(needle, action=None):
        if action is not None:
            os.write(master, action)
        deadline = time.monotonic() + 5
        output = bytearray()
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError as exc:
                    if exc.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                output.extend(chunk)
                if needle in output:
                    return
        raise AssertionError(f"Did not see {needle!r}: {output[-2000:]!r}")

    try:
        expect(b"LEAN TOP")
        expect(b"PAUSED", b" ")
        expect(b"KEYBOARD", b"?")
        os.write(master, b"?")
        expect(b"SCROLL", b"\x1b[A")
        expect(b"FOLLOW ON", b"f")
        expect(b"DEMO", b" ")
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        expect(b"LEAN TOP")
        if quit_key is None:
            process.send_signal(signal.SIGTERM)
        else:
            os.write(master, quit_key)
        # Drain the repaint: a PTY with no emulator attached has a small buffer.
        # Waiting without reading could block the child in a stdout write.
        deadline = time.monotonic() + 5
        while process.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:
                try:
                    os.read(master, 65536)
                except OSError as exc:
                    if exc.errno != errno.EIO:
                        raise
        assert process.poll() == 0, "Monitor did not quit cleanly"
        assert termios.tcgetattr(slave) == before, "Terminal settings were not restored"
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)
        os.close(slave)


def main():
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/lean-top").resolve())
    plain = subprocess.check_output([binary, "--demo", "--snapshot"])
    assert b"\x1b" not in plain
    assert b"342 / 680" in plain
    bad = subprocess.run([binary, "--interval-ms", "99"], capture_output=True)
    assert bad.returncode != 0
    non_tty = subprocess.run([binary, "--demo"], capture_output=True)
    assert non_tty.returncode != 0 and b"requires a terminal" in non_tty.stderr
    with tempfile.TemporaryDirectory(prefix="lean-top-smoke-") as directory:
        fifo = os.path.join(directory, "not-a-log")
        os.mkfifo(fifo)
        rejected = subprocess.check_output([binary, "--snapshot", "--log", fifo], timeout=5)
        assert b"regular file" in rejected, "FIFO log should be rejected without blocking"
        log_path = Path(directory) / "build.log"
        log_path.write_text(
            "✔ [1/3] Built Example\n"
            "warning: diagnostic fraction [99/100]\n"
            "⣾ [2/20] Running Example:c.o",
            encoding="utf-8",
        )
        observed = subprocess.check_output([binary, "--snapshot", "--log", str(log_path)], timeout=5)
        assert b"2 / 20 jobs" in observed, "Use Lake's changing job total, including pending spinner output"
    for key in (b"q", b"\x03", None):
        exercise(binary, key)
    if sys.platform.startswith("linux"):
        live = subprocess.check_output([binary, "--snapshot"])
        assert b"require Linux /proc" not in live
        assert b"Disk capacity unavailable" not in live
        assert b"Sampling" not in live
    print("PASS: snapshot, invalid arguments, non-TTY, FIFO rejection, pause, help, scroll/follow, resize, q/Ctrl-C/SIGTERM, terminal restoration")


if __name__ == "__main__":
    main()
