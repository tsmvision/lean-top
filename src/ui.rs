use crate::estimate::Estimate;
use crate::input::MouseKind;
use crate::monitor::{clean, progress, LogTail, Process, Snapshot};
use std::fmt::Write;
use std::ops::Range;
use std::path::Path;

/// The renderer and mouse hit testing share the exact same panel boundaries.
struct Layout {
    resources: Range<usize>,
    build: Range<usize>,
    processes: Option<Range<usize>>,
    log: Option<Range<usize>>,
}

impl Layout {
    fn new(
        width: usize,
        height: usize,
        requested: Option<[usize; 3]>,
        process_count: usize,
    ) -> Self {
        if width < 52 || height < 20 {
            return Self {
                resources: 0..0,
                build: 0..0,
                processes: None,
                log: None,
            };
        }
        let full = width >= 90 && height >= 32;
        let automatic = requested.is_none();
        let minimum = [3, 3, if full { 3 } else { 0 }];
        let mut requested = requested.unwrap_or(if full {
            [7, 7, process_count.saturating_add(3).max(3)]
        } else {
            [3, 6, 0]
        });
        if full && automatic {
            // Fit the automatically sized process list before optional summary
            // space; preserve the minimum fields in the resource/build panes.
            let mut excess = requested
                .iter()
                .fold(0usize, |sum, value| sum.saturating_add(*value))
                .saturating_sub(height - 10);
            for index in [1, 0] {
                let shrink = requested[index].saturating_sub(3).min(excess);
                requested[index] -= shrink;
                excess -= shrink;
            }
        }
        let mut heights = minimum;
        // Reserve the fixed header/footer and at least three log content rows.
        // Allocate from the top so preferences also fit smaller terminals.
        let mut remaining = height - 10 - minimum.iter().sum::<usize>();
        for index in 0..3 {
            if index == 2 && !full {
                continue;
            }
            let extra = requested[index]
                .saturating_sub(minimum[index])
                .min(remaining);
            heights[index] += extra;
            remaining -= extra;
        }
        let build_y = 4 + heights[0];
        let process_y = build_y + heights[1];
        let log_y = process_y + heights[2];
        Self {
            resources: 4..build_y,
            build: build_y..process_y,
            processes: full.then_some(process_y..log_y),
            log: Some(log_y..height - 2),
        }
    }

    fn process_rows(&self) -> usize {
        self.processes
            .as_ref()
            .map(|range| range.len().saturating_sub(3).max(1))
            .unwrap_or(0)
    }

    fn adjacent(&self, divider: usize) -> Option<(Range<usize>, Range<usize>)> {
        self.log.as_ref()?;
        match divider {
            0 => Some((self.resources.clone(), self.build.clone())),
            1 => Some((
                self.build.clone(),
                self.processes.as_ref().or(self.log.as_ref())?.clone(),
            )),
            2 => Some((self.processes.as_ref()?.clone(), self.log.as_ref()?.clone())),
            _ => None,
        }
    }

    fn divider_at(&self, row: usize) -> Option<usize> {
        (0..3).find(|index| {
            self.adjacent(*index)
                .is_some_and(|(_, lower)| lower.start == row)
        })
    }

    fn process_window(&self, count: usize, offset: usize) -> Range<usize> {
        let start = offset.min(count.saturating_sub(self.process_rows()));
        start..(start + self.process_rows()).min(count)
    }
}

struct Dialog {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

impl Dialog {
    fn new(width: usize, height: usize) -> Self {
        let dialog_width = width.saturating_sub(8).min(100);
        let dialog_height = height.saturating_sub(6).min(18);
        Self {
            x: (width - dialog_width) / 2,
            y: (height - dialog_height) / 2,
            width: dialog_width,
            height: dialog_height,
        }
    }

