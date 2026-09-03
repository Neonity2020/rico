//! Unicode-aware text editing, cursor geometry, and wrapping utilities.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

fn is_closing_punct(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。'
            | '！'
            | '？'
            | '：'
            | '；'
            | '、'
            | '）'
            | '】'
            | '》'
            | '”'
            | '’'
            | ','
            | '.'
            | '!'
            | '?'
            | ':'
            | ';'
            | ')'
            | ']'
            | '}'
            | '>'
            | '\''
            | '"'
    )
}

fn is_opening_punct(ch: char) -> bool {
    matches!(ch, '（' | '【' | '《' | '“' | '‘' | '(' | '[' | '{' | '<')
}

fn is_cjk_char(ch: char) -> bool {
    UnicodeWidthChar::width(ch).unwrap_or(1) == 2
}

#[derive(Debug, Clone)]
struct Token<'a> {
    text: &'a str,
    byte_offset: usize,
    width: usize,
    is_whitespace: bool,
}

fn tokenize<'a>(line: &'a str, base_byte: usize) -> Vec<Token<'a>> {
    let mut tokens = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some(&(idx, ch)) = chars.peek() {
        if ch.is_whitespace() {
            let start = idx;
            while let Some(&(_, next_ch)) = chars.peek() {
                if next_ch.is_whitespace() {
                    chars.next();
                } else {
                    break;
                }
            }
            let end = chars.peek().map(|&(i, _)| i).unwrap_or(line.len());
            let s = &line[start..end];
            tokens.push(Token {
                text: s,
                byte_offset: base_byte + start,
                width: UnicodeWidthStr::width(s),
                is_whitespace: true,
            });
        } else if is_cjk_char(ch) || is_closing_punct(ch) || is_opening_punct(ch) {
            chars.next();
            let end = chars.peek().map(|&(i, _)| i).unwrap_or(line.len());
            let s = &line[idx..end];
            tokens.push(Token {
                text: s,
                byte_offset: base_byte + idx,
                width: UnicodeWidthStr::width(s),
                is_whitespace: false,
            });
        } else {
            let start = idx;
            while let Some(&(_, next_ch)) = chars.peek() {
                if !next_ch.is_whitespace()
                    && !is_cjk_char(next_ch)
                    && !is_closing_punct(next_ch)
                    && !is_opening_punct(next_ch)
                {
                    chars.next();
                } else {
                    break;
                }
            }
            let end = chars.peek().map(|&(i, _)| i).unwrap_or(line.len());
            let s = &line[start..end];
            tokens.push(Token {
                text: s,
                byte_offset: base_byte + start,
                width: UnicodeWidthStr::width(s),
                is_whitespace: false,
            });
        }
    }
    tokens
}

#[derive(Debug, Clone)]
struct LineSegment<'a> {
    text: &'a str,
    byte_offset: usize,
    width: usize,
}

#[derive(Debug, Clone)]
struct VisualLine<'a> {
    segments: Vec<LineSegment<'a>>,
    width: usize,
    start_byte: usize,
    end_byte: usize,
}

impl<'a> VisualLine<'a> {
    fn new(start_byte: usize) -> Self {
        Self {
            segments: Vec::new(),
            width: 0,
            start_byte,
            end_byte: start_byte,
        }
    }

    fn push(&mut self, token: Token<'a>) {
        self.width += token.width;
        self.end_byte = token.byte_offset + token.text.len();
        self.segments.push(LineSegment {
            text: token.text,
            byte_offset: token.byte_offset,
            width: token.width,
        });
    }

    fn pop(&mut self) -> Option<Token<'a>> {
        let seg = self.segments.pop()?;
        self.width -= seg.width;
        self.end_byte = self
            .segments
            .last()
            .map(|s| s.byte_offset + s.text.len())
            .unwrap_or(self.start_byte);
        Some(Token {
            text: seg.text,
            byte_offset: seg.byte_offset,
            width: seg.width,
            is_whitespace: false,
        })
    }

    fn text(&self) -> String {
        self.segments.iter().map(|s| s.text).collect()
    }
}

