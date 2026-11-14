//! Bounded glob-to-regex compilation; this module never consults the filesystem.

use crate::{Error, ErrorCode, Result};

const COMPONENT: &str = "[^/\\x00]+";
const MAX_EXPANSIONS: usize = 1_024;
const MAX_REGEX_BYTES: usize = 1_048_576;

pub(super) fn invalid(message: &str) -> Error {
    Error::new(ErrorCode::InvalidArgument, message)
}

pub(super) fn complex() -> Error {
    Error::new(
        ErrorCode::PatternTooComplex,
        "Pattern automaton or intersection budget exceeded.",
    )
}

pub(super) fn regex(pattern: &str) -> Result<String> {
    if pattern.is_empty() || pattern.starts_with('/') || pattern.contains('\0') {
        return Err(invalid("Expected a nonempty relative pattern without NUL."));
    }
    if pattern.len() > 1_024 {
        return Err(complex());
    }
    let pattern = if pattern.ends_with('/') {
        format!("{pattern}**")
    } else {
        pattern.to_owned()
    };
    let mut expanded = Vec::new();
    expand(&pattern, 0, &mut expanded)?;
    let mut alternatives = Vec::new();
    let mut size = 0;
    for branch in expanded {
        let result = branch_regex(&branch)?;
        size += result.len();
        if size > MAX_REGEX_BYTES {
            return Err(complex());
        }
        alternatives.push(result);
    }
    Ok(format!("\\A(?:{})\\z", alternatives.join("|")))
}

fn expand(pattern: &str, depth: usize, output: &mut Vec<String>) -> Result<()> {
    if depth > 32 || output.len() >= MAX_EXPANSIONS {
        return Err(complex());
    }
    let mut escaped = false;
    let mut class = false;
    let mut start = None;
    let mut nesting = 0;
    let mut pieces = Vec::new();
    let mut piece_start = 0;
    for (i, c) in pattern.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '[' && !class {
            class = true;
            continue;
        }
        if c == ']' && class {
            class = false;
            continue;
        }
        if class {
            continue;
        }
        match c {
            '{' => {
                if nesting == 0 {
                    start = Some(i);
                    piece_start = i + 1;
                }
                nesting += 1;
            }
            ',' if nesting == 1 => {
                pieces.push(&pattern[piece_start..i]);
                piece_start = i + 1;
            }
            '}' => {
                if nesting == 0 {
                    return Err(invalid("Unmatched closing brace."));
                }
                nesting -= 1;
                if nesting == 0 {
                    pieces.push(&pattern[piece_start..i]);
                    if pieces.len() < 2 || pieces.iter().any(|s| s.is_empty()) {
                        return Err(invalid(
                            "Brace alternatives require at least two nonempty branches.",
                        ));
                    }
                    let begin = start.expect("opening brace establishes start");
                    for piece in pieces {
                        let next = format!("{}{piece}{}", &pattern[..begin], &pattern[i + 1..]);
                        expand(&next, depth + 1, output)?;
                    }
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    if escaped || class || nesting != 0 {
        return Err(invalid("Unterminated escape, class or brace."));
    }
    output.push(pattern.to_owned());
    Ok(())
}

fn branch_regex(branch: &str) -> Result<String> {
    if branch.starts_with('!') {
        return Err(invalid("Pattern negation is unsupported."));
    }
    let mut components = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for c in branch.chars() {
        if c == '/' {
            if escaped {
                current.pop();
                escaped = false;
            }
            components.push(std::mem::take(&mut current));
        } else {
            current.push(c);
            escaped = c == '\\' && !escaped;
        }
    }
    components.push(current);
    if components.iter().any(String::is_empty) {
        return Err(invalid("Empty path component."));
    }
    components.dedup_by(|a, b| a == "**" && b == "**");
    let mut result = String::new();
    for (i, component) in components.iter().enumerate() {
        let last = i + 1 == components.len();
        if component == "**" {
            if !last {
                result.push_str(&format!("(?:{COMPONENT}/)*"));
            } else if i == 0 {
                result.push_str(&format!("(?:{COMPONENT}(?:/{COMPONENT})*)?"));
            } else {
                result.push_str(&format!("(?:/{COMPONENT})*"));
            }
        } else {
            result.push_str(&component_regex(component)?);
            if !(last || i + 2 == components.len() && components[i + 1] == "**") {
                result.push('/');
            }
        }
    }
    Ok(result)
}

fn literal(c: char) -> String {
    format!("\\x{{{:x}}}", c as u32)
}

fn component_regex(component: &str) -> Result<String> {
    let mut chars = component.chars().peekable();
    let mut regex = String::new();
    let mut plain = Some(String::new());
    while let Some(c) = chars.next() {
        if "@+?*!".contains(c) && chars.peek() == Some(&'(') {
            return Err(invalid("Extglob is unsupported."));
        }
        match c {
            '\\' => {
                let c = chars
                    .next()
                    .ok_or_else(|| invalid("Unterminated escape."))?;
                regex.push_str(&literal(c));
                if let Some(p) = &mut plain {
                    p.push(c);
                }
            }
            '*' => {
                if chars.peek() == Some(&'*') {
                    return Err(invalid("Recursive ** must occupy a path component."));
                }
                regex.push_str("[^/\\x00]*");
                plain = None;
            }
            '?' => {
                regex.push_str("[^/\\x00]");
                plain = None;
            }
            '[' => {
                plain = None;
                let mut class = String::new();
                if matches!(chars.peek(), Some('!' | '^')) {
                    chars.next();
                    class.push('^');
                }
                let mut count = 0;
                let mut closed = false;
                while let Some(c) = chars.next() {
                    if c == ']' {
                        closed = true;
                        break;
                    }
                    if c == '\\' {
                        let c = chars
                            .next()
                            .ok_or_else(|| invalid("Unterminated class escape."))?;
                        class.push_str(&literal(c));
                    } else if c == '-' && count > 0 && chars.peek() != Some(&']') {
                        class.push('-');
                    } else {
                        class.push_str(&literal(c));
                    }
                    count += 1;
                }
                if count == 0 || !closed {
                    return Err(invalid("Empty or unterminated character class."));
                }
                regex.push_str(&format!("[[{class}]&&[^/\\x00]]"));
            }
            ']' | '{' | '}' => return Err(invalid("Unmatched pattern delimiter.")),
            _ => {
                regex.push_str(&literal(c));
                if let Some(p) = &mut plain {
                    p.push(c);
                }
            }
        }
    }
    if matches!(plain.as_deref(), Some("." | "..")) {
        return Err(invalid("Traversal components are forbidden."));
    }
    Ok(regex)
}