    fn contains(&self, column: usize, row: usize) -> bool {
        (self.x..self.x + self.width).contains(&column)
            && (self.y..self.y + self.height).contains(&row)
    }
}

fn target_name(process: &Process) -> &str {
    process
        .target
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("—")
}

fn detail_lines(process: &Process, live: bool, width: usize) -> Vec<String> {
    let fields = [
        format!("PID     {}", process.pid),
        format!("TYPE    {}", process.name),
        format!("MEMORY  {}", bytes(process.rss)),
        format!(
            "STATE   {}",
            if live {
                "Visible in latest sample"
            } else {
                "No longer visible; last captured details"
            }
        ),
        String::new(),
        "TARGET PATH".into(),
        process
            .target
            .clone()
            .unwrap_or_else(|| "Unavailable".into()),
        String::new(),
        "COMMAND".into(),
        if process.command.is_empty() {
            "Unavailable".into()
        } else {
            process.command.clone()
        },
    ];
    fields
        .iter()
        .flat_map(|field| wrap_text(field, width))
        .collect()
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for ch in clean(text).chars() {
        let size = char_width(ch);
        if size == 0 {
            continue;
        }
        if used + size > width.max(2) {
            rows.push(std::mem::take(&mut line));
            used = 0;
        }
        line.push(ch);
        used += size;
    }
    rows.push(line);
    rows
}

pub fn log_row_count(lines: &[String], terminal_width: usize) -> usize {
    let width = terminal_width.saturating_sub(4).max(2);
    lines
        .iter()
        .map(|line| {
            let (mut count, mut used) = (1, 0);
            for ch in clean(line).chars() {
                let size = char_width(ch);
                if used + size > width {
                    count += 1;
                    used = 0;
                }
                used += size;
            }
            count
        })
        .sum()
}

pub fn appended_log_rows(
    before: &[String],
    before_cursor: u64,
    after: &[String],
    after_cursor: u64,
    width: usize,
) -> usize {
    let added = usize::try_from(after_cursor.saturating_sub(before_cursor)).unwrap_or(usize::MAX);
    let removed = before
        .len()
        .saturating_add(added)
        .saturating_sub(after.len())
        .min(before.len());
    log_row_count(after, width)
        .saturating_add(log_row_count(&before[..removed], width))
        .saturating_sub(log_row_count(before, width))
}

fn log_color(line: &str) -> Color {
    let lower = line.to_lowercase();
    if lower.contains("error:") || lower.contains("failed") || line.contains('✖') {
        Color::Red
    } else if lower.contains("warning:") || line.contains('⚠') {
        Color::Yellow
    } else if line.contains("Built ") || line.contains('✔') {
        Color::Green
    } else {
        Color::Text
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

fn duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0).ceil() as u64;
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
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
    pub pane_heights: Option<[usize; 3]>,
    pub dragging_divider: Option<(usize, bool)>,
    pub process_detail: Option<Process>,
    pub detail_offset: usize,
    pub estimate: Estimate,
    pub help: bool,
    pub demo: bool,
    pub tick: u64,
}

impl ViewState {
    pub fn close_detail(&mut self) {
        self.process_detail = None;
        self.detail_offset = 0;
    }

    pub fn open_process(&mut self, snapshot: &Snapshot, size: (usize, usize)) -> bool {
        if self.help {
            return false;
        }
        let layout = Layout::new(size.0, size.1, self.pane_heights, snapshot.processes.len());
        if layout.processes.is_none() {
            return false;
        }
        let window = layout.process_window(snapshot.processes.len(), self.process_offset);
        if let Some(process) = snapshot.processes.get(window.start) {
            self.process_detail = Some(process.clone());
            self.detail_offset = 0;
            self.dragging_divider = None;
            true
        } else {
            false
        }
    }

    pub fn scroll_detail(
        &mut self,
        up: bool,
        count: usize,
        size: (usize, usize),
        snapshot: &Snapshot,
    ) -> bool {
        let Some(selected) = &self.process_detail else {
            return false;
        };
        let current = snapshot.processes.iter().find(|p| p.pid == selected.pid);
        let dialog = Dialog::new(size.0, size.1);
        let rows = dialog.height.saturating_sub(4);
        let lines = detail_lines(
            current.unwrap_or(selected),
            current.is_some(),
            dialog.width.saturating_sub(4),
        );
        let maximum = lines.len().saturating_sub(rows);
        let previous = self.detail_offset;
        self.detail_offset = self.detail_offset.min(maximum);
        self.detail_offset = if up {
            self.detail_offset.saturating_sub(count)
        } else {
            self.detail_offset.saturating_add(count).min(maximum)
        };
        previous != self.detail_offset
    }

    pub fn scroll(&mut self, up: bool, count: usize, log: &LogTail, width: usize) {
        self.follow = false;
        let max = log_row_count(&log.visible_lines(), width).saturating_sub(1);
        self.offset = if up {
            self.offset.saturating_add(count).min(max)
        } else {
            self.offset.saturating_sub(count)
        };
    }

