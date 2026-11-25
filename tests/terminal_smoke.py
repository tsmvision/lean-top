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
import unicodedata


def screen_text(data):
    """Interpret cursor-addressed output so assertions also support partial writes."""
    rows = [[" "] * 200 for _ in range(100)]
    x = y = 0
    for token in re.findall(r"\x1b\[[0-?]*[ -/]*[@-~]|[^\x1b]+", data.decode("utf-8", "replace")):
        if token.startswith("\x1b["):
            args, command = token[2:-1], token[-1]
            if command == "H":
                values = [int(v or 1) for v in args.split(";")]
                y, x = values[0] - 1, (values[1] if len(values) > 1 else 1) - 1
            elif command == "J" and args == "2":
                rows = [[" "] * 200 for _ in range(100)]
            elif command == "K" and args == "2" and y < 100:
                rows[y] = [" "] * 200
            continue
        for char in token:
            if char == "\r":
                x = 0
            elif char == "\n":
                y += 1
            elif ord(char) >= 32 and not unicodedata.combining(char):
                if y < 100 and x < 200:
                    rows[y][x] = char
                    width = 2 if unicodedata.east_asian_width(char) in "WF" else 1
                    if width == 2 and x + 1 < 200:
                        rows[y][x + 1] = ""
                    x += width
    return "\n".join("".join(row) for row in rows)


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
                if needle in output or needle.decode("utf-8") in screen_text(captured):
                    return bytes(output)
        raise AssertionError(f"Did not see {needle!r}: {output[-2000:]!r}")

    def event_output(action, needle=None):
        drain()
        os.write(master, action)
        output = expect(needle) if needle is not None else b""
        return output + drain()

    def assert_panel_only(output, first_row, last_row):
        rows = [int(row) for row in re.findall(rb"\x1b\[(\d+);\d+H", output)]
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
        assert b"1-6 / 6" in captured, "All demo processes should fit by default"
        assert b"TYPE     MEMORY       TARGET" in captured
        assert "▆".encode() not in captured, "CPU history graph was removed"

        # Details expose full paths and PID without crowding normal rows.
        output = event_output(b"\x1b[<0;10;21M",b"PROCESS DETAILS")
        assert b"28416" in output and b"TARGET PATH" in output and b"COMMAND" in output
        event_output(b"\x1b[<0;10;21m")
        event_output(b"\x1b",b"PROCESSES")
        event_output(b"p",b"PROCESS DETAILS")
        event_output(b"\r")
        output = event_output(b"\x1b[<0;10;22M",b"PROCESS DETAILS")
        assert b"28421" in output
        event_output(b"\x1b[<0;10;22m")
        event_output(b"\x1b[<0;2;2M")

        # Explicitly shrink the auto-sized process panel to exercise scrolling.
        event_output(b"\x1b[<0;10;28M")
        event_output(b"\x1b[<32;10;25M",b"1-3 / 6")
        event_output(b"\x1b[<0;10;25m")

        # Mouse coordinates are physical terminal coordinates (one-based).
        # Process scrolling must not change the log viewport or follow mode.
        output = event_output(b"\x1b[<65;10;22M", b"2-4 / 6")
        assert_panel_only(output, 19, 24)
        output = event_output(b"\x1b[<64;10;22M", b"1-3 / 6")
        assert_panel_only(output, 19, 24)

        # Scroll older/newer log records without repainting process rows.
        output = event_output(b"\x1b[<64;10;28M")
        assert_panel_only(output, 25, 36)
        output = event_output(b"\x1b[<65;10;28M")
        assert_panel_only(output, 25, 36)
        event_output(b"f")

        assert event_output(b"\x1b[<65;5;7M") == b"", "Wheel outside panels must be ignored"
        assert event_output(b"\x1b[<0;10;20M") == b"", "Clicking a table header must not scroll"
        assert event_output(b"\x1b[<65;10;22m") == b"", "Mouse release must not scroll"

        event_output(b"?", b"KEYBOARD")
        assert event_output(b"\x1b[<65;10;22M") == b"", "Help must block underlying mouse scrolling"
        event_output(b"?")

        # The BUILD divider shares space between resources and the build panel.
        event_output(b"\x1b[<0;10;12M")
        output = event_output(b"\x1b[<32;10;10M", b"BUILD")
        assert_panel_only(output,5,18)
        assert re.search(rb"\x1b\[10;\d+H", output)
        event_output(b"\x1b[<0;10;10m")
        event_output(b"\x1b[<0;10;10M")
        assert_panel_only(event_output(b"\x1b[<32;10;12M", b"BUILD"),5,18)
        event_output(b"\x1b[<0;10;12m")

        # The PROCESSES divider shares space with Build, leaving logs untouched.
        event_output(b"\x1b[<0;10;19M")
        output = event_output(b"\x1b[<32;10;17M", b"1-5 / 6")
        assert_panel_only(output,12,24)
        assert re.search(rb"\x1b\[17;\d+H", output)
        event_output(b"\x1b[<0;10;17m")
        assert_panel_only(event_output(b"\x1b[<65;10;20M",b"2-6 / 6"),17,24)
        event_output(b"\x1b[<64;10;20M",b"1-5 / 6")
        event_output(b"\x1b[<0;10;17M")
        assert_panel_only(event_output(b"\x1b[<32;10;19M",b"1-3 / 6"),12,24)
        event_output(b"\x1b[<0;10;19m")

        # Drag the log header (row25) upward to row23, then downward to row29.
        event_output(b"\x1b[<0;10;25M")
        output = event_output(b"\x1b[<32;10;23M", b"1-1 / 6")
        assert_panel_only(output, 19, 36)
        assert re.search(rb"\x1b\[23;\d+H", output) and "BUILD LOG" in screen_text(captured)
        event_output(b"\x1b[<0;10;23m")
        # No held left button means a stray motion must not change the height.
        assert event_output(b"\x1b[<32;10;29M") == b""
        event_output(b"\x1b[<0;10;23M")
        output = event_output(b"\x1b[<32;10;29M", b"1-6 / 6")
        assert_panel_only(output, 19, 36)
        assert re.search(rb"\x1b\[29;\d+H", output) and "BUILD LOG" in screen_text(captured)
        event_output(b"\x1b[<0;10;29m")
        # Mouse routing follows the moved divider, not its original row25.
        output = event_output(b"\x1b[<64;10;32M")
        assert_panel_only(output,29,36)
        event_output(b"f")
        # Restore the original split before exercising existing keyboard keys.
        event_output(b"\x1b[<0;10;29M")
        event_output(b"\x1b[<32;10;25M", b"1-3 / 6")
        event_output(b"\x1b[<0;10;25m")
        assert_panel_only(event_output(b"\x1b[A"),25,36)
        event_output(b"f")
        expect(b"DEMO", b" ")
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        expect(b"LEAN TOP")
        # Compact mode has Resources/Build/Log and both remaining dividers work.
        event_output(b" ",b"PAUSED")
        event_output(b"\x1b[<0;10;12M")
        assert_panel_only(event_output(b"\x1b[<32;10;10M",b"BUILD"),5,18)
        event_output(b"\x1b[<0;10;10m")
        event_output(b"\x1b[<0;10;19M")
        output = event_output(b"\x1b[<32;10;17M",b"BUILD LOG")
        assert_panel_only(output,10,22)
        assert re.search(rb"\x1b\[17;\d+H", output)
        event_output(b"\x1b[<0;10;17m")
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
    assert b"34 / 120" in plain
    assert b"Lake jobs: 342/680" in plain
    assert b"Initial Lean files" not in plain and b"Est. time" in plain
    bad = subprocess.run([binary, "--interval-ms", "99"], capture_output=True)
    assert bad.returncode != 0
    non_tty = subprocess.run([binary, "--demo"], capture_output=True)
    assert non_tty.returncode != 0 and b"requires a terminal" in non_tty.stderr
    with tempfile.TemporaryDirectory(prefix="lean-top-smoke-") as directory:
        logs = Path(directory) / "logs"
        logs.mkdir()
        (logs / "z.log").write_text("EXPLICIT_Z_LOG\n")
        (logs / "a.log").write_text("FIRST_A_LOG\n")
        isolated = dict(os.environ,HOME=directory)
        selected = subprocess.check_output([binary,"--project",directory,"--snapshot"],env=isolated,timeout=5)
        assert b"FIRST_A_LOG" in selected and b"EXPLICIT_Z_LOG" not in selected
        explicit = subprocess.check_output([binary,"--project",directory,"--log",str(logs / "z.log"),"--snapshot"],env=isolated,timeout=5)
        assert b"EXPLICIT_Z_LOG" in explicit and b"FIRST_A_LOG" not in explicit
        demo = subprocess.check_output([binary,"--demo","--snapshot"],env=isolated,timeout=5)
        assert b"FIRST_A_LOG" not in demo
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
        assert b"Lake jobs: 2/20" in observed, "Lake counters are reference-only metadata"
        assert b"2 / 20 files" not in observed
    for key in (b"q", b"\x03", None):
        exercise(binary, key)
    exercise_wrapping(binary)
    exercise_estimates(binary)
    exercise_estimate_restart(binary)
    exercise_log_stability(binary)
    exercise_log_history(binary)
    if sys.platform.startswith("linux"):
        live = subprocess.check_output([binary, "--snapshot"])
        assert b"require Linux /proc" not in live
        assert b"Disk capacity unavailable" not in live
        assert b"Sampling" not in live
    print("PASS: snapshot, invalid arguments, non-TTY, FIFO rejection, pause, process details, auto process height, wrapped logs, file-backed scrollback, restored compilation counts after restart, scoped mouse wheel, all section dividers, help, keyboard scroll/follow, resize, q/Ctrl-C/SIGTERM, terminal/mouse restoration")


