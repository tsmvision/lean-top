#!/usr/bin/env python3
"""Exercise the actual binary through a Unix PTY; Python stdlib only."""
import errno
import fcntl
import os
from pathlib import Path
import re
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
    captured = bytearray()

    def read_chunk(timeout):
        if not select.select([master], [], [], timeout)[0]:
            return b""
        try:
            chunk = os.read(master, 65536)
        except OSError as exc:
            if exc.errno == errno.EIO:
                return b""
            raise
        captured.extend(chunk)
        return chunk

    def drain():
        """Finish a paused frame before attributing bytes to the next event."""
        output = bytearray()
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            chunk = read_chunk(0.15)
            if not chunk:
                break
            output.extend(chunk)
        return bytes(output)

    def expect(needle, action=None):
        if action is not None:
            os.write(master, action)
        deadline = time.monotonic() + 5
        output = bytearray()
        while time.monotonic() < deadline:
            chunk = read_chunk(0.1)
            if chunk:
                output.extend(chunk)
                if needle in output:
                    return bytes(output)
        raise AssertionError(f"Did not see {needle!r}: {output[-2000:]!r}")

    def event_output(action, needle=None):
        drain()
        os.write(master, action)
        output = expect(needle) if needle is not None else b""
        return output + drain()

    def assert_panel_only(output, first_row, last_row):
        rows = [int(row) for row in re.findall(rb"\x1b\[(\d+);1H", output)]
        assert rows, f"Expected panel repaint, received {output!r}"
        assert all(first_row <= row <= last_row for row in rows), (
            f"Wheel repainted another panel: rows={rows}, expected {first_row}..{last_row}"
        )
        assert b"\x1b[2J" not in output, "Wheel should not clear the full screen"

    try:
        expect(b"LEAN TOP")
        expect(b"PAUSED", b" ")
        drain()
        assert all(f"\x1b[?{mode}h".encode() in captured for mode in (1000,1002,1006))

        # Mouse coordinates are physical terminal coordinates (one-based).
        # Process scrolling must not change the log viewport or follow mode.
        output = event_output(b"\x1b[<65;10;22M", b"2-4 / 6")
        assert_panel_only(output, 19, 24)
        output = event_output(b"\x1b[<64;10;22M", b"1-3 / 6")
        assert_panel_only(output, 19, 24)

        # Scroll older/newer log records without repainting process rows.
        output = event_output(b"\x1b[<64;10;28M", b"SCROLL")
        assert_panel_only(output, 25, 36)
        output = event_output(b"\x1b[<65;10;28M")
        assert_panel_only(output, 25, 36)
        event_output(b"f", b"FOLLOW ON")

        assert event_output(b"\x1b[<65;5;7M") == b"", "Wheel outside panels must be ignored"
        assert event_output(b"\x1b[<0;10;22M") == b"", "Click must not scroll"
        assert event_output(b"\x1b[<65;10;22m") == b"", "Mouse release must not scroll"

        event_output(b"?", b"KEYBOARD")
        assert event_output(b"\x1b[<65;10;22M") == b"", "Help must block underlying mouse scrolling"
        event_output(b"?")

        # Drag the log header (row25) upward to row23, then downward to row29.
        event_output(b"\x1b[<0;10;25M", b"RESIZING")
        output = event_output(b"\x1b[<32;10;23M", b"1-1 / 6")
        assert_panel_only(output, 19, 36)
        assert b"\x1b[23;1H" in output and b"RESIZING" in output
        event_output(b"\x1b[<0;10;23m", b"FOLLOW ON")
        # No held left button means a stray motion must not change the height.
        assert event_output(b"\x1b[<32;10;29M") == b""
        event_output(b"\x1b[<0;10;23M", b"RESIZING")
        output = event_output(b"\x1b[<32;10;29M", b"1-6 / 6")
        assert_panel_only(output, 19, 36)
        assert b"\x1b[29;1H" in output and b"RESIZING" in output
        event_output(b"\x1b[<0;10;29m", b"FOLLOW ON")
        # Mouse routing follows the moved divider, not its original row25.
        output = event_output(b"\x1b[<64;10;32M", b"SCROLL")
        assert_panel_only(output,29,36)
        event_output(b"f", b"FOLLOW ON")
        # Restore the original split before exercising existing keyboard keys.
        event_output(b"\x1b[<0;10;29M", b"RESIZING")
        event_output(b"\x1b[<32;10;25M", b"1-3 / 6")
        event_output(b"\x1b[<0;10;25m", b"FOLLOW ON")
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
            read_chunk(0.1)
        assert process.poll() == 0, "Monitor did not quit cleanly"
        drain()
        assert all(f"\x1b[?{mode}l".encode() in captured for mode in (1000,1002,1006)), "Mouse capture was not disabled"
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
    print("PASS: snapshot, invalid arguments, non-TTY, FIFO rejection, pause, scoped mouse wheel, divider drag, help, keyboard scroll/follow, resize, q/Ctrl-C/SIGTERM, terminal/mouse restoration")


if __name__ == "__main__":
    main()