    pub fn clamp_processes(&mut self, count: usize, size: (usize, usize)) {
        let rows = Layout::new(size.0, size.1, self.pane_heights, count).process_rows();
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
        let layout = Layout::new(width, height, self.pane_heights, snapshot.processes.len());
        if self.help || layout.log.is_none() {
            return self.dragging_divider.take().is_some();
        }
        if self.process_detail.is_some() {
            if kind == MouseKind::Press && !Dialog::new(width, height).contains(column, row) {
                self.close_detail();
                return true;
            }
            return false;
        }
        match kind {
            MouseKind::Press => {
                if column > 0 && column < width - 1 {
                    if let Some(region) = &layout.processes {
                        let window =
                            layout.process_window(snapshot.processes.len(), self.process_offset);
                        let first_row = region.start + 2;
                        if (first_row..first_row + window.len()).contains(&row) {
                            self.process_detail =
                                Some(snapshot.processes[window.start + row - first_row].clone());
                            self.detail_offset = 0;
                            self.dragging_divider = None;
                            return true;
                        }
                    }
                }
                let active = if column > 0 && column < width - 1 {
                    layout
                        .divider_at(row)
                        .map(|index| (index, layout.processes.is_some()))
                } else {
                    None
                };
                let previous = std::mem::replace(&mut self.dragging_divider, active);
                previous != active
            }
            MouseKind::Drag if self.dragging_divider.is_some() => {
                let (index, full) = self.dragging_divider.unwrap();
                if full != layout.processes.is_some() {
                    self.dragging_divider = None;
                    return true;
                }
                let (upper, lower) = layout.adjacent(index).unwrap();
                let lower_minimum = if lower.start == layout.log.as_ref().unwrap().start {
                    4
                } else {
                    3
                };
                let divider = row.clamp(upper.start + 3, lower.end - lower_minimum);
                let mut requested = [
                    layout.resources.len(),
                    layout.build.len(),
                    layout
                        .processes
                        .as_ref()
                        .map(Range::len)
                        .unwrap_or_else(|| self.pane_heights.map(|h| h[2]).unwrap_or(6)),
                ];
                requested[index] = divider - upper.start;
                if index + 1 < 3 && !(index == 1 && !full) {
                    requested[index + 1] = lower.end - divider;
                }
                let changed = self.pane_heights != Some(requested);
                self.pane_heights = Some(requested);
                self.clamp_processes(snapshot.processes.len(), size);
                changed
            }
            MouseKind::Release => self.dragging_divider.take().is_some(),
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
        if self.help
            || self.dragging_divider.is_some()
            || column == 0
            || column >= width.saturating_sub(1)
        {
            return false;
        }
        if self.process_detail.is_some() {
            return Dialog::new(width, height).contains(column, row)
                && self.scroll_detail(up, 3, size, snapshot);
        }
        let layout = Layout::new(width, height, self.pane_heights, snapshot.processes.len());
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
            self.scroll(up, 3, log, width);
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
    let layout = Layout::new(
        width,
        height,
        view.state.pane_heights,
        view.snapshot.processes.len(),
    );
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
        let resource_y = layout.resources.start + usize::from(layout.resources.len() >= 6);
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
            frame.text(x, resource_y, title, Color::Muted, col - 2);
            frame.text(x, resource_y + 1, &values[index], Color::Text, col - 2);
            frame.bar(
                x,
                resource_y + 2,
                col - 3,
                percentages[index],
                pressure(percentages[index]),
            );
        }
        if resource_y + 3 < layout.resources.end {
            frame.text(
                2,
                resource_y + 3,
                &format!("Load {}", s.load),
                Color::Muted,
                col - 2,
            );
        }
        let available = s
            .memory
            .map(|(used, total)| format!("Available {}", bytes(total.saturating_sub(used))))
            .unwrap_or_default();
        if resource_y + 3 < layout.resources.end {
            frame.text(2 + col, resource_y + 3, &available, Color::Muted, col - 2);
        }
        if resource_y + 4 < layout.resources.end {
            frame.text(
                2 + col,
                resource_y + 4,
                &format!("Swap {}", usage(s.swap)),
                Color::Muted,
                col - 2,
            );
        }
        let free = s
            .disk
            .map(|(_, _, available)| format!("Free {}", bytes(available)))
            .unwrap_or_default();
        if resource_y + 3 < layout.resources.end {
            frame.text(2 + col * 2, resource_y + 3, &free, Color::Muted, col - 2);
        }
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
            let y = layout.resources.start + index;
            frame.text(2, y, title, Color::Muted, 8);
            frame.text(11, y, value, Color::Text, 26);
            frame.bar(38, y, w - 41, *percent, pressure(*percent));
        }
        let available = s
            .memory
            .map(|(used, total)| bytes(total.saturating_sub(used)))
            .unwrap_or_else(|| "—".into());
        let free = s
            .disk
            .map(|(_, _, available)| bytes(available))
            .unwrap_or_else(|| "—".into());
        for (index, detail) in [
            format!("Load {}", s.load),
            format!("Swap {}", usage(s.swap)),
            format!("Memory available {available}"),
            format!("Storage free {free}"),
        ]
        .iter()
        .enumerate()
        {
            let y = layout.resources.start + 3 + index;
            if y < layout.resources.end {
                frame.text(2, y, detail, Color::Muted, inside);
            }
        }
    }
    let b = layout.build.start;
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
    let progress_y = b + 2 + usize::from(layout.build.len() >= 6);
    if layout.build.len() >= 6 {
        let files = state
            .estimate
            .files
            .map(|total| {
                let scope = if inside >= 85 && !state.estimate.scope.is_empty() {
                    format!(" · {}", state.estimate.scope)
                } else {
                    " · approx.".into()
                };
                format!(
                    "Initial Lean files ≈ {total} · done {} · additional {}{scope}",
                    state.estimate.completed, state.estimate.additional
                )
            })
            .unwrap_or_else(|| {
                if state.estimate.scope.to_lowercase().contains("scann") {
                    "Est. Lean files: scanning sources…".into()
                } else {
                    "Est. Lean files: unavailable".into()
                }
            });
        frame.text(2, b + 2, &files, Color::Muted, inside);
    }
    if let Some(total) = state.estimate.files {
        let completed = state.estimate.completed.min(total);
        let extra = if state.estimate.additional > 0 {
            format!(" +{} extra", state.estimate.additional)
        } else {
            String::new()
        };
        let label = format!("{completed} / {total} files (fixed){extra}");
        let bar_width = inside.saturating_sub(label.len() + 3);
        frame.bar(
            2,
            progress_y,
            bar_width,
            if total == 0 {
                0.0
            } else {
                completed as f64 / total as f64 * 100.0
            },
            Color::Cyan,
        );
        let x = if bar_width == 0 { 2 } else { 4 + bar_width };
        frame.text(
            x,
            progress_y,
            &label,
            Color::Text,
            (w - 2).saturating_sub(x),
        );
    } else {
        frame.text(
            2,
            progress_y,
            "File total unavailable · waiting for source analysis",
            Color::Muted,
            inside,
        );
    }
    let lines = log.visible_lines();
    let latest = lines.iter().rev().find(|line| progress(line).is_some());
    if progress_y + 1 < layout.build.end {
        let time = match (
            state.estimate.total_seconds,
            state.estimate.remaining_seconds,
        ) {
            (Some(total), Some(left)) => format!(
                "Est. time ≈ {} total · ≈ {} left",
                duration(total),
                duration(left)
            ),
            (_, Some(left)) => format!("Est. time ≈ {} remaining", duration(left)),
            _ if state.estimate.files.is_none()
                && state.estimate.scope.to_lowercase().contains("scann") =>
            {
                "Est. time: waiting for source analysis…".into()
            }
            _ if state.estimate.files.is_none() => "Est. time: unavailable".into(),
            _ => "Est. time: learning from completed modules…".into(),
        };
        frame.text(2, progress_y + 1, &time, Color::Text, inside);
    }
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
    if progress_y + 2 < layout.build.end {
        frame.text(
            2,
            progress_y + 2,
            &format!("Log: {source} · last read {observed}"),
            Color::Muted,
            inside,
        );
    }
    if progress_y + 3 < layout.build.end {
        frame.text(
            2,
            progress_y + 3,
            &format!(
                "Lake jobs: {} · {}",
                log.current_progress()
                    .map(|(done, total)| format!("{done}/{total}"))
                    .unwrap_or_else(|| "unknown".into()),
                latest.map(|s| s.as_str()).unwrap_or("—")
            ),
            Color::Muted,
            inside,
        );
    }
    let log_y = layout.log.as_ref().unwrap().start;
    if let Some(process_region) = &layout.processes {
        let window = layout.process_window(s.processes.len(), state.process_offset);
        let (start, end) = (window.start, window.end);
        let range = if s.processes.is_empty() {
            "0 / 0".into()
        } else {
            format!("{}-{} / {}", start + 1, end, s.processes.len())
        };
        frame.rule(process_region.start, &format!("PROCESSES · {range}"));
        frame.text(
            2,
            process_region.start + 1,
            "TYPE     MEMORY       TARGET",
            Color::Muted,
            inside,
        );
        for (index, process) in s.processes[start..end].iter().enumerate() {
            let row = format!(
                "{:<8} {:<12} {}",
                process.name,
                bytes(process.rss),
                target_name(process)
            );
            frame.text(
                2,
                process_region.start + 2 + index,
                &row,
                Color::Text,
                inside,
            );
        }
        if s.processes.is_empty() {
            frame.text(
                2,
                process_region.start + 2,
                "No Lean/Lake processes detected. Waiting for a build…",
                Color::Muted,
                inside,
            );
        }
    }
    frame.rule(log_y, "BUILD LOG");
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
    let wrapped: Vec<_> = lines
        .iter()
        .flat_map(|line| {
            let color = log_color(line);
            wrap_text(line, inside)
                .into_iter()
                .map(move |text| (text, color))
        })
        .collect();
    let end = if state.follow {
        wrapped.len()
    } else {
        wrapped
            .len()
            .saturating_sub(state.offset.min(wrapped.len().saturating_sub(1)))
    };
    let start = end.saturating_sub(log_rows);
    for (index, (line, color)) in wrapped[start..end].iter().enumerate() {
        frame.text(2, log_y + 1 + warning_rows + index, line, *color, inside);
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
        } else if full {
            "q Quit  Space Pause  Wheel Scroll  Click/p Details  ↑↓ Logs  f Follow  ? Help"
        } else {
            "q Quit   Space Pause   Wheel Scroll   ↑↓/jk Logs   f Follow   ? Help"
        },
        Color::Muted,
        w - 2,
    );
    if let Some(selected) = &state.process_detail {
        render_detail(&mut frame, selected, s, state.detail_offset);
    }
    if state.help {
        let help = [
            "KEYBOARD",
            "Wheel        Scroll the panel under pointer",
            "Left drag    Resize any section divider",
            "Click/p      Process details; Enter/Esc close",
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

fn render_detail(frame: &mut Frame, selected: &Process, snapshot: &Snapshot, offset: usize) {
    let dialog = Dialog::new(frame.width, frame.height);
    let current = snapshot.processes.iter().find(|p| p.pid == selected.pid);
    let lines = detail_lines(
        current.unwrap_or(selected),
        current.is_some(),
        dialog.width - 4,
    );
    let rows = dialog.height - 4;
    let start = offset.min(lines.len().saturating_sub(rows));
    let end = (start + rows).min(lines.len());
    for row in dialog.y..dialog.y + dialog.height {
        frame.text(
            dialog.x,
            row,
            &" ".repeat(dialog.width),
            Color::Text,
            dialog.width,
        );
        frame.text(dialog.x, row, "│", Color::Cyan, 1);
        frame.text(dialog.x + dialog.width - 1, row, "│", Color::Cyan, 1);
    }
    frame.text(
        dialog.x,
        dialog.y,
        &format!("╭{}╮", "─".repeat(dialog.width - 2)),
        Color::Cyan,
        dialog.width,
    );
    frame.text(
        dialog.x,
        dialog.y + dialog.height - 1,
        &format!("╰{}╯", "─".repeat(dialog.width - 2)),
        Color::Cyan,
        dialog.width,
    );
    frame.text(
        dialog.x + 2,
        dialog.y + 1,
        "PROCESS DETAILS",
        Color::Cyan,
        dialog.width - 4,
    );
    let page = format!("{}-{end} / {}", start + 1, lines.len());
    frame.text(
        dialog.x + dialog.width - page.len() - 2,
        dialog.y + 1,
        &page,
        Color::Muted,
        page.len(),
    );
    for (index, line) in lines[start..end].iter().enumerate() {
        frame.text(
            dialog.x + 2,
            dialog.y + 2 + index,
            line,
            Color::Text,
            dialog.width - 4,
        );
    }
    frame.text(
        dialog.x + 2,
        dialog.y + dialog.height - 2,
        "Wheel/↑↓ Scroll · Enter/Esc Close",
        Color::Muted,
        dialog.width - 4,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_process_pane_shows_all_rows_when_space_allows_and_keeps_log_minimum() {
        let snapshot = crate::monitor::demo(0);
        let layout = Layout::new(110, 38, None, snapshot.processes.len());
        assert_eq!(layout.process_rows(), snapshot.processes.len());
        assert_eq!(Layout::new(110, 38, None, 15).process_rows(), 15);
        assert_eq!(
            layout.process_window(snapshot.processes.len(), 0),
            0..snapshot.processes.len()
        );
        let crowded = Layout::new(110, 38, None, 200);
        assert!(crowded.process_rows() < 200);
        assert!(crowded.log.as_ref().unwrap().len() >= 4);
        let manual = Layout::new(110, 38, Some([7, 7, 4]), 6);
        assert_eq!(manual.process_rows(), 1);
    }

    #[test]
    fn build_estimates_are_distinct_from_reported_lake_job_counts() {
        let snapshot = crate::monitor::demo(0);
        let mut log = LogTail::default();
        log.progress = Some((342, 680));
        let state = ViewState {
            estimate: Estimate {
                files: Some(120),
                completed: 34,
                additional: 0,
                total_seconds: Some(180.0),
                remaining_seconds: Some(129.0),
                scope: "local estimate".into(),
            },
            ..ViewState::default()
        };
        let text = render(
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
        )
        .plain();
        assert!(text.contains("Initial Lean files ≈ 120 · done 34"));
        assert!(text.contains("34 / 120 files (fixed)"));
        assert!(text.contains("Lake jobs: 342/680"));
        assert!(text.contains("3m 0s total · ≈ 2m 9s left"));
        let before = render(
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
        log.progress = Some((600, 9000));
        let after = render(
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
        assert_eq!(
            before.cells[14 * 110..15 * 110],
            after.cells[14 * 110..15 * 110],
            "The main progress bar must not depend on Lake's job counts"
        );
    }

    #[test]
    fn concise_process_rows_hide_paths_and_pid_but_details_preserve_them() {
        let mut snapshot = crate::monitor::demo(0);
        snapshot.processes[0].target = Some("/opt/lean files/Explicit Formula.lean".into());
        let log = LogTail::default();
        let mut state = ViewState::default();
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
        assert!(text.contains("TYPE     MEMORY       TARGET"));
        assert!(text.contains("Explicit Formula.lean"));
        assert!(!text.contains("/opt/lean files/"));
        assert!(!text.contains("28416"));
        assert!(!text.chars().any(|c| ('▁'..='█').contains(&c) && c != '█'));
        assert!(state.mouse(MouseKind::Press, 9, 20, (110, 38), &snapshot));
        assert_eq!(state.process_detail.as_ref().unwrap().pid, 28416);
        let details = render(
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
        )
        .plain();
        assert!(details.contains("PROCESS DETAILS"));
        assert!(details.contains("28416"));
        assert!(details.contains("/opt/lean files/Explicit Formula.lean"));
        assert!(details.contains("COMMAND"));
    }

    #[test]
    fn details_remain_tied_to_pid_and_do_not_scroll_background_panels() {
        let mut snapshot = crate::monitor::demo(0);
        snapshot.processes[1].command = format!("lean {}", "long-command-argument ".repeat(100));
        let log = LogTail::default();
        let mut state = ViewState {
            pane_heights: Some([7, 7, 6]),
            process_offset: 1,
            offset: 6,
            follow: false,
            ..ViewState::default()
        };
        assert!(state.mouse(MouseKind::Press, 9, 20, (110, 38), &snapshot));
        assert_eq!(state.process_detail.as_ref().unwrap().pid, 28421);
        snapshot.processes.swap(0, 1);
        assert!(state.wheel(false, 10, 20, (110, 38), &snapshot, &log));
        assert!(state.detail_offset > 0);
        assert_eq!(
            (state.process_offset, state.offset, state.follow),
            (1, 6, false)
        );
        assert!(!state.mouse(MouseKind::Press, 10, 20, (110, 38), &snapshot));
        assert!(state.process_detail.is_some());
        snapshot.processes.clear();
        let lines = detail_lines(state.process_detail.as_ref().unwrap(), false, 96).join("\n");
        assert!(lines.contains("No longer visible"));
        assert!(state.mouse(MouseKind::Press, 1, 1, (110, 38), &snapshot));
        assert!(state.process_detail.is_none());
        assert_eq!(
            (state.process_offset, state.offset, state.follow),
            (1, 6, false)
        );
    }

    #[test]
    fn wrapped_logs_preserve_text_and_can_scroll_inside_a_single_long_record() {
        let text = format!("BEGIN {} 끝END", "검증abc ".repeat(100));
        let rows = wrap_text(&text, 48);
        assert_eq!(rows.join(""), text);
        assert!(rows
            .iter()
            .all(|line| line.chars().map(char_width).sum::<usize>() <= 48));
        let snapshot = crate::monitor::demo(0);
        let mut log = LogTail::default();
        log.push(text);
        assert_eq!(log_row_count(&log.visible_lines(), 52), rows.len());
        let mut state = ViewState {
            follow: true,
            ..ViewState::default()
        };
        let view = |state: &ViewState| {
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
                52,
                24,
            )
            .plain()
        };
        assert!(view(&state).contains("끝END"));
        state.scroll(true, usize::MAX, &log, 52);
        assert!(state.offset > 0);
        assert!(view(&state).contains("BEGIN"));
    }

    #[test]
    fn wrapped_scroll_anchor_accounts_for_ring_eviction_and_growing_partial_lines() {
        let before = vec!["abcdefghij".into(), "ok".into()];
        let after = vec!["ok".into(), "newtextlong".into()];
        assert_eq!(appended_log_rows(&before, 2, &after, 3, 10), 2);
        assert_eq!(
            appended_log_rows(&["abc".into()], 1, &["abcdefghij".into()], 1, 10),
            1
        );
    }

    #[test]
    fn every_full_layout_divider_resizes_only_its_adjacent_sections() {
        let snapshot = crate::monitor::demo(0);
        for (index, target) in [(0, 9), (1, 16), (2, 26)] {
            let mut state = ViewState {
                offset: 6,
                follow: false,
                ..ViewState::default()
            };
            let before = Layout::new(110, 38, None, snapshot.processes.len());
            let (upper, lower) = before.adjacent(index).unwrap();
            assert!(state.mouse(MouseKind::Press, 9, lower.start, (110, 38), &snapshot));
            assert!(state.mouse(MouseKind::Drag, 9, target, (110, 38), &snapshot));
            let after = Layout::new(110, 38, state.pane_heights, snapshot.processes.len());
            let (new_upper, new_lower) = after.adjacent(index).unwrap();
            assert_eq!(new_upper, upper.start..target);
            assert_eq!(new_lower, target..lower.end);
            assert_eq!((state.offset, state.follow), (6, false));
            if index != 0 {
                assert_eq!(before.resources, after.resources);
            }
            if index != 2 {
                assert_eq!(before.log, after.log);
            }
            assert!(state.mouse(MouseKind::Release, 9, target, (110, 38), &snapshot));
            let preferences = state.pane_heights;
            assert!(!state.mouse(MouseKind::Drag, 9, 0, (110, 38), &snapshot));
            assert_eq!(state.pane_heights, preferences);
        }
    }

    #[test]
    fn all_dividers_clamp_at_minimum_heights_in_full_and_compact_layouts() {
        let snapshot = crate::monitor::demo(0);
        for size in [(110, 38), (79, 24), (52, 20)] {
            let initial = Layout::new(size.0, size.1, None, snapshot.processes.len());
            for index in 0..3 {
                let Some((_, lower)) = initial.adjacent(index) else {
                    continue;
                };
                for row in [0, usize::MAX] {
                    let mut state = ViewState::default();
                    state.mouse(MouseKind::Press, 9, lower.start, size, &snapshot);
                    state.mouse(MouseKind::Drag, 9, row, size, &snapshot);
                    let layout =
                        Layout::new(size.0, size.1, state.pane_heights, snapshot.processes.len());
                    assert!(layout.resources.len() >= 3);
                    assert!(layout.build.len() >= 3);
                    if let Some(region) = &layout.processes {
                        assert!(region.len() >= 3);
                    }
                    assert!(layout.log.as_ref().unwrap().len() >= 4);
                    assert_eq!(layout.resources.end, layout.build.start);
                    assert_eq!(
                        layout.build.end,
                        layout
                            .processes
                            .as_ref()
                            .unwrap_or(layout.log.as_ref().unwrap())
                            .start
                    );
                    assert_eq!(layout.log.as_ref().unwrap().end, size.1 - 2);
                }
            }
        }
    }

    #[test]
    fn shrinking_resource_and_build_sections_does_not_draw_over_later_sections() {
        let snapshot = crate::monitor::demo(0);
        let log = LogTail::default();
        let state = ViewState {
            pane_heights: Some([3, 3, 3]),
            ..ViewState::default()
        };
        let layout = Layout::new(110, 38, state.pane_heights, snapshot.processes.len());
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
        let lines: Vec<_> = frame.plain().lines().map(str::to_string).collect();
        assert!(lines[layout.build.start].contains("BUILD"));
        assert!(lines[layout.processes.as_ref().unwrap().start].contains("PROCESSES"));
        assert!(lines[layout.log.as_ref().unwrap().start].contains("BUILD LOG"));
        let logs = lines[layout.log.as_ref().unwrap().start..].join("\n");
        for hidden in [
            "Load ",
            "Swap ",
            "Available ",
            "Latest event",
            "last read",
            "PID ",
        ] {
            assert!(
                !logs.contains(hidden),
                "Clipped content leaked into logs: {hidden}"
            );
        }
    }

    #[test]
    fn switching_layout_modes_cancels_an_in_progress_drag() {
        let snapshot = crate::monitor::demo(0);
        let mut state = ViewState::default();
        state.mouse(MouseKind::Press, 9, 18, (110, 38), &snapshot);
        assert!(state.dragging_divider.is_some());
        state.mouse(MouseKind::Drag, 9, 16, (79, 24), &snapshot);
        assert!(state.dragging_divider.is_none());
        assert!(state.pane_heights.is_none());
    }

    #[test]
    fn divider_drag_resizes_logs_and_preserves_independent_scroll_state() {
        let snapshot = crate::monitor::demo(0);
        let mut state = ViewState {
            pane_heights: Some([7, 7, 6]),
            process_offset: 3,
            offset: 6,
            follow: false,
            ..ViewState::default()
        };
        assert!(!state.mouse(MouseKind::Drag, 9, 22, (110, 38), &snapshot));
        assert!(state.mouse(MouseKind::Press, 9, 24, (110, 38), &snapshot));
        assert!(state.dragging_divider.is_some());
        assert!(state.mouse(MouseKind::Drag, 9, 22, (110, 38), &snapshot));
        let expanded = Layout::new(110, 38, state.pane_heights, snapshot.processes.len());
        assert_eq!(expanded.log.as_ref().unwrap().start, 22);
        assert_eq!(expanded.process_rows(), 1);
        assert_eq!(
            (state.process_offset, state.offset, state.follow),
            (3, 6, false)
        );
        assert!(state.mouse(MouseKind::Release, 9, 22, (110, 38), &snapshot));
        let height = state.pane_heights;
        assert!(!state.mouse(MouseKind::Drag, 9, 30, (110, 38), &snapshot));
        assert_eq!(state.pane_heights, height);
        assert!(state.mouse(MouseKind::Press, 9, 22, (110, 38), &snapshot));
        assert!(state.mouse(MouseKind::Drag, 9, usize::MAX, (110, 38), &snapshot));
        let shrunk = Layout::new(110, 38, state.pane_heights, snapshot.processes.len());
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
            pane_heights: Some([7, 7, 6]),
            follow: true,
            ..ViewState::default()
        };
        assert!(!state.mouse(MouseKind::Press, 9, 14, (110, 38), &snapshot));
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
        assert!(state.dragging_divider.is_none());
    }

    #[test]
    fn resized_layouts_keep_minimum_rows_and_fit_terminal_bounds() {
        let snapshot = crate::monitor::demo(0);
        let log = LogTail::default();
        for (width, height) in [(40, 12), (52, 20), (79, 24), (90, 32), (110, 38), (110, 60)] {
            for requested in [
                None,
                Some([0; 3]),
                Some([3; 3]),
                Some([7, 7, 6]),
                Some([usize::MAX; 3]),
            ] {
                let state = ViewState {
                    pane_heights: requested,
                    ..ViewState::default()
                };
                let layout = Layout::new(width, height, requested, snapshot.processes.len());
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
            pane_heights: Some([7, 7, 6]),
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
        let mut state = ViewState {
            pane_heights: Some([7, 7, 6]),
            ..ViewState::default()
        };
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
