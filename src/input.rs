//! Bounded terminal keyboard and mouse decoding.
//!
//! Mouse encodings follow XTerm Control Sequences, "Normal tracking mode",
//! "Wheel mice", and "SGR (1006)":
//! https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-Mouse-Tracking
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Key(u8),
    Up,
    Down,
    Wheel {
        up: bool,
        column: usize,
        row: usize,
    },
    Mouse {
        kind: MouseKind,
        column: usize,
        row: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Drag,
    Release,
}

#[derive(Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi,
    DiscardCsi,
    Ss3,
    X10,
}

pub struct Decoder {
    state: State,
    bytes: [u8; 64],
    len: usize,
    escape_at: Option<Instant>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            state: State::default(),
            bytes: [0; 64],
            len: 0,
            escape_at: None,
        }
    }
}

impl Decoder {
    /// Resolve a standalone Escape without splitting normal escape packets.
    pub fn take_escape(&mut self) -> bool {
        if matches!(self.state, State::Escape)
            && self
                .escape_at
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(100))
        {
            self.state = State::Ground;
            self.len = 0;
            self.escape_at = None;
            true
        } else {
            false
        }
    }

    /// Feed one wire byte. Mouse coordinates in returned events are zero-based.
    /// Unknown and malformed escape sequences are consumed, not sent as keys.
    pub fn feed(&mut self, byte: u8) -> Option<Event> {
        // Interrupt/EOF bytes cannot be valid mouse fields. Preserve an escape
        // hatch even when a terminal report was truncated or malformed.
        if matches!(byte, 3 | 4) {
            self.state = State::Ground;
            self.len = 0;
            self.escape_at = None;
            return Some(Event::Key(byte));
        }
        // X10 has exactly three raw payload bytes, which must never become keys.
        if matches!(self.state, State::X10) {
            self.bytes[self.len] = byte;
            self.len += 1;
            if self.len == 3 {
                self.state = State::Ground;
                self.len = 0;
                return mouse(
                    self.bytes[0].checked_sub(32)? as usize,
                    self.bytes[1].checked_sub(32)? as usize,
                    self.bytes[2].checked_sub(32)? as usize,
                    false,
                );
            }
            return None;
        }
        if byte == 27 {
            self.state = State::Escape;
            self.len = 0;
            self.escape_at = Some(Instant::now());
            return None;
        }
        match self.state {
            State::Ground => Some(Event::Key(byte)),
            State::Escape => {
                self.escape_at = None;
                self.state = match byte {
                    b'[' => State::Csi,
                    b'O' => State::Ss3,
                    _ => State::Ground,
                };
                if matches!(self.state, State::Ground) {
                    Some(Event::Key(byte))
                } else {
                    None
                }
            }
            State::Ss3 => {
                self.state = State::Ground;
                arrow(byte)
            }
            State::Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    if byte == b'M' && self.len == 0 {
                        self.state = State::X10;
                        return None;
                    }
                    self.state = State::Ground;
                    if self.len == 0 {
                        return arrow(byte);
                    }
                    if byte == b'M' || byte == b'm' {
                        return sgr(&self.bytes[..self.len], byte == b'm');
                    }
                    // Lowercase 'm' is a release, not a wheel press.
                    None
                } else if (0x20..=0x3f).contains(&byte) && self.len < self.bytes.len() {
                    self.bytes[self.len] = byte;
                    self.len += 1;
                    None
                } else {
                    self.state = State::DiscardCsi;
                    None
                }
            }
            State::DiscardCsi => {
                if (0x40..=0x7e).contains(&byte) {
                    self.state = State::Ground;
                }
                None
            }
            State::X10 => unreachable!("X10 payload is handled above"),
        }
    }
}

fn arrow(byte: u8) -> Option<Event> {
    match byte {
        b'A' => Some(Event::Up),
        b'B' => Some(Event::Down),
        _ => None,
    }
}

fn sgr(bytes: &[u8], release: bool) -> Option<Event> {
    let text = std::str::from_utf8(bytes).ok()?.strip_prefix('<')?;
    let mut fields = text.split(';');
    let mut number = || {
        let value = fields.next()?;
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        value.parse::<usize>().ok()
    };
    let (button, column, row) = (number()?, number()?, number()?);
    if fields.next().is_some() {
        return None;
    }
    mouse(button, column, row, release)
}

