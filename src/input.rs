use unicode_width::UnicodeWidthStr;

/// Move one Unicode scalar value to the left, returning the new byte index.
pub fn cursor_prev_char(s: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos - 1;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Move one Unicode scalar value to the right, returning the new byte index.
pub fn cursor_next_char(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut i = pos + 1;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Jump one word to the left (bash/readline style): skip whitespace then
/// non-whitespace going backwards.
pub fn cursor_prev_word(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = pos;
    // skip trailing spaces
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    // skip word chars
    while i > 0 && !bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    i
}

/// Jump one word to the right (bash/readline style): skip non-whitespace then
/// whitespace going forward.
pub fn cursor_next_word(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let len = s.len();
    let mut i = pos;
    // skip word chars
    while i < len && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    // skip trailing spaces
    while i < len && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

pub fn cursor_row_col(s: &str, pos: usize) -> (u16, u16) {
    let before = &s[..pos];
    let row = before.chars().filter(|&c| c == '\n').count() as u16;
    let last_nl = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = UnicodeWidthStr::width(&before[last_nl..]) as u16;
    (row, col)
}