def exercise_wrapping(binary):
    with tempfile.TemporaryDirectory(prefix="lean-top-wrap-") as directory:
        log = Path(directory) / "build.log"
        log.write_text("START_" + "x" * (48 * 10 - 6) + "END_MARKER\n")
        master,slave = os.openpty()
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",38,111,0,0))
        process = subprocess.Popen([binary,"--log",str(log),"--project",directory],stdin=slave,stdout=slave,stderr=slave)
        def output_until_marker():
            data = bytearray()
            deadline = time.monotonic()+5
            while time.monotonic()<deadline:
                if select.select([master],[],[],0.1)[0]:
                    data.extend(os.read(master,65536))
                    if b"END_MARKER" in data:
                        return bytes(data)
            raise AssertionError(f"Missing wrapped log tail: {data[-1000:]!r}")
        try:
            output_until_marker()
            fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",38,53,0,0))
            output = output_until_marker()
            assert b"START_" in output and b"x"*48 in output
            os.write(master,b"q")
            deadline = time.monotonic()+5
            while process.poll() is None and time.monotonic()<deadline:
                if select.select([master],[],[],0.1)[0]:
                    os.read(master,65536)
            assert process.poll()==0
        finally:
            if process.poll() is None:
                process.kill();process.wait()
            os.close(master);os.close(slave)


