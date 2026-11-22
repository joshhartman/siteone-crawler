//! A streaming parser of the crawler's console output: ANSI SGR sequences (colours, bold, …) and
//! carriage returns (`\r` rewrites the current line) → [`StyledLine`]s. Bytes may arrive in any
//! chunks: a sequence or a UTF-8 character split across two reads is completed by the next one.
//!
//! What it understands:
//!
//! - SGR (`ESC [ … m`): reset, bold, dim, italic, underline, inverse and their "off" codes, the 16
//!   standard foreground/background colours (`30–37`, `90–97`, `40–47`, `100–107`), the 256-colour
//!   palette (`38;5;n`) and true colour (`38;2;r;g;b`), default colours (`39`, `49`). The style
//!   carries over from one line to the next, as in a terminal.
//! - `\n` ends a line; `\r\n` too. A lone `\r` returns to the start of the line: the next text
//!   replaces the line (the crawler's progress lines). `\r` at the very end keeps the line.
//! - Every other escape sequence (other CSI commands, OSC/DCS strings, two-byte escapes) is dropped
//!   without touching the text around it; other C0 control characters except `\t` are dropped.
//!   A control character inside a sequence aborts the sequence and is processed as usual.
//! - Invalid UTF-8 becomes U+FFFD; a line is cut at [`MAX_LINE_BYTES`].
//!
//! An empty line has no spans.

use crate::model::console::{AnsiColor, Span, Stream, Style, StyledLine};

/// The longest line kept; longer lines are cut (bounded memory).
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// The longest parameter string of a CSI sequence that is interpreted; a longer one is dropped.
const MAX_CSI_PARAMS: usize = 64;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    /// After `ESC`.
    Escape,
    /// After `ESC` and intermediate bytes (`0x20–0x2F`), waiting for the final byte.
    EscapeIntermediate,
    /// Inside `ESC [`.
    Csi,
    /// Inside an OSC/DCS/SOS/PM/APC string, which ends with `BEL` or `ESC \`.
    String,
    /// `ESC` seen inside such a string.
    StringEscape,
}

pub struct AnsiParser {
    stream: Stream,
    state: State,
    style: Style,
    /// The finished spans of the current line.
    spans: Vec<Span>,
    /// Text bytes of the current span in [`Self::style`], decoded when the span ends (so a UTF-8
    /// character split across `feed` calls stays whole).
    text: Vec<u8>,
    /// Bytes of the current line so far (spans + text), for the length cap.
    line_bytes: usize,
    /// A `\r` was seen: the next text replaces the line, a `\n` keeps it.
    carriage_return: bool,
    /// The parameter bytes of the current CSI sequence.
    params: Vec<u8>,
    /// The current CSI sequence is too long or private: drop it.
    params_invalid: bool,
}

impl AnsiParser {
    pub fn new(stream: Stream) -> Self {
        Self {
            stream,
            state: State::Ground,
            style: Style::default(),
            spans: Vec::new(),
            text: Vec::new(),
            line_bytes: 0,
            carriage_return: false,
            params: Vec::new(),
            params_invalid: false,
        }
    }

