//! Console data types: the crawler's output as styled lines, and a bounded ring buffer of them.
//! The parser that turns raw bytes (ANSI SGR sequences, `\r`) into these lines is
//! `backend::ansi`.

use std::collections::VecDeque;

/// The console keeps at most this many lines (GUI-SPEC §5.4).
pub const CONSOLE_CAPACITY: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// A terminal colour as an SGR sequence names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnsiColor {
    /// The 16 standard colours: 0–7 normal (black, red, green, yellow, blue, magenta, cyan,
    /// white), 8–15 bright.
    Standard(u8),
    /// The 256-colour palette (`38;5;n`).
    Indexed(u8),
    /// True colour (`38;2;r;g;b`).
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Option<AnsiColor>,
    pub bg: Option<AnsiColor>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

/// A run of text in one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// One line of output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLine {
    pub stream: Stream,
    pub spans: Vec<Span>,
}

impl StyledLine {
    /// An unstyled line.
    pub fn plain(stream: Stream, text: impl Into<String>) -> Self {
        Self {
            stream,
            spans: vec![Span {
                text: text.into(),
                style: Style::default(),
            }],
        }
    }

    /// The line's text without styles.
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// The last [`CONSOLE_CAPACITY`] lines; older ones are dropped and counted.
#[derive(Debug, Clone)]
pub struct ConsoleBuffer {
    lines: VecDeque<StyledLine>,
    capacity: usize,
    dropped: u64,
}

impl Default for ConsoleBuffer {
    fn default() -> Self {
        Self::with_capacity(CONSOLE_CAPACITY)
    }
}

impl ConsoleBuffer {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            capacity: capacity.max(1),
            dropped: 0,
        }
    }

    pub fn push(&mut self, line: StyledLine) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line);
    }

    pub fn lines(&self) -> &VecDeque<StyledLine> {
        &self.lines
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Lines dropped because the buffer was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// All lines as plain text (for *Copy all*).
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            text.push_str(&line.text());
            text.push('\n');
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_keeps_the_newest_lines() {
        let mut buffer = ConsoleBuffer::with_capacity(3);
        for n in 0..5 {
            buffer.push(StyledLine::plain(Stream::Stdout, format!("line {n}")));
        }
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.dropped(), 2);
        assert_eq!(buffer.plain_text(), "line 2\nline 3\nline 4\n");
    }
}
