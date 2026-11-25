use crate::input::MouseKind;
use crate::monitor::{clean, progress, LogTail, Snapshot};
use std::collections::VecDeque;
use std::fmt::Write;
use std::ops::Range;

/// The renderer and mouse hit testing share the exact same panel boundaries.
struct Layout {
    build_y: usize,
    processes: Option<Range<usize>>,
    log: Option<Range<usize>>,
}

impl Layout {
    fn new(width: usize, height: usize, log_height: Option<usize>) -> Self {
        if width < 52 || height < 20 {
            return Self {
                build_y: 0,
                processes: None,
                log: None,
            };
        }
        let full = width >= 90 && height >= 32;
        let build_y = if full { 11 } else { 7 };
        let minimum_y = build_y + if full { 11 } else { 6 };
        let default_y = build_y + if full { 13 } else { 6 };
        let log_y = log_height
            .map(|rows| height.saturating_sub(3).saturating_sub(rows))
            .unwrap_or(default_y)
            .clamp(minimum_y, height - 6);
        Self {
            build_y,
            processes: full.then_some(build_y + 7..log_y),
            log: Some(log_y..height - 2),
        }
    }

    fn process_rows(&self) -> usize {
        self.processes
            .as_ref()
            .map(|range| range.len().saturating_sub(3))
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Text,
    Muted,
    Cyan,
    Green,
    Yellow,
    Red,
}

impl Color {
    fn ansi(self) -> &'static str {
        match self {
            Self::Text => "\x1b[0;38;5;252m",
            Self::Muted => "\x1b[0;38;5;245m",
            Self::Cyan => "\x1b[1;38;5;80m",
            Self::Green => "\x1b[0;38;5;114m",
            Self::Yellow => "\x1b[0;38;5;221m",
            Self::Red => "\x1b[0;38;5;203m",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Cell {
    ch: char,
    color: Color,
    continuation: bool,
}

pub struct Frame {
    pub width: usize,
    pub height: usize,
    cells: Vec<Cell>,
}

// Terminal width for common CJK, combining marks, and the symbols used here.
// Complex emoji/ZWJ clusters are intentionally outside this lightweight renderer.
fn char_width(c: char) -> usize {
    match c as u32 {
        0x0300..=0x036f
        | 0x1ab0..=0x1aff
        | 0x1dc0..=0x1dff
        | 0x200b..=0x200f
        | 0x202a..=0x202e
        | 0x2060..=0x206f
        | 0xfe00..=0xfe0f
        | 0xfe20..=0xfe2f => 0,
        0x1100..=0x115f
        | 0x2329..=0x232a
        | 0x2e80..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe10..=0xfe19
        | 0xfe30..=0xfe6f
        | 0xff01..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1faff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

impl Frame {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            cells: vec![
                Cell {
                    ch: ' ',
                    ..Cell::default()
                };
                width * height
            ],
        }
    }

    fn text(&mut self, x: usize, y: usize, text: &str, color: Color, limit: usize) {
        if y >= self.height {
            return;
        }
        let end = (x + limit).min(self.width);
        let mut x = x;
        for ch in clean(text).chars() {
            let width = char_width(ch);
            if width == 0 {
                continue;
            }
            if x + width > end {
                break;
            }
            // A modal may cover either half of an existing CJK character.
            // Repair that pair first so the row retains its terminal width.
            for column in x..x + width {
                let index = y * self.width + column;
                let old = self.cells[index];
                if old.continuation && column > 0 {
                    self.cells[index - 1] = Cell {
                        ch: ' ',
                        ..Cell::default()
                    };
                } else if char_width(old.ch) == 2 && column + 1 < self.width {
                    self.cells[index + 1] = Cell {
                        ch: ' ',
                        ..Cell::default()
                    };
                }
                self.cells[index] = Cell {
                    ch: ' ',
                    ..Cell::default()
                };
            }
            self.cells[y * self.width + x] = Cell {
                ch,
                color,
                continuation: false,
            };
            if width == 2 {
                self.cells[y * self.width + x + 1] = Cell {
                    ch: ' ',
                    color,
                    continuation: true,
                };
            }
            x += width;
        }
    }

    fn rule(&mut self, y: usize, title: &str) {
        if self.width < 4 {
            return;
        }
        self.text(
            0,
            y,
            &format!("├{}┤", "─".repeat(self.width - 2)),
            Color::Muted,
            self.width,
        );
        if !title.is_empty() {
            self.text(
                2,
                y,
                &format!(" {title} "),
                Color::Cyan,
                self.width.saturating_sub(5),
            );
        }
    }

    fn bar(&mut self, x: usize, y: usize, width: usize, percent: f64, color: Color) {
        let count = (width as f64 * percent.clamp(0.0, 100.0) / 100.0).round() as usize;
        self.text(x, y, &"█".repeat(count), color, width);
        self.text(
            x + count,
            y,
            &"░".repeat(width - count),
            Color::Muted,
            width - count,
        );
    }

    pub fn plain(&self) -> String {
        let mut text = String::new();
        for row in self.cells.chunks(self.width.max(1)) {
            for cell in row {
                if !cell.continuation {
                    text.push(cell.ch);
                }
            }
            text.push('\n');
        }
        text
    }

    pub fn ansi_diff(&self, previous: Option<&Self>, color: bool) -> String {
        let mut output = String::new();
        let same_size = previous
            .map(|p| p.width == self.width && p.height == self.height)
            .unwrap_or(false);
        if !same_size {
            output.push_str("\x1b[2J");
        }
        for y in 0..self.height {
            let range = y * self.width..(y + 1) * self.width;
            if same_size && previous.unwrap().cells[range.clone()] == self.cells[range.clone()] {
                continue;
            }
            let _ = write!(output, "\x1b[{};1H\x1b[0m\x1b[2K", y + 1);
            let mut last = None;
            for cell in &self.cells[range] {
                if cell.continuation {
                    continue;
                }
                if color && last != Some(cell.color) {
                    output.push_str(cell.color.ansi());
                    last = Some(cell.color);
                }
                output.push(cell.ch);
            }
        }
        if !output.is_empty() {
            output.push_str("\x1b[0m");
        }
        output
    }
}

pub fn bytes(value: u64) -> String {
    let value = value as f64;
    if value >= 1024.0_f64.powi(3) {
        format!("{:.1} GiB", value / 1024.0_f64.powi(3))
    } else {
        format!("{:.0} MiB", value / 1024.0_f64.powi(2))
    }
}

fn ratio(value: Option<(u64, u64)>) -> f64 {
    value
        .filter(|(_, total)| *total > 0)
        .map(|(used, total)| used as f64 / total as f64 * 100.0)
        .unwrap_or(0.0)
}

fn usage(value: Option<(u64, u64)>) -> String {
    value
        .map(|(used, total)| format!("{} / {}", bytes(used), bytes(total)))
        .unwrap_or_else(|| "Unavailable".into())
}

fn pressure(percent: f64) -> Color {
    if percent >= 90.0 {
        Color::Red
    } else if percent >= 75.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

#[derive(Default)]
pub struct ViewState {
    pub paused: bool,
    pub follow: bool,
    pub offset: usize,
    pub process_offset: usize,
    pub log_height: Option<usize>,
    pub dragging_log: bool,
    pub help: bool,
    pub history: VecDeque<f64>,
    pub demo: bool,
    pub tick: u64,
}

impl ViewState {
    pub fn push_cpu(&mut self, value: Option<f64>) {
        if let Some(value) = value {
            if self.history.len() >= 32 {
                self.history.pop_front();
            }
            self.history.push_back(value);
        }
    }

    pub fn scroll(&mut self, up: bool, count: usize, log: &LogTail) {
        self.follow = false;
        let max = log.visible_lines().len().saturating_sub(1);
        self.offset = if up {
            self.offset.saturating_add(count).min(max)
        } else {
            self.offset.saturating_sub(count)
        };
    }

    pub fn clamp_processes(&mut self, count: usize, size: (usize, usize)) {
        let rows = Layout::new(size.0, size.1, self.log_height).process_rows();
        self.process_offset = self.process_offset.min(count.saturating_sub(rows.max(1)));
    }

    pub fn mouse(
        &mut self,
        kind: MouseKind,
        column: usize,
        row: usize,
        size: (usize, usize),
        snapshot: &Snapshot,
    ) -> bool {
        let (width, height) = size;
        let layout = Layout::new(width, height, self.log_height);
        if self.help || layout.log.is_none() {
            return std::mem::replace(&mut self.dragging_log, false);
        }
        match kind {
            MouseKind::Press => {
                let active =
                    column > 0 && column < width - 1 && row == layout.log.as_ref().unwrap().start;
                let previous = std::mem::replace(&mut self.dragging_log, active);
                previous != active
            }
            MouseKind::Drag if self.dragging_log => {
                let minimum_y = layout.build_y + if layout.processes.is_some() { 11 } else { 6 };
                let divider = row.clamp(minimum_y, height - 6);
                let requested = Some(height - 3 - divider);
                let changed = self.log_height != requested;
                self.log_height = requested;
                self.clamp_processes(snapshot.processes.len(), size);
                changed
            }
            MouseKind::Release => std::mem::replace(&mut self.dragging_log, false),
            _ => false,
        }
    }

    pub fn wheel(
        &mut self,
        up: bool,
        column: usize,
        row: usize,
        size: (usize, usize),
        snapshot: &Snapshot,
        log: &LogTail,
    ) -> bool {
        let (width, height) = size;
        if self.help || self.dragging_log || column == 0 || column >= width.saturating_sub(1) {
            return false;
        }
        let layout = Layout::new(width, height, self.log_height);
        if layout
            .processes
            .as_ref()
            .is_some_and(|range| range.contains(&row))
        {
            let previous = self.process_offset;
            self.clamp_processes(snapshot.processes.len(), size);
            self.process_offset = if up {
                self.process_offset.saturating_sub(1)
            } else {
                self.process_offset.saturating_add(1).min(
                    snapshot
                        .processes
                        .len()
                        .saturating_sub(layout.process_rows()),
                )
            };
            self.process_offset != previous
        } else if layout
            .log
            .as_ref()
            .is_some_and(|range| range.contains(&row))
        {
            let previous = (self.follow, self.offset);
            self.scroll(up, 3, log);
            previous != (self.follow, self.offset)
        } else {
            false
        }
    }
}

pub struct View<'a> {
    pub snapshot: &'a Snapshot,
    pub log: &'a LogTail,
    pub state: &'a ViewState,
    pub project: &'a str,
    pub host: &'a str,
    pub cores: usize,
    pub interval_ms: u64,
}

pub fn render(view: &View<'_>, width: usize, height: usize) -> Frame {
    let mut frame = Frame::new(width, height);
    let layout = Layout::new(width, height, view.state.log_height);
    if layout.log.is_none() {
        frame.text(1, 1, "LEAN TOP", Color::Cyan, width.saturating_sub(2));
        frame.text(
            1,
            3,
            "Resize to at least 52 columns x 20 rows.",
            Color::Yellow,
            width.saturating_sub(2),
        );
        frame.text(
            1,
            5,
            "q Quit   Space Pause   ? Help",
            Color::Muted,
            width.saturating_sub(2),
        );
        return frame;
    }
    let w = width;
    let h = height;
    let inside = w - 4;
    let s = view.snapshot;
    let state = view.state;
    let log = view.log;
    frame.text(0, 0, &format!("╭{}╮", "─".repeat(w - 2)), Color::Muted, w);
    for y in 1..h - 2 {
        frame.text(0, y, "│", Color::Muted, 1);
        frame.text(w - 1, y, "│", Color::Muted, 1);
    }
    frame.text(2, 1, "LEAN TOP", Color::Cyan, 9);
    let mode = if state.paused {
        "PAUSED"
    } else if state.demo {
        "DEMO"
    } else {
        "LIVE"
    };
    let badge = format!("● {mode}  {:.1}s", view.interval_ms as f64 / 1000.0);
    frame.text(
        w.saturating_sub(23),
        1,
        &badge,
        if state.paused {
            Color::Yellow
        } else {
            Color::Green
        },
        21,
    );
    if w >= 90 {
        frame.text(15, 1, "Lean build monitor", Color::Text, w - 42);
    }
    let location = format!(
        "{}   ·   {} / {} cores",
        view.project, view.host, view.cores
    );
    frame.text(2, 2, &location, Color::Muted, inside);
    frame.rule(3, "");

    let full = layout.processes.is_some();
    if full {
        let col = inside / 3;
        let values = [
            s.cpu
                .map(|p| format!("{p:.1}%"))
                .unwrap_or_else(|| "Sampling…".into()),
            usage(s.memory),
            usage(s.disk.map(|(used, total, _)| (used, total))),
        ];
        let percentages = [
            s.cpu.unwrap_or(0.0),
            ratio(s.memory),
            ratio(s.disk.map(|(used, total, _)| (used, total))),
        ];
        for (index, title) in ["CPU", "MEMORY", "STORAGE"].iter().enumerate() {
            let x = 2 + col * index;
            frame.text(x, 5, title, Color::Muted, col - 2);
            frame.text(x, 6, &values[index], Color::Text, col - 2);
            frame.bar(
                x,
                7,
                col - 3,
                percentages[index],
                pressure(percentages[index]),
            );
        }
        let symbols: Vec<_> = "▁▂▃▄▅▆▇█".chars().collect();
        let spark: String = state
            .history
            .iter()
            .rev()
            .take(col - 3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|p| symbols[((p.clamp(0.0, 100.0) / 100.0 * 7.0).round() as usize).min(7)])
            .collect();
        frame.text(2, 8, &spark, Color::Cyan, col - 2);
        frame.text(2, 9, &format!("Load {}", s.load), Color::Muted, col - 2);
        let available = s
            .memory
            .map(|(used, total)| format!("Available {}", bytes(total.saturating_sub(used))))
            .unwrap_or_default();
        frame.text(2 + col, 8, &available, Color::Muted, col - 2);
        frame.text(
            2 + col,
            9,
            &format!("Swap {}", usage(s.swap)),
            Color::Muted,
            col - 2,
        );
        let free = s
            .disk
            .map(|(_, _, available)| format!("Free {}", bytes(available)))
            .unwrap_or_default();
        frame.text(2 + col * 2, 8, &free, Color::Muted, col - 2);
    } else {
        for (index, (title, value, percent)) in [
            (
                "CPU",
                s.cpu
                    .map(|p| format!("{p:.1}%"))
                    .unwrap_or_else(|| "Sampling…".into()),
                s.cpu.unwrap_or(0.0),
            ),
            ("MEMORY", usage(s.memory), ratio(s.memory)),
            (
                "STORAGE",
                usage(s.disk.map(|(a, b, _)| (a, b))),
                ratio(s.disk.map(|(a, b, _)| (a, b))),
            ),
        ]
        .iter()
        .enumerate()
        {
            let y = 4 + index;
            frame.text(2, y, title, Color::Muted, 8);
            frame.text(11, y, value, Color::Text, 26);
            frame.bar(38, y, w - 41, *percent, pressure(*percent));
        }
    }
    let b = layout.build_y;
    frame.rule(b, "BUILD");
    let active = !s.processes.is_empty();
    let status = if active { "● ACTIVE" } else { "○ IDLE" };
    frame.text(
        2,
        b + 1,
        status,
        if active { Color::Green } else { Color::Muted },
        10,
    );
    frame.text(
        14,
        b + 1,
        &format!("{} Lean/Lake processes · server-wide", s.processes.len()),
        Color::Text,
        w - 17,
    );
    if let Some((reported, total)) = log.current_progress() {
        let label = format!("{reported} / {total} jobs");
        let bar_width = inside.saturating_sub(label.len() + 3);
        frame.bar(
            2,
            b + 3,
            bar_width,
            reported as f64 / total as f64 * 100.0,
            Color::Cyan,
        );
        let x = if bar_width == 0 { 2 } else { 4 + bar_width };
        frame.text(x, b + 3, &label, Color::Text, (w - 2).saturating_sub(x));
    } else {
        frame.text(
            2,
            b + 3,
            "Lake jobs unknown · waiting for build status output",
            Color::Muted,
            inside,
        );
    }
    let lines = log.visible_lines();
    let latest = lines.iter().rev().find(|line| progress(line).is_some());
    frame.text(
        2,
        b + 4,
        &format!(
            "Latest event   {}",
            latest.map(|s| s.as_str()).unwrap_or("—")
        ),
        Color::Text,
        inside,
    );
    let source = log
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| {
            if state.demo {
                "simulated build.log".into()
            } else {
                "none · pass --log /path/to/build.log".into()
            }
        });
    let observed = log
        .updated
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|time| {
            let seconds = time.as_secs() % 86400;
            format!(
                "{:02}:{:02}:{:02} UTC",
                seconds / 3600,
                (seconds / 60) % 60,
                seconds % 60
            )
        })
        .unwrap_or_else(|| "waiting".into());
    frame.text(
        2,
        b + 5,
        &format!("Log: {source} · last read {observed}"),
        Color::Muted,
        inside,
    );
    let log_y = layout.log.as_ref().unwrap().start;
    if let Some(process_region) = &layout.processes {
        let start = state
            .process_offset
            .min(s.processes.len().saturating_sub(layout.process_rows()));
        let end = (start + layout.process_rows()).min(s.processes.len());
        let range = if s.processes.is_empty() {
            "0 / 0".into()
        } else {
            format!("{}-{} / {}", start + 1, end, s.processes.len())
        };
        frame.rule(process_region.start, &format!("PROCESSES · {range}"));
        frame.text(
            2,
            b + 8,
            "PID      NAME      MEMORY       COMMAND",
            Color::Muted,
            inside,
        );
        for (index, process) in s.processes[start..end].iter().enumerate() {
            let row = format!(
                "{:<8} {:<9} {:<12} {}",
                process.pid,
                process.name,
                bytes(process.rss),
                process.command
            );
            frame.text(2, b + 9 + index, &row, Color::Text, inside);
        }
        if s.processes.is_empty() {
            frame.text(
                2,
                b + 9,
                "No Lean/Lake processes detected. Waiting for a build…",
                Color::Muted,
                inside,
            );
        }
    }
    let title = if state.dragging_log {
        "BUILD LOG · RESIZING"
    } else if state.follow {
        "BUILD LOG · FOLLOW ON · drag to resize"
    } else {
        "BUILD LOG · SCROLL · drag to resize"
    };
    frame.rule(log_y, title);
    let available = (h - 2).saturating_sub(log_y + 1);
    let mut messages = Vec::new();
    if let Some(error) = &log.error {
        messages.push(format!("Log unavailable: {error} (retrying)"));
    }
    messages.extend(s.warnings.iter().cloned());
    if messages.is_empty() && lines.is_empty() {
        messages.push("Waiting for build output… Start lake build in another terminal.".into());
    }
    let warning_rows = messages.len().min(available);
    for (index, message) in messages.iter().take(warning_rows).enumerate() {
        frame.text(2, log_y + 1 + index, message, Color::Yellow, inside);
    }
    let log_rows = available - warning_rows;
    let end = if state.follow {
        lines.len()
    } else {
        lines
            .len()
            .saturating_sub(state.offset.min(lines.len().saturating_sub(1)))
    };
    let start = end.saturating_sub(log_rows);
    for (index, line) in lines[start..end].iter().enumerate() {
        let lower = line.to_lowercase();
        let color = if lower.contains("error:") || lower.contains("failed") || line.contains('✖')
        {
            Color::Red
        } else if lower.contains("warning:") || line.contains('⚠') {
            Color::Yellow
        } else if line.contains("Built ") || line.contains('✔') {
            Color::Green
        } else {
            Color::Text
        };
        frame.text(2, log_y + 1 + warning_rows + index, line, color, inside);
    }
    frame.text(
        0,
        h - 2,
        &format!("╰{}╯", "─".repeat(w - 2)),
        Color::Muted,
        w,
    );
    frame.text(
        1,
        h - 1,
        if w < 60 {
            "q Quit  Space Pause  ↑↓/jk Logs  f Follow  ? Help"
        } else {
            "q Quit   Space Pause   Wheel Scroll   ↑↓/jk Logs   f Follow   ? Help"
        },
        Color::Muted,
        w - 2,
    );
    if state.help {
        let help = [
            "KEYBOARD",
            "Wheel        Scroll the panel under pointer",
            "Left drag    Resize the Build Log divider",
            "q / Ctrl-C   Quit monitor (build continues)",
            "Space        Pause sampling and log reading",
            "Up / k       Scroll toward older log lines",
            "Down / j     Scroll toward newer log lines",
            "f            Resume following latest output",
            "?            Toggle this help",
            "",
            "Metrics and processes are server-wide.",
            "Storage is the project filesystem capacity.",
            "Lake job counts are not compiled-file counts.",
            "Compilation does not certify a theorem proof.",
        ];
        let box_width = 48.min(w - 4);
        let x = (w - box_width) / 2;
        let y = h.saturating_sub(help.len() + 2) / 2;
        for row in 0..help.len() + 2 {
            frame.text(x, y + row, &" ".repeat(box_width), Color::Text, box_width);
            if row > 0 && row <= help.len() {
                frame.text(
                    x + 1,
                    y + row,
                    help[row - 1],
                    if row == 1 { Color::Cyan } else { Color::Text },
                    box_width - 2,
                );
            }
        }
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divider_drag_resizes_logs_and_preserves_independent_scroll_state() {
        let snapshot = crate::monitor::demo(0);
        let mut state = ViewState {
            process_offset: 3,
            offset: 6,
            follow: false,
            ..ViewState::default()
        };
        assert!(!state.mouse(MouseKind::Drag, 9, 22, (110, 38), &snapshot));
        assert!(state.mouse(MouseKind::Press, 9, 24, (110, 38), &snapshot));
        assert!(state.dragging_log);
        assert!(state.mouse(MouseKind::Drag, 9, 22, (110, 38), &snapshot));
        let expanded = Layout::new(110, 38, state.log_height);
        assert_eq!(expanded.log.as_ref().unwrap().start, 22);
        assert_eq!(expanded.process_rows(), 1);
        assert_eq!(
            (state.process_offset, state.offset, state.follow),
            (3, 6, false)
        );
        assert!(state.mouse(MouseKind::Release, 9, 22, (110, 38), &snapshot));
        let height = state.log_height;
        assert!(!state.mouse(MouseKind::Drag, 9, 30, (110, 38), &snapshot));
        assert_eq!(state.log_height, height);
        assert!(state.mouse(MouseKind::Press, 9, 22, (110, 38), &snapshot));
        assert!(state.mouse(MouseKind::Drag, 9, usize::MAX, (110, 38), &snapshot));
        let shrunk = Layout::new(110, 38, state.log_height);
        assert_eq!(shrunk.log.as_ref().unwrap().len() - 1, 3);
        assert_eq!(
            state.process_offset, 0,
            "A larger process window must clamp its offset"
        );
        assert_eq!((state.offset, state.follow), (6, false));
    }

    #[test]
    fn divider_hit_testing_and_wheels_follow_the_resized_layout() {
        let snapshot = crate::monitor::demo(0);
        let mut log = LogTail::default();
        for index in 0..30 {
            log.push(format!("line {index}"));
        }
        let mut state = ViewState {
            follow: true,
            ..ViewState::default()
        };
        assert!(!state.mouse(MouseKind::Press, 9, 21, (110, 38), &snapshot));
        assert!(!state.mouse(MouseKind::Press, 0, 24, (110, 38), &snapshot));
        state.mouse(MouseKind::Press, 9, 24, (110, 38), &snapshot);
        state.mouse(MouseKind::Drag, 9, 30, (110, 38), &snapshot);
        assert!(!state.wheel(true, 9, 32, (110, 38), &snapshot, &log));
        state.mouse(MouseKind::Release, 9, 30, (110, 38), &snapshot);
        assert!(!state.wheel(false, 9, 26, (110, 38), &snapshot, &log));
        assert!(
            state.follow,
            "Former log coordinates now belong to the expanded process pane"
        );
        assert!(state.wheel(true, 9, 32, (110, 38), &snapshot, &log));
        assert!(!state.follow);
        state.help = true;
        assert!(!state.mouse(MouseKind::Press, 9, 30, (110, 38), &snapshot));
        assert!(!state.dragging_log);
    }

    #[test]
    fn resized_layouts_keep_minimum_rows_and_fit_terminal_bounds() {
        let snapshot = crate::monitor::demo(0);
        let log = LogTail::default();
        for (width, height) in [(40, 12), (52, 20), (79, 24), (90, 32), (110, 38), (110, 60)] {
            for requested in [Some(0), Some(3), Some(13), Some(usize::MAX)] {
                let state = ViewState {
                    log_height: requested,
                    ..ViewState::default()
                };
                let layout = Layout::new(width, height, requested);
                if let Some(range) = &layout.log {
                    assert!(range.len() >= 4);
                }
                if layout.processes.is_some() {
                    assert!(layout.process_rows() >= 1);
                }
                let frame = render(
                    &View {
                        snapshot: &snapshot,
                        log: &log,
                        state: &state,
                        project: "/srv/riemann",
                        host: "ubuntu",
                        cores: 16,
                        interval_ms: 500,
                    },
                    width,
                    height,
                );
                assert_eq!(frame.plain().lines().count(), height);
                assert!(frame.plain().lines().all(|line| line
                    .chars()
                    .map(char_width)
                    .sum::<usize>()
                    == width));
            }
        }
    }

    #[test]
    fn wheel_scrolls_only_the_panel_under_the_pointer() {
        let snapshot = crate::monitor::demo(0);
        let mut log = LogTail::default();
        for index in 0..30 {
            log.push(format!("log line {index}"));
        }
        let mut state = ViewState {
            follow: true,
            ..ViewState::default()
        };
        let frame = |state: &ViewState| {
            render(
                &View {
                    snapshot: &snapshot,
                    log: &log,
                    state,
                    project: "/srv/riemann",
                    host: "ubuntu",
                    cores: 16,
                    interval_ms: 500,
                },
                110,
                38,
            )
        };
        let before = frame(&state);
        assert!(state.wheel(false, 9, 21, (110, 38), &snapshot, &log));
        assert_eq!(
            (state.process_offset, state.offset, state.follow),
            (1, 0, true)
        );
        let processes_scrolled = frame(&state);
        assert_eq!(
            before.cells[..18 * 110],
            processes_scrolled.cells[..18 * 110]
        );
        assert_ne!(
            before.cells[18 * 110..24 * 110],
            processes_scrolled.cells[18 * 110..24 * 110]
        );
        assert_eq!(
            before.cells[24 * 110..],
            processes_scrolled.cells[24 * 110..]
        );
        assert!(processes_scrolled.plain().contains("2-4 / 6"));
        assert!(state.wheel(true, 9, 27, (110, 38), &snapshot, &log));
        assert_eq!(
            (state.process_offset, state.offset, state.follow),
            (1, 3, false)
        );
        let logs_scrolled = frame(&state);
        assert_eq!(
            processes_scrolled.cells[..24 * 110],
            logs_scrolled.cells[..24 * 110]
        );
        assert_ne!(
            processes_scrolled.cells[24 * 110..],
            logs_scrolled.cells[24 * 110..]
        );
        assert!(state.wheel(false, 9, 27, (110, 38), &snapshot, &log));
        assert_eq!((state.process_offset, state.offset), (1, 0));
        assert!(state.wheel(true, 9, 21, (110, 38), &snapshot, &log));
        assert_eq!((state.process_offset, state.offset), (0, 0));
    }

    #[test]
    fn wheel_ignores_borders_other_panels_help_and_tiny_windows() {
        let snapshot = crate::monitor::demo(0);
        let log = LogTail::default();
        let mut state = ViewState {
            follow: true,
            ..ViewState::default()
        };
        for (x, y, size) in [
            (9, 7, (110, 38)),
            (0, 21, (110, 38)),
            (109, 21, (110, 38)),
            (9, 36, (110, 38)),
            (9, 37, (110, 38)),
            (999, 21, (110, 38)),
            (9, 999, (110, 38)),
            (9, 21, (40, 12)),
            (9, 7, (79, 24)),
        ] {
            assert!(!state.wheel(false, x, y, size, &snapshot, &log));
            assert_eq!(
                (state.process_offset, state.offset, state.follow),
                (0, 0, true)
            );
        }
        // The compact layout routes the same physical row to its visible log,
        // never to the hidden process panel.
        assert!(state.wheel(false, 9, 21, (79, 24), &snapshot, &log));
        assert_eq!(state.process_offset, 0);
        assert!(!state.follow);
        state.follow = true;
        state.help = true;
        assert!(!state.wheel(false, 9, 21, (110, 38), &snapshot, &log));
        assert!(!state.wheel(false, 9, 27, (110, 38), &snapshot, &log));
        assert!(state.follow);
    }

    #[test]
    fn process_scroll_bounds_and_shrinking_lists_are_clamped() {
        let snapshot = crate::monitor::demo(0);
        let log = LogTail::default();
        let mut state = ViewState::default();
        for _ in 0..100 {
            state.wheel(false, 9, 21, (110, 38), &snapshot, &log);
        }
        assert_eq!(state.process_offset, 3);
        for _ in 0..100 {
            state.wheel(true, 9, 21, (110, 38), &snapshot, &log);
        }
        assert_eq!(state.process_offset, 0);
        state.process_offset = 100;
        state.clamp_processes(4, (110, 38));
        assert_eq!(state.process_offset, 1);
        for count in [3, 2, 1, 0] {
            state.clamp_processes(count, (110, 38));
            assert_eq!(state.process_offset, 0);
        }
    }

    #[test]
    fn full_layout_matches_approved_sections() {
        let snapshot = crate::monitor::demo(0);
        let state = ViewState {
            follow: true,
            demo: true,
            ..ViewState::default()
        };
        let log = LogTail::default();
        let frame = render(
            &View {
                snapshot: &snapshot,
                log: &log,
                state: &state,
                project: "/srv/riemann",
                host: "ubuntu",
                cores: 16,
                interval_ms: 500,
            },
            110,
            38,
        );
        let text = frame.plain();
        for label in [
            "LEAN TOP",
            "CPU",
            "MEMORY",
            "STORAGE",
            "BUILD",
            "PROCESSES",
            "BUILD LOG",
            "FOLLOW ON",
            "0.5s",
        ] {
            assert!(text.contains(label), "missing {label}");
        }
        assert_eq!(text.lines().count(), 38);
        assert!(text
            .lines()
            .all(|l| l.chars().map(char_width).sum::<usize>() == 110));
    }

    #[test]
    fn compact_and_tiny_layouts_do_not_overflow() {
        let snapshot = Snapshot::default();
        let state = ViewState::default();
        let log = LogTail::default();
        let view = View {
            snapshot: &snapshot,
            log: &log,
            state: &state,
            project: "한글/project",
            host: "host",
            cores: 1,
            interval_ms: 500,
        };
        for (w, h) in [(1, 1), (40, 12), (52, 20), (80, 24), (90, 32)] {
            let frame = render(&view, w, h);
            assert_eq!(frame.plain().lines().count(), h);
            assert!(frame
                .plain()
                .lines()
                .all(|l| l.chars().map(char_width).sum::<usize>() == w));
        }
    }

    #[test]
    fn unchanged_frame_emits_nothing() {
        let frame = Frame::new(80, 24);
        assert_eq!(frame.ansi_diff(Some(&frame), true), "");
        assert_eq!(frame.ansi_diff(Some(&frame), false), "");
    }

    #[test]
    fn unchanged_log_does_not_repaint_as_time_passes() {
        let snapshot = crate::monitor::demo(0);
        let state = ViewState {
            follow: true,
            ..ViewState::default()
        };
        let mut log = LogTail::default();
        log.updated = Some(std::time::SystemTime::now());
        log.push("✔ [1/2] Built Example".into());
        let view = View {
            snapshot: &snapshot,
            log: &log,
            state: &state,
            project: "/srv/riemann",
            host: "ubuntu",
            cores: 16,
            interval_ms: 500,
        };
        let before = render(&view, 110, 38);
        std::thread::sleep(std::time::Duration::from_millis(120));
        let after = render(&view, 110, 38);
        assert_eq!(after.ansi_diff(Some(&before), true), "");
        let changed_cpu = crate::monitor::demo(1);
        let refreshed = render(
            &View {
                snapshot: &changed_cpu,
                ..view
            },
            110,
            38,
        );
        assert!(!refreshed.ansi_diff(Some(&after), true).is_empty());
        assert_eq!(
            refreshed.cells[11 * 110..],
            after.cells[11 * 110..],
            "Resource refreshes must leave the unchanged build and log rows alone"
        );
    }

    #[test]
    fn untrusted_text_cannot_inject_terminal_controls() {
        let mut frame = Frame::new(40, 1);
        frame.text(0, 0, "hello\x1b[2J\x1b]0;evil\x07world", Color::Text, 40);
        assert!(frame.plain().starts_with("helloworld"));
        assert!(!frame.plain().contains('\x1b'));
    }

    #[test]
    fn overlays_repair_both_halves_of_wide_characters() {
        for x in 0..8 {
            let mut frame = Frame::new(8, 1);
            frame.text(0, 0, "검증검증", Color::Text, 8);
            frame.text(x, 0, "X", Color::Text, 1);
            assert_eq!(
                frame
                    .plain()
                    .trim_end_matches('\n')
                    .chars()
                    .map(char_width)
                    .sum::<usize>(),
                8
            );
        }
    }
}