    /// Feeds raw bytes; returns the lines completed by them (`\n`).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<StyledLine> {
        let mut lines = Vec::new();
        for &byte in bytes {
            self.byte(byte, &mut lines);
        }
        lines
    }

    /// Ends the stream: returns the unfinished last line, if any. An unfinished escape sequence
    /// is dropped.
    pub fn finish(&mut self) -> Option<StyledLine> {
        self.state = State::Ground;
        self.carriage_return = false;
        self.flush_text();
        (!self.spans.is_empty()).then(|| self.take_line())
    }

    fn byte(&mut self, byte: u8, lines: &mut Vec<StyledLine>) {
        match self.state {
            State::Ground => self.ground(byte, lines),
            State::Escape => match byte {
                b'[' => {
                    self.params.clear();
                    self.params_invalid = false;
                    self.state = State::Csi;
                }
                b']' | b'P' | b'X' | b'^' | b'_' => self.state = State::String,
                0x20..=0x2f => self.state = State::EscapeIntermediate,
                ESC => {}
                0x00..=0x1f => {
                    self.state = State::Ground;
                    self.ground(byte, lines);
                }
                // A two-byte escape (`ESC 7`, `ESC M`, …): dropped.
                _ => self.state = State::Ground,
            },
            State::EscapeIntermediate => match byte {
                0x20..=0x2f => {}
                ESC => self.state = State::Escape,
                0x00..=0x1f => {
                    self.state = State::Ground;
                    self.ground(byte, lines);
                }
                _ => self.state = State::Ground,
            },
            State::Csi => match byte {
                0x30..=0x3f => {
                    // `<`, `=`, `>`, `?` mark private sequences, which are not SGR.
                    if self.params.len() >= MAX_CSI_PARAMS || byte >= b'<' {
                        self.params_invalid = true;
                    } else {
                        self.params.push(byte);
                    }
                }
                // Intermediate bytes: no SGR has them.
                0x20..=0x2f => self.params_invalid = true,
                0x40..=0x7e => {
                    if byte == b'm' && !self.params_invalid {
                        self.apply_sgr();
                    }
                    self.state = State::Ground;
                }
                ESC => self.state = State::Escape,
                0x00..=0x1f => {
                    self.state = State::Ground;
                    self.ground(byte, lines);
                }
                // DEL or a byte ≥ 0x80 cannot be part of a CSI sequence: abort it, keep the byte.
                _ => {
                    self.state = State::Ground;
                    self.ground(byte, lines);
                }
            },
            State::String => match byte {
                BEL => self.state = State::Ground,
                ESC => self.state = State::StringEscape,
                _ => {}
            },
            State::StringEscape => match byte {
                b'\\' => self.state = State::Ground,
                ESC => {}
                _ => self.state = State::String,
            },
        }
    }

    fn ground(&mut self, byte: u8, lines: &mut Vec<StyledLine>) {
        match byte {
            b'\n' => {
                self.carriage_return = false;
                self.flush_text();
                lines.push(self.take_line());
            }
            b'\r' => self.carriage_return = true,
            ESC => self.state = State::Escape,
            b'\t' => self.push_text(byte),
            0x00..=0x1f | 0x7f => {}
            _ => self.push_text(byte),
        }
    }

    fn push_text(&mut self, byte: u8) {
        if self.carriage_return {
            // Back at the start of the line: the new text replaces it.
            self.carriage_return = false;
            self.spans.clear();
            self.text.clear();
            self.line_bytes = 0;
        }
        if self.line_bytes < MAX_LINE_BYTES {
            self.text.push(byte);
            self.line_bytes += 1;
        }
    }

    /// Ends the current span (when it has text).
    fn flush_text(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&self.text).into_owned();
        self.text.clear();
        match self.spans.last_mut() {
            Some(last) if last.style == self.style => last.text.push_str(&text),
            _ => self.spans.push(Span {
                text,
                style: self.style,
            }),
        }
    }

    fn set_style(&mut self, style: Style) {
        if style != self.style {
            self.flush_text();
            self.style = style;
        }
    }

    fn take_line(&mut self) -> StyledLine {
        self.line_bytes = 0;
        StyledLine {
            stream: self.stream,
            spans: std::mem::take(&mut self.spans),
        }
    }

    fn apply_sgr(&mut self) {
        let text = String::from_utf8_lossy(&self.params).into_owned();
        // Colon sub-parameters (`38:2::r:g:b`) are not used by the crawler: such a parameter is
        // skipped, the others still apply.
        let codes: Vec<Option<u16>> = text
            .split(';')
            .map(|part| match part {
                "" => Some(0),
                part if part.contains(':') => None,
                part => part.parse().ok(),
            })
            .collect();
        let mut style = self.style;
        let mut i = 0;
        while i < codes.len() {
            let Some(code) = codes[i] else {
                i += 1;
                continue;
            };
            match code {
                0 => style = Style::default(),
                1 => style.bold = true,
                2 => style.dim = true,
                3 => style.italic = true,
                4 => style.underline = true,
                7 => style.inverse = true,
                22 => {
                    style.bold = false;
                    style.dim = false;
                }
                23 => style.italic = false,
                24 => style.underline = false,
                27 => style.inverse = false,
                30..=37 => style.fg = Some(AnsiColor::Standard((code - 30) as u8)),
                39 => style.fg = None,
                40..=47 => style.bg = Some(AnsiColor::Standard((code - 40) as u8)),
                49 => style.bg = None,
                90..=97 => style.fg = Some(AnsiColor::Standard((code - 90 + 8) as u8)),
                100..=107 => style.bg = Some(AnsiColor::Standard((code - 100 + 8) as u8)),
                38 | 48 => {
                    let (color, used) = extended_color(&codes[i + 1..]);
                    if let Some(color) = color {
                        if code == 38 {
                            style.fg = Some(color);
                        } else {
                            style.bg = Some(color);
                        }
                    }
                    i += used;
                }
                _ => {}
            }
            i += 1;
        }
        self.set_style(style);
    }
}