fn wrap_lines<'a>(text: &'a str, max_width: usize) -> Vec<VisualLine<'a>> {
    let max_width = max_width.max(1);
    let mut visual_lines = Vec::new();
    let mut base_byte = 0;

    let natural_lines: Vec<&str> = text.split('\n').collect();
    let total_natural = natural_lines.len();

    for (nat_idx, line) in natural_lines.into_iter().enumerate() {
        let tokens = tokenize(line, base_byte);
        let mut curr = VisualLine::new(base_byte);

        if tokens.is_empty() {
            visual_lines.push(curr);
        } else {
            let mut is_wrapped_continuation = false;
            for token in tokens {
                if curr.width == 0 && token.is_whitespace && is_wrapped_continuation {
                    continue;
                }

                if curr.width + token.width <= max_width {
                    curr.push(token);
                } else {
                    let first_ch = token.text.chars().next().unwrap_or(' ');
                    if is_closing_punct(first_ch) && !curr.segments.is_empty() {
                        if let Some(prev) = curr.pop() {
                            let next_start = curr.end_byte;
                            visual_lines
                                .push(std::mem::replace(&mut curr, VisualLine::new(next_start)));
                            curr.push(prev);
                            curr.push(token);
                            is_wrapped_continuation = true;
                            continue;
                        }
                    }

                    if !curr.segments.is_empty() {
                        let next_start = curr.end_byte;
                        visual_lines
                            .push(std::mem::replace(&mut curr, VisualLine::new(next_start)));
                        is_wrapped_continuation = true;
                    }

                    if token.is_whitespace {
                        continue;
                    }

                    if token.width > max_width {
                        for (g_idx, grapheme) in token.text.graphemes(true).enumerate() {
                            let gw = UnicodeWidthStr::width(grapheme);
                            let g_byte = token.byte_offset
                                + token
                                    .text
                                    .grapheme_indices(true)
                                    .nth(g_idx)
                                    .map(|(i, _)| i)
                                    .unwrap_or(0);
                            if curr.width + gw > max_width && !curr.segments.is_empty() {
                                let next_start = curr.end_byte;
                                visual_lines.push(std::mem::replace(
                                    &mut curr,
                                    VisualLine::new(next_start),
                                ));
                                is_wrapped_continuation = true;
                            }
                            curr.push(Token {
                                text: grapheme,
                                byte_offset: g_byte,
                                width: gw,
                                is_whitespace: false,
                            });
                        }
                    } else {
                        curr.push(token);
                    }
                }
            }

            if !curr.segments.is_empty() {
                visual_lines.push(curr);
            }
        }

        base_byte += line.len();
        if nat_idx + 1 < total_natural {
            base_byte += 1;
        }
    }

    if visual_lines.is_empty() {
        visual_lines.push(VisualLine::new(0));
    }

    visual_lines
}

pub fn editor_rows(text: &str, width: usize) -> usize {
    wrap_lines(text, width).len().max(1)
}

pub fn cursor_position(text: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let cursor = cursor.min(text.len());
    let lines = wrap_lines(text, width);
    if lines.is_empty() {
        return (0, 0);
    }

    if cursor == text.len() && text.ends_with('\n') {
        return (0, lines.len().saturating_sub(1));
    }

    for (row, line) in lines.iter().enumerate() {
        let is_last = row == lines.len() - 1;
        if cursor >= line.start_byte
            && (cursor < line.end_byte || (cursor == line.end_byte && is_last))
        {
            let mut col = 0;
            for seg in &line.segments {
                let seg_end = seg.byte_offset + seg.text.len();
                if cursor >= seg_end {
                    col += seg.width;
                } else if cursor >= seg.byte_offset {
                    let sub = &seg.text[..cursor - seg.byte_offset];
                    col += UnicodeWidthStr::width(sub);
                    break;
                }
            }
            if col >= width {
                return (0, row + 1);
            }
            return (col, row);
        }
    }

    let last = lines.last().unwrap();
    if last.width >= width {
        (0, lines.len())
    } else {
        (last.width, lines.len() - 1)
    }
}

pub fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

pub fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .graphemes(true)
        .next()
        .map(|value| cursor + value.len())
        .unwrap_or(cursor)
}

pub fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

pub fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map(|index| cursor + index)
        .unwrap_or(text.len())
}

pub fn truncate_to_width(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let size = UnicodeWidthStr::width(grapheme);
        if used + size > width - 1 {
            break;
        }
        result.push_str(grapheme);
        used += size;
    }
    result.push('…');
    result
}

pub fn join_sides(left: &str, right: &str, width: usize) -> String {
    let right = truncate_to_width(right, width / 2);
    let right_width = UnicodeWidthStr::width(right.as_str());
    let left = truncate_to_width(left, width.saturating_sub(right_width + 2));
    let padding = width.saturating_sub(UnicodeWidthStr::width(left.as_str()) + right_width);
    format!("{left}{}{right}", " ".repeat(padding))
}

pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    wrap_lines(text, width)
        .into_iter()
        .map(|l| l.text())
        .collect()
}