def exercise_estimates(binary):
    with tempfile.TemporaryDirectory(prefix="lean-top-estimate-") as directory:
        project = Path(directory)
        for module in "ABC":
            (project / f"{module}.lean").write_text(f"def {module.lower()} := 0\n")
        (project / "D.lean").write_text("def d := 0\n")
        artifacts = project / ".lake/build/lib/lean"
        artifacts.mkdir(parents=True)
        (artifacts / "D.olean").write_text("cached")
        log = project / "build.log"
        log.write_text("✔ [1/10] Built A (1s)\n",encoding="utf-8")
        original_paths = {path.relative_to(project) for path in project.rglob("*")}
        master,slave = os.openpty()
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",38,111,0,0))
        process = subprocess.Popen([binary,"--project",directory,"--log",str(log),"--interval-ms","100"],stdin=slave,stdout=slave,stderr=slave)
        captured = bytearray()
        def expect_all(needles):
            data = bytearray()
            deadline = time.monotonic()+5
            while time.monotonic()<deadline:
                if select.select([master],[],[],0.1)[0]:
                    chunk = os.read(master,65536)
                    data.extend(chunk)
                    captured.extend(chunk)
                    screen = screen_text(captured)
                    if all(needle.decode("utf-8") in screen for needle in needles):
                        return bytes(data)
            raise AssertionError(f"Missing estimate evidence {needles!r}: {data[-1500:]!r}")
        try:
            expect_all([b"1 / 3",b"3s total",b"2s left"])
            with log.open("a",encoding="utf-8") as output:
                output.write("✔ [4/10] Built B (1s)\n")
            expect_all([b"2 / 3"])
            with log.open("a",encoding="utf-8") as output:
                output.write("✔ [5/50000] Built D (1s)\n")
            expect_all([b"3 / 3",b"Built D"])
            screen = screen_text(captured)
            assert "extra" not in screen and "additional" not in screen
            os.write(master,b"q")
            deadline = time.monotonic()+5
            while process.poll() is None and time.monotonic()<deadline:
                if select.select([master],[],[],0.1)[0]:
                    os.read(master,65536)
            assert process.poll()==0
            assert {path.relative_to(project) for path in project.rglob("*")} == original_paths, "Estimation must not write artifacts"
        finally:
            if process.poll() is None:
                process.kill();process.wait()
            os.close(master);os.close(slave)