/// The colour after `38`/`48` (`5;n` or `2;r;g;b`) and how many parameters it used.
fn extended_color(rest: &[Option<u16>]) -> (Option<AnsiColor>, usize) {
    let byte = |index: usize| rest.get(index).copied().flatten().and_then(|v| u8::try_from(v).ok());
    match rest.first().copied().flatten() {
        Some(5) => (byte(1).map(AnsiColor::Indexed), 2.min(rest.len())),
        Some(2) => {
            let color = match (byte(1), byte(2), byte(3)) {
                (Some(r), Some(g), Some(b)) => Some(AnsiColor::Rgb(r, g, b)),
                _ => None,
            };
            (color, 4.min(rest.len()))
        }
        // Unknown colour space: the rest of the sequence cannot be read reliably.
        _ => (None, rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[StyledLine]) -> Vec<String> {
        lines.iter().map(StyledLine::text).collect()
    }

    fn red() -> Style {
        Style {
            fg: Some(AnsiColor::Standard(1)),
            ..Style::default()
        }
    }

    #[test]
    fn lines_split_across_chunks_and_crlf_ends_a_line() {
        let mut parser = AnsiParser::new(Stream::Stdout);
        assert!(parser.feed(b"hel").is_empty());
        let lines = parser.feed(b"lo\r\nwor");
        assert_eq!(texts(&lines), ["hello"]);
        assert_eq!(lines[0].stream, Stream::Stdout);
        assert_eq!(parser.finish().map(|line| line.text()).as_deref(), Some("wor"));
        assert_eq!(parser.finish(), None);
        // `\r` and `\n` in separate chunks.
        assert!(parser.feed(b"a\r").is_empty());
        assert_eq!(texts(&parser.feed(b"\n")), ["a"]);
    }

    #[test]
    fn the_colours_the_crawler_emits_become_styles() {
        let mut parser = AnsiParser::new(Stream::Stderr);
        // `utils::get_color_text`: `0;31` foreground, `38;5;244` gray, `1;41` background.
        let lines = parser.feed(b"\x1b[0;31mERROR\x1b[0m ok \x1b[38;5;244mgray\x1b[0m \x1b[1;41mbg\x1b[0m\n");
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 5);
        assert_eq!((spans[0].text.as_str(), spans[0].style), ("ERROR", red()));
        assert_eq!((spans[1].text.as_str(), spans[1].style), (" ok ", Style::default()));
        assert_eq!(spans[2].style.fg, Some(AnsiColor::Indexed(244)));
        assert_eq!(
            spans[4].style,
            Style {
                bg: Some(AnsiColor::Standard(1)),
                bold: true,
                ..Style::default()
            }
        );
        assert_eq!(lines[0].text(), "ERROR ok gray bg");
    }

    #[test]
    fn attributes_bright_true_colour_and_off_codes() {
        let mut parser = AnsiParser::new(Stream::Stdout);
        let lines = parser.feed(b"\x1b[1;2;3;4;7;92;104ma\x1b[22;23;24;27;39;49mb\x1b[38;2;10;20;30;48;5;9mc\x1b[mD\n");
        let spans = &lines[0].spans;
        assert_eq!(
            spans[0].style,
            Style {
                fg: Some(AnsiColor::Standard(10)),
                bg: Some(AnsiColor::Standard(12)),
                bold: true,
                dim: true,
                italic: true,
                underline: true,
                inverse: true,
            }
        );
        assert_eq!(spans[1].style, Style::default());
        assert_eq!(spans[2].style.fg, Some(AnsiColor::Rgb(10, 20, 30)));
        assert_eq!(spans[2].style.bg, Some(AnsiColor::Indexed(9)));
        assert_eq!((spans[3].text.as_str(), spans[3].style), ("D", Style::default()));
    }

    #[test]
    fn the_style_carries_over_to_the_next_line() {
        let mut parser = AnsiParser::new(Stream::Stdout);
        let lines = parser.feed(b"\x1b[31mone\ntwo\x1b[0m\n");
        assert_eq!(lines[1].spans[0].style, red());
    }

    #[test]
    fn a_carriage_return_rewrites_the_current_line() {
        let mut parser = AnsiParser::new(Stream::Stdout);
        let lines = parser.feed(b"progress 10%\rprogress 20%\rdone\n");
        assert_eq!(texts(&lines), ["done"]);
        // Split across chunks, and with a colour change between `\r` and the new text.
        assert!(parser.feed(b"old text\r").is_empty());
        let lines = parser.feed(b"\x1b[31mnew\x1b[0m\n");
        assert_eq!(texts(&lines), ["new"]);
        assert_eq!(lines[0].spans[0].style, red());
        // A `\r` before the line end, even with a sequence between them, keeps the line.
        assert_eq!(texts(&parser.feed(b"kept\r\x1b[0m\r\n")), ["kept"]);
        // At the end of the stream, too.
        parser.feed(b"last\r");
        assert_eq!(parser.finish().map(|line| line.text()).as_deref(), Some("last"));
    }

    #[test]
    fn sequences_split_across_chunks_are_completed() {
        let input = b"a\x1b[0;31mred\x1b[38;5;244mgray\x1b[0m\n";
        let mut whole = AnsiParser::new(Stream::Stdout);
        let expected = whole.feed(input);
        for split in 1..input.len() {
            let mut parser = AnsiParser::new(Stream::Stdout);
            let mut lines = parser.feed(&input[..split]);
            lines.extend(parser.feed(&input[split..]));
            assert_eq!(lines, expected, "split at {split}");
        }
        // Byte by byte.
        let mut parser = AnsiParser::new(Stream::Stdout);
        let lines: Vec<StyledLine> = input.iter().flat_map(|byte| parser.feed(&[*byte])).collect();
        assert_eq!(lines, expected);
    }

    #[test]
    fn utf8_split_across_chunks_stays_whole() {
        let input = "žluťoučký ✓ 🦀\n".as_bytes();
        for split in 1..input.len() {
            let mut parser = AnsiParser::new(Stream::Stdout);
            let mut lines = parser.feed(&input[..split]);
            lines.extend(parser.feed(&input[split..]));
            assert_eq!(texts(&lines), ["žluťoučký ✓ 🦀"], "split at {split}");
        }
        let mut parser = AnsiParser::new(Stream::Stdout);
        assert_eq!(texts(&parser.feed(b"bad \xff byte\n")), ["bad \u{fffd} byte"]);
    }

    #[test]
    fn unknown_sequences_are_dropped_without_corrupting_text() {
        let mut parser = AnsiParser::new(Stream::Stdout);
        let lines = parser.feed(
            b"a\x1b[2Kb\x1b[?25lc\x1b]0;window title\x07d\x1b]8;;http://x\x1b\\e\x1b7f\x1b(Bg\x1b[1 qh\x08\x07i\tj\n",
        );
        assert_eq!(texts(&lines), ["abcdefghi\tj"]);
        assert_eq!(lines[0].spans.len(), 1);
        // A private sequence ending in `m` is not SGR.
        assert_eq!(parser.feed(b"\x1b[>4;1mplain\n")[0].spans[0].style, Style::default());
        // A newline inside a sequence aborts it and still ends the line.
        assert_eq!(texts(&parser.feed(b"x\x1b[31\ny\n")), ["x", "y"]);
        assert_eq!(parser.feed(b"z\n")[0].spans[0].style, Style::default());
        // Unknown SGR codes and malformed extended colours are ignored.
        let lines = parser.feed(b"\x1b[5;31;38;7mq\x1b[38;5mr\n");
        assert_eq!(lines[0].spans[0].style, red());
        // An endless parameter list does not grow memory and is dropped.
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'1', 10_000));
        long.extend(b"mtext\n");
        assert_eq!(parser.feed(&long)[0].spans[0].style, red());
    }

    #[test]
    fn empty_lines_have_no_spans_and_long_lines_are_cut() {
        let mut parser = AnsiParser::new(Stream::Stdout);
        let lines = parser.feed(b"\n\x1b[31m\x1b[0m\n");
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.spans.is_empty()));
        assert_eq!(parser.finish(), None);

        let mut long = vec![b'x'; MAX_LINE_BYTES + 100];
        long.push(b'\n');
        let lines = parser.feed(&long);
        assert_eq!(lines[0].text().len(), MAX_LINE_BYTES);
        assert_eq!(texts(&parser.feed(b"next\n")), ["next"]);
    }
}