fn mouse(button: usize, column: usize, row: usize, release: bool) -> Option<Event> {
    // Bits 2–4 are Shift/Meta/Control. Ignore right clicks and hover-only motion.
    let button = button & !(4 | 8 | 16);
    let (column, row) = (column.checked_sub(1)?, row.checked_sub(1)?);
    if !release && (button == 64 || button == 65) {
        return Some(Event::Wheel {
            up: button == 64,
            column,
            row,
        });
    }
    let kind = match (button, release) {
        (0 | 3, true) | (3, false) => MouseKind::Release,
        (0, false) => MouseKind::Press,
        (32, false) => MouseKind::Drag,
        _ => return None,
    };
    Some(Event::Mouse { kind, column, row })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_escape_waits_but_does_not_consume_arrow_sequences() {
        let mut decoder = Decoder::default();
        decoder.feed(27);
        assert!(!decoder.take_escape());
        decoder.escape_at = Some(Instant::now() - Duration::from_secs(1));
        assert!(decoder.take_escape());
        assert_eq!(decoder.feed(b'p'), Some(Event::Key(b'p')));
        for byte in b"\x1b[A" {
            let _ = decoder.feed(*byte);
        }
        assert!(!decoder.take_escape());
    }

    fn decode(bytes: &[u8]) -> Vec<Event> {
        let mut decoder = Decoder::default();
        bytes.iter().filter_map(|b| decoder.feed(*b)).collect()
    }

    #[test]
    fn existing_keyboard_sequences_work() {
        assert_eq!(
            decode(b"q \x03\x1b[A\x1b[B\x1bOA\x1bOB\x1bf"),
            vec![
                Event::Key(b'q'),
                Event::Key(b' '),
                Event::Key(3),
                Event::Up,
                Event::Down,
                Event::Up,
                Event::Down,
                Event::Key(b'f'),
            ]
        );
    }

    #[test]
    fn fragmented_sgr_wheels_have_zero_based_coordinates() {
        let mut decoder = Decoder::default();
        for byte in b"\x1b[<64;10;22" {
            assert_eq!(decoder.feed(*byte), None);
        }
        assert_eq!(
            decoder.feed(b'M'),
            Some(Event::Wheel {
                up: true,
                column: 9,
                row: 21
            })
        );
        for byte in b"\x1b[<65;1;1" {
            assert_eq!(decoder.feed(*byte), None);
        }
        assert_eq!(
            decoder.feed(b'M'),
            Some(Event::Wheel {
                up: false,
                column: 0,
                row: 0
            })
        );
    }

    #[test]
    fn modifiers_are_allowed_but_unneeded_mouse_events_are_ignored() {
        for modifiers in [0, 4, 8, 16, 28] {
            assert_eq!(
                decode(format!("\x1b[<{};5;6M", 64 + modifiers).as_bytes()),
                vec![Event::Wheel {
                    up: true,
                    column: 4,
                    row: 5
                }]
            );
        }
        for packet in [
            b"\x1b[<1;5;6M".as_slice(),
            b"\x1b[<64;5;6m",
            b"\x1b[<66;5;6M",
            b"\x1b[<67;5;6M",
            b"\x1b[<96;5;6M",
        ] {
            assert_eq!(decode(packet), vec![]);
        }
    }

    #[test]
    fn left_press_drag_and_release_preserve_coordinates_and_order() {
        assert_eq!(
            decode(b"\x1b[<0;10;25M\x1b[<32;10;23M\x1b[<0;10;23m"),
            vec![
                Event::Mouse {
                    kind: MouseKind::Press,
                    column: 9,
                    row: 24
                },
                Event::Mouse {
                    kind: MouseKind::Drag,
                    column: 9,
                    row: 22
                },
                Event::Mouse {
                    kind: MouseKind::Release,
                    column: 9,
                    row: 22
                },
            ]
        );
        assert_eq!(
            decode(b"\x1b[<3;10;23m"),
            vec![Event::Mouse {
                kind: MouseKind::Release,
                column: 9,
                row: 22
            }]
        );
    }

    #[test]
    fn malformed_and_overflowing_coordinates_are_ignored_without_key_leaks() {
        for packet in [
            "\x1b[<64;0;2M",
            "\x1b[<64;1;0M",
            "\x1b[<64;;2M",
            "\x1b[<64;1;2;3M",
            "\x1b[<64;-1;2M",
            "\x1b[<64;184467440737095516160;2M",
            "\x1b[<64; 1;2M",
            "\x1b[<64;1;2q",
        ] {
            assert_eq!(decode(packet.as_bytes()), vec![], "{packet:?}");
        }
    }

    #[test]
    fn oversized_sequence_is_bounded_and_recovers_after_terminator() {
        let mut decoder = Decoder::default();
        for byte in b"\x1b[<64;"
            .iter()
            .chain(std::iter::repeat(&b'1').take(1000))
            .chain(b";2M")
        {
            assert_eq!(decoder.feed(*byte), None);
        }
        assert!(decoder.len <= 64);
        assert_eq!(decoder.feed(b'q'), Some(Event::Key(b'q')));
        for byte in b"\x1b[A".iter().take(2) {
            assert_eq!(decoder.feed(*byte), None);
        }
        assert_eq!(decoder.feed(b'A'), Some(Event::Up));
    }

    #[test]
    fn x10_payload_is_consumed_including_key_shaped_coordinate_bytes() {
        assert_eq!(
            decode(b"\x1b[M`!q"),
            vec![Event::Wheel {
                up: true,
                column: 0,
                row: 80
            }]
        );
        assert_eq!(
            decode(b"\x1b[Ma!!"),
            vec![Event::Wheel {
                up: false,
                column: 0,
                row: 0
            }]
        );
        assert_eq!(
            decode(b"\x1b[M qq"),
            vec![Event::Mouse {
                kind: MouseKind::Press,
                column: 80,
                row: 80
            }]
        );
        assert_eq!(decode(b"\x1b[M` !"), vec![]);
        assert_eq!(
            decode(b"\x1b[M qqf"),
            vec![
                Event::Mouse {
                    kind: MouseKind::Press,
                    column: 80,
                    row: 80
                },
                Event::Key(b'f')
            ]
        );
    }

    #[test]
    fn interrupt_and_eof_recover_from_incomplete_and_oversized_reports() {
        for prefix in [b"\x1b[<64;".as_slice(), b"\x1b[M`", b"\x1bO"] {
            let mut decoder = Decoder::default();
            for byte in prefix {
                assert_eq!(decoder.feed(*byte), None);
            }
            assert_eq!(decoder.feed(3), Some(Event::Key(3)));
            assert_eq!(decoder.feed(b'f'), Some(Event::Key(b'f')));
        }
        let mut decoder = Decoder::default();
        for byte in b"\x1b[<64;"
            .iter()
            .chain(std::iter::repeat(&b'1').take(1000))
        {
            assert_eq!(decoder.feed(*byte), None);
        }
        assert_eq!(decoder.feed(4), Some(Event::Key(4)));
        assert_eq!(decoder.feed(b'q'), Some(Event::Key(b'q')));
    }
}
