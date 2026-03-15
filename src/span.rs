/// Byte offset + length into the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub offset: usize,
    pub length: usize,
}

impl Span {
    pub const DUMMY: Span = Span {
        offset: 0,
        length: 0,
    };

    pub fn new(offset: usize, length: usize) -> Self {
        Self { offset, length }
    }

    pub fn merge(self, other: Span) -> Span {
        let start = self.offset.min(other.offset);
        let end = (self.offset + self.length).max(other.offset + other.length);
        Span::new(start, end - start)
    }

    pub fn line_col(&self, source: &str) -> (usize, usize) {
        let mut line = 1;
        let mut col = 1;
        for (i, ch) in source.char_indices() {
            if i >= self.offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}