def exercise_log_stability(binary):
    with tempfile.TemporaryDirectory(prefix="lean-top-log-stable-") as directory:
        log = Path(directory) / "build.log"
        log.write_text("✔ [1/4] Built A\n⣾ [2/4] Running B\r", encoding="utf-8")
        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 38, 111, 0, 0))
        process = subprocess.Popen([binary, "--project", directory, "--log", str(log), "--interval-ms", "100"], stdin=slave, stdout=slave, stderr=slave)
        def collect(seconds):
            data = bytearray()
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.05)[0]:
                    data.extend(os.read(master, 65536))
            return bytes(data)
        try:
            initial = collect(1)
            rows = screen_text(initial).splitlines()
            header = next(i + 1 for i, row in enumerate(rows) if "BUILD LOG" in row)
            for chunk in ["", "⣷ [2/4] Running B\r", "⣯ [2/4] Running B", "\r\n"]:
                with log.open("a", encoding="utf-8") as output:
                    output.write(chunk)
                output = collect(0.4)
                written = [int(row) for row in re.findall(rb"\x1b\[(\d+);\d+H", output)]
                assert not any(header <= row <= 36 for row in written), (chunk, written, header)
                assert b"\x1b[2J" not in output and b"\x1b[2K" not in output
            with log.open("a", encoding="utf-8") as output:
                output.write("ACTUAL_NEW_BUILD_EVENT\n")
            output = collect(0.5)
            assert b"ACTUAL_NEW_BUILD_EVENT" in output
            assert b"\x1b[2J" not in output and b"\x1b[2K" not in output
        finally:
            os.write(master, b"q")
            process.wait(timeout=3)
            os.close(master)
            os.close(slave)


def exercise_log_history(binary):
    with tempfile.TemporaryDirectory(prefix="lean-top-scrollback-") as directory:
        log = Path(directory) / "build.log"
        log.write_text("".join(f"history record {i:05}\n" for i in range(3000)))
        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 38, 111, 0, 0))
        process = subprocess.Popen([binary, "--project", directory, "--log", str(log), "--interval-ms", "100"], stdin=slave, stdout=slave, stderr=slave)
        captured = bytearray()
        def collect(seconds):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.03)[0]:
                    captured.extend(os.read(master, 65536))
            return screen_text(captured).splitlines()
        try:
            rows = collect(1)
            header = next(i for i, row in enumerate(rows) if "BUILD LOG" in row)
            assert any("history record 02999" in row for row in rows[header:36])
            os.write(master, f"\x1b[<64;10;{header + 3}M".encode() * 80)
            rows = collect(1)
            records = [int(value) for value in re.findall(r"history record (\d+)", "\n".join(rows[header:36]))]
            assert records and min(records) < 2800, records
            before = rows[header:36]
            with log.open("a") as output:
                output.write("NEW_WHILE_READING_HISTORY\n")
            rows = collect(0.5)
            assert rows[header:36] == before, "New output moved the historical viewport"
            os.write(master, b"f")
            rows = collect(0.5)
            assert any("NEW_WHILE_READING_HISTORY" in row for row in rows[header:36])
        finally:
            os.write(master, b"q")
            deadline = time.monotonic() + 3
            while process.poll() is None and time.monotonic() < deadline:
                if select.select([master], [], [], 0.05)[0]:
                    os.read(master, 65536)
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)


def exercise_estimate_restart(binary):
    with tempfile.TemporaryDirectory(prefix="lean-top-restart-") as directory:
        project = Path(directory)
        artifacts = project / ".lake/build/lib/lean"
        artifacts.mkdir(parents=True)
        for i in range(320):
            (project / f"M{i:04}.lean").write_text("import Init\n")
            if i < 300:
                (artifacts / f"M{i:04}.olean").write_text("fresh")
        log = project / "build.log"
        log.write_text("".join(f"✔ [{i}/50000] Built M{i:04} {'retained history ' * 15}(1s)\n" for i in range(300)), encoding="utf-8")
        assert log.stat().st_size > 65536
        original_paths = {path.relative_to(project) for path in project.rglob("*")}
        for completed in (300, 300, 301):
            if completed == 301:
                with log.open("a", encoding="utf-8") as output:
                    output.write("✔ [301/50000] Built M0300 (1s)\n")
                (artifacts / "M0300.olean").write_text("fresh")
                original_paths.add(Path(".lake/build/lib/lean/M0300.olean"))
            master, slave = os.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH",38,111,0,0))
            process = subprocess.Popen([binary,"--project",directory,"--log",str(log),"--interval-ms","100"],stdin=slave,stdout=slave,stderr=slave)
            captured = bytearray()
            try:
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    if select.select([master], [], [], 0.05)[0]:
                        captured.extend(os.read(master, 65536))
                        if f"{completed} / 320" in screen_text(captured):
                            break
                else:
                    raise AssertionError(f"Restart did not restore {completed} completions: {screen_text(captured)}")
                os.write(master, b"q")
                deadline = time.monotonic() + 3
                while process.poll() is None and time.monotonic() < deadline:
                    if select.select([master], [], [], 0.05)[0]:
                        os.read(master, 65536)
                assert process.poll() == 0
                assert {path.relative_to(project) for path in project.rglob("*")} == original_paths
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                os.close(master)
                os.close(slave)


if __name__ == "__main__":
    main()
