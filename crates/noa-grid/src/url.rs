//! Plain-text `https?://` URL detection within a single row, for the
//! Cmd+hover auto-link feature (`noa-app` mouse hover path). Operates on one
//! [`Row`] at a time — wrapped multi-row URLs (a URL that soft-wraps across
//! the `Row::wrapped` boundary) are out of scope for v1 and are not
//! detected.
//!
//! Surrounding brackets and prose punctuation delimit links; balanced ASCII
//! brackets inside URL components are retained. Letters without a delimiter
//! remain part of the URL, including Japanese path and domain characters.
//!
//! Char-index-to-cell-x mapping follows the same WIDE_SPACER-skipping
//! approach as [`crate::search::compute_matches`].

use crate::cell::Row;
use noa_core::CellAttrs;

/// A detected URL run, in both cell-x (for underline rendering) and text
/// (for opening) form.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UrlMatch {
    pub start_x: u16,
    pub end_x: u16,
    pub uri: String,
}

const URL_SCHEMES: [&str; 2] = ["https://", "http://"];
const TRAILING_PUNCTUATION: [char; 6] = ['.', ',', ';', ':', '!', '?'];
const RUN_BOUNDARIES: &str = "\"<>`|｜（）「」『』【】［］｛｝〈〉《》“”‘’、。！？：；…‥→←⇒⇐↔";

fn is_run_boundary(c: char) -> bool {
    c.is_whitespace() || RUN_BOUNDARIES.contains(c)
}

fn scheme_len_at(chars: &[char], start: usize) -> Option<usize> {
    URL_SCHEMES.iter().find_map(|scheme| {
        let candidate = chars.get(start..start + scheme.len())?;
        candidate
            .iter()
            .zip(scheme.chars())
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(&expected))
            .then_some(scheme.len())
    })
}

fn starts_quoted_item_or_call(chars: &[char]) -> bool {
    if chars
        .first()
        .is_some_and(|c| TRAILING_PUNCTUATION.contains(c))
        && chars.iter().find(|&&c| !TRAILING_PUNCTUATION.contains(&c)) == Some(&'\'')
    {
        return true;
    }
    if !matches!(chars, ['.', c, ..] if c.is_alphabetic() || *c == '_') {
        return false;
    }
    chars[1..]
        .iter()
        .skip_while(|&&c| c.is_alphanumeric() || matches!(c, '_' | '.'))
        .find(|c| !c.is_whitespace())
        == Some(&'(')
}

// A bare extension can be URL data; operators or indexing after a member
// identify a following expression instead.
fn starts_property_expression(chars: &[char]) -> bool {
    if chars.starts_with(&['?', '.']) {
        return true;
    }
    if !matches!(chars, ['.', c, ..] if c.is_alphabetic() || matches!(c, '_' | '$')) {
        return false;
    }
    let member = &chars[1..];
    let name_len = member
        .iter()
        .take_while(|&&c| c.is_alphanumeric() || matches!(c, '_' | '$' | '.'))
        .count();
    let tail = &member[name_len..];
    match tail.iter().find(|c| !c.is_whitespace()) {
        Some('?') => starts_conditional_expression(tail),
        Some(
            '+' | '-' | '*' | '/' | '%' | '&' | '|' | '^' | '=' | '!' | '[' | '(' | ',' | ')' | ']'
            | '}' | ';' | ':',
        ) => true,
        _ => false,
    }
}

fn starts_conditional_expression(chars: &[char]) -> bool {
    let mut tail = chars
        .iter()
        .copied()
        .filter(|c| !c.is_whitespace())
        .peekable();
    if tail.next() != Some('?') {
        return false;
    }
    if matches!(tail.peek(), Some('.' | '?' | '\'' | '"' | '`')) {
        return true;
    }
    let mut previous = None;
    while let Some(c) = tail.next() {
        match c {
            '\'' => return false,
            ':' => return true,
            // A single assignment can identify URL query data, whereas
            // comparison operators belong to the ternary's true branch.
            '=' if !matches!(previous, Some('=' | '!' | '<' | '>'))
                && tail.peek() != Some(&'=') =>
            {
                return false;
            }
            _ => {}
        }
        previous = Some(c);
    }
    false
}

fn starts_keyword_expression(chars: &[char], sql_quotes: bool) -> bool {
    let mut following = chars;
    loop {
        let word_end = following
            .iter()
            .take_while(|c| c.is_alphanumeric() || **c == '_')
            .count();
        if !is_operator_keyword(&following[..word_end], sql_quotes) {
            return false;
        }
        if following[word_end..].starts_with(&['/', '*']) {
            return true;
        }
        match following.get(word_end) {
            Some('\'') => {
                // A quoted operand ends at a token boundary or another word
                // operator, never at a quote borrowed from a later call.
                let Some(end) = single_quote_end(following, word_end, true) else {
                    return false;
                };
                following = &following[end + 1..];
                if following
                    .first()
                    .is_none_or(|next| !next.is_alphanumeric() && !matches!(next, '_' | '\''))
                {
                    return true;
                }
            }
            Some(c) => return c.is_whitespace() || matches!(c, '(' | '[' | '{' | '+' | '-' | '~'),
            None => return false,
        }
    }
}

fn single_quote_end(chars: &[char], start: usize, backslash_escapes: bool) -> Option<usize> {
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate().skip(start + 1) {
        if escaped {
            escaped = false;
        } else if backslash_escapes && c == '\\' {
            escaped = true;
        } else if c == '\'' {
            return Some(i);
        }
    }
    None
}

fn is_operator_keyword(word: &[char], sql_quotes: bool) -> bool {
    [
        "in",
        "and",
        "or",
        "if",
        "else",
        "is",
        "not",
        "for",
        "as",
        "satisfies",
        "instanceof",
    ]
    .into_iter()
    .chain(sql_quotes.then_some("like"))
    .chain(sql_quotes.then_some("ilike"))
    .chain(sql_quotes.then_some("between"))
    .chain(sql_quotes.then_some("glob"))
    .any(|keyword| {
        word.len() == keyword.len()
            && word.iter().zip(keyword.chars()).all(|(&actual, expected)| {
                if sql_quotes {
                    actual.eq_ignore_ascii_case(&expected)
                } else {
                    actual == expected
                }
            })
    })
}

fn is_literal_prefix_keyword(word: &[char], sql_quotes: bool) -> bool {
    is_operator_keyword(word, sql_quotes)
        || (!sql_quotes
            && [
                "return", "yield", "raise", "assert", "await", "case", "throw",
            ]
            .iter()
            .any(|keyword| word.iter().copied().eq(keyword.chars())))
}

fn open_single_quote_width(chars: &[char], end: usize, sql_quotes: bool) -> Option<usize> {
    // Track outer delimiters separately from single-quoted messages inside
    // log strings, so inner quotes cannot change the outer string's state.
    let mut quote = None;
    let mut nested_single_quote = false;
    let mut block_comment = false;
    let mut i = 0;
    while i < end {
        let c = chars[i];
        if block_comment {
            if chars[i..end].starts_with(&['*', '/']) {
                block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if c == '\\' {
            i += 2;
            continue;
        }
        // Use the same URL boundaries as detection, including quotes adjacent
        // to word operators. Reclassifying them as contractions shifts every
        // later string on this row.
        if let Some(scheme_len) = scheme_len_at(chars, i) {
            let single_width = match quote {
                Some(('\'', width)) => Some(width),
                _ if nested_single_quote => Some(1),
                _ => None,
            };
            i = url_end(chars, i, scheme_len, single_width, sql_quotes);
            if nested_single_quote && chars.get(i) == Some(&'\'') {
                nested_single_quote = false;
                i += 1;
            } else if let Some((delimiter, width)) = quote {
                if chars[i..].starts_with(&[delimiter; 3][..width]) {
                    quote = None;
                    nested_single_quote = false;
                    i += width;
                }
            } else if chars.get(i) == Some(&'\'') {
                i += 1;
            }
            continue;
        }
        if let Some((delimiter, width)) = quote {
            // Logs can contain shell strings with prose before the URL.
            // A possessive apostrophe does not open an inner string.
            if delimiter != '\'' && c == '\'' && !is_word_apostrophe(chars, i) {
                if nested_single_quote {
                    nested_single_quote = false;
                } else if i == 0 || !chars[i - 1].is_alphanumeric() {
                    nested_single_quote = true;
                }
            }
            if c != delimiter || !chars[i..end].starts_with(&[delimiter; 3][..width]) {
                i += 1;
                continue;
            }
            // Preserve contractions/apostrophes in single-quoted messages.
            if delimiter != '\''
                || width > 1
                || !is_word_apostrophe(chars, i)
                || starts_keyword_expression(&chars[i + 1..], sql_quotes)
            {
                quote = None;
                nested_single_quote = false;
            }
            i += width;
            continue;
        }
        if chars[i..end].starts_with(&['/', '*']) {
            block_comment = true;
            i += 2;
            continue;
        }
        if matches!(c, '\'' | '"' | '`') {
            // Contractions in a message do not open or close its quotes.
            let string_prefix = string_prefix_len(chars, i) > 0;
            let word_start = chars[..i]
                .iter()
                .rposition(|c| !c.is_alphanumeric() && *c != '_')
                .map_or(0, |offset| offset + 1);
            if c != '\''
                || !is_word_apostrophe(chars, i)
                || string_prefix
                || is_literal_prefix_keyword(&chars[word_start..i], sql_quotes)
            {
                // SQL uses doubled quote escapes. Python triples need a
                // matching delimiter on this row to establish triple context.
                let triple = [c; 3];
                let width = if !sql_quotes
                    && c != '`'
                    && chars[i..end].starts_with(&triple)
                    && chars[i + 3..].windows(3).any(|window| window == triple)
                {
                    3
                } else {
                    1
                };
                quote = Some((c, width));
                i += width;
                continue;
            }
        }
        i += 1;
    }
    match quote {
        Some(('\'', width)) => Some(width),
        _ if nested_single_quote => Some(1),
        _ => None,
    }
}

fn starts_sql_statement(chars: &[char]) -> bool {
    // Terminal rows may prefix a statement with a prompt or log metadata.
    // Stop before literal/URL data so keywords inside it cannot select SQL.
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\'' || scheme_len_at(chars, i).is_some() {
            return false;
        }
        if chars[i..].starts_with(&['/', '*']) {
            let Some(end) = chars[i + 2..].windows(2).position(|w| w == ['*', '/']) else {
                return false;
            };
            i += end + 4;
            continue;
        }
        if chars[i..].starts_with(&['-', '-']) {
            return false;
        }
        if chars[i].is_alphabetic() || chars[i] == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            if (start == 0 || chars[start - 1] != '.')
                && chars
                    .get(i)
                    .is_some_and(|c| c.is_whitespace() || matches!(c, '\'' | '(' | '/'))
                && is_sql_statement_start(&chars[start..])
            {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

fn is_sql_statement_start(chars: &[char]) -> bool {
    let start = chars
        .iter()
        .position(|c| !c.is_whitespace())
        .unwrap_or(chars.len());
    let len = chars[start..]
        .iter()
        .take_while(|c| c.is_alphanumeric() || **c == '_')
        .count();
    // A SQL keyword used as a source identifier does not select SQL quoting.
    if matches!(
        chars[start + len..].iter().find(|c| !c.is_whitespace()),
        Some('=' | ':' | '.')
    ) {
        return false;
    }
    [
        "SELECT", "VALUES", "WITH", "INSERT", "UPDATE", "DELETE", "MERGE", "CREATE", "ALTER",
        "DROP", "EXPLAIN",
    ]
    .iter()
    .any(|keyword| {
        len == keyword.len()
            && chars[start..start + len]
                .iter()
                .zip(keyword.chars())
                .all(|(actual, expected)| actual.eq_ignore_ascii_case(&expected))
    })
}

fn is_word_apostrophe(chars: &[char], i: usize) -> bool {
    i > 0 && chars[i - 1].is_alphanumeric() && chars.get(i + 1).is_some_and(|c| c.is_alphanumeric())
}

fn string_prefix_len(chars: &[char], quote: usize) -> usize {
    let start = chars[..quote]
        .iter()
        .rposition(|&c| !c.is_alphanumeric() && c != '_')
        .map_or(0, |offset| offset + 1);
    if matches!(
        &chars[start..quote],
        ['r' | 'R' | 'f' | 'F' | 'b' | 'B' | 'u' | 'U' | 'e' | 'E' | 'n' | 'N']
            | ['r' | 'R', 'f' | 'F' | 'b' | 'B']
            | ['f' | 'F' | 'b' | 'B', 'r' | 'R']
    ) {
        quote - start
    } else {
        0
    }
}

fn ends_with_expression_operator(chars: &[char]) -> bool {
    matches!(
        chars.iter().rev().find(|c| !c.is_whitespace()),
        Some('+' | '-' | '*' | '/' | '%' | '&' | '|' | '^' | '=' | '!' | '?' | ':')
    )
}

// In `value+'text'` or `value+f'text'` the next quote opens a new literal.
// A URL value ending in `+` can still close before an expression/delimiter.
fn opens_expression_literal(chars: &[char], quote: usize) -> bool {
    let expression_end = quote - string_prefix_len(chars, quote);
    ends_with_expression_operator(&chars[..expression_end])
        && chars
            .get(quote + 1)
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '\''))
}

fn starts_literal_url_parameters(chars: &[char]) -> bool {
    let Some(parameters) = chars.strip_prefix(&[';']) else {
        return false;
    };
    let end = parameters
        .iter()
        .position(|&c| c == '\'' || is_run_boundary(c))
        .unwrap_or(parameters.len());
    if parameters.get(end) == Some(&'\'') {
        // Shell fragments can begin with whitespace or punctuation. Check the
        // whole fragment, without borrowing a nested call's opening quote.
        if let Some(closing) = single_quote_end(parameters, end, false) {
            let body = &parameters[end + 1..closing];
            let expression_prefix = body.iter().find(|c| !c.is_whitespace()).is_some_and(|c| {
                matches!(
                    c,
                    '+' | '-' | '*' | '/' | '%' | '.' | '[' | '(' | ')' | ']' | '}'
                )
            });
            let mut brackets = Vec::new();
            if expression_prefix {
                for &c in body {
                    match c {
                        '(' => brackets.push(')'),
                        '[' => brackets.push(']'),
                        '{' => brackets.push('}'),
                        ')' | ']' | '}' => {
                            brackets.pop();
                        }
                        _ => {}
                    }
                }
            }
            let closes_literal =
                single_quote_end(parameters, closing, true).is_some_and(|argument_end| {
                    parameters
                        .get(argument_end + 1)
                        .is_none_or(|next| !next.is_alphanumeric() && !matches!(next, '_' | '\''))
                });
            // An operator at the end of a shell fragment does not make its
            // closing quote the start of a literal in a later expression.
            let following_expression = expression_prefix
                && closes_literal
                && (!brackets.is_empty() || opens_expression_literal(parameters, closing));
            if !following_expression {
                return false;
            }
        }
    }
    parameters[..end].split(|&c| c == ';').all(|parameter| {
        !parameter
            .iter()
            .any(|c| matches!(c, '(' | ')' | '[' | ']' | '{' | '}'))
            && parameter
                .iter()
                .position(|&c| c == '=')
                .is_some_and(|equals| equals > 0 && equals + 1 < parameter.len())
    })
}

fn has_closing_fragment_quote(chars: &[char]) -> bool {
    // A contraction in a comment is not the closing delimiter of a fragment.
    // Nor can an opening quote of a later token close the URL.
    chars
        .iter()
        .enumerate()
        .take_while(|(_, c)| !is_run_boundary(**c))
        .find(|(i, c)| **c == '\'' && !is_word_apostrophe(chars, *i))
        .is_some_and(|(i, _)| {
            chars
                .get(i + 1)
                .is_none_or(|c| !c.is_alphanumeric() && !matches!(c, '_' | '\''))
        })
}

/// Accept an internal apostrophe only when URL data continues toward another
/// quote. A closing quote is consumed immediately, before inspecting any
/// quotes in subsequent arguments, expressions, or statements.
fn is_internal_quoted_apostrophe(
    chars: &[char],
    end: usize,
    brackets: &[char],
    sql_quotes: bool,
) -> bool {
    let following = &chars[end + 1..];
    // Python allows adjacent prefixed literals. Exclude SQL's E/N prefixes
    // here so apostrophes in URL data such as rock'n'roll remain intact.
    let starts_prefixed_literal = matches!(
        following.first(),
        Some('r' | 'R' | 'f' | 'F' | 'b' | 'B' | 'u' | 'U')
    ) && following
        .iter()
        .take(3)
        .position(|&c| matches!(c, '\'' | '"'))
        .is_some_and(|quote| string_prefix_len(following, quote) == quote);
    let has_later_quote = following
        .iter()
        .take_while(|&&c| !is_run_boundary(c))
        .any(|&c| c == '\'');
    if !has_later_quote
        || starts_prefixed_literal
        // SQL permits bare aliases after a string. Identifier tokens here
        // belong to the statement, even without separating whitespace.
        || (sql_quotes && following.first().is_some_and(|c| c.is_alphabetic() || *c == '_'))
        || following.starts_with(&['.', '.'])
        || starts_quoted_item_or_call(following)
        || starts_property_expression(following)
        || starts_conditional_expression(following)
        || starts_keyword_expression(following, sql_quotes)
    {
        return false;
    }
    let mut closing_len = 0;
    for &opening in brackets.iter().rev() {
        let closing = match opening {
            '(' => ')',
            '[' => ']',
            _ => '}',
        };
        if following.get(closing_len) != Some(&closing) {
            break;
        }
        closing_len += 1;
    }
    if closing_len > 0 {
        let tail = &following[closing_len..];
        // A caller's closing bracket can also balance a bracket in the URL.
        // Do not borrow the opening quote of a later call as the URL's close.
        let mut nested = Vec::new();
        let mut closing_quote = false;
        for (i, &c) in tail.iter().enumerate() {
            if is_run_boundary(c) {
                break;
            }
            match c {
                '\'' if nested.is_empty() => {
                    closing_quote = !opens_expression_literal(tail, i);
                    break;
                }
                '(' | '[' | '{' => nested.push(c),
                ')' | ']' | '}' => {
                    let opening = match c {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    if nested.pop() != Some(opening) {
                        return false;
                    }
                }
                _ => {}
            }
        }
        if !closing_quote {
            return false;
        }
        return !starts_quoted_item_or_call(tail)
            && !tail.starts_with(&['.', '.'])
            && !starts_property_expression(tail)
            && tail.first().is_some_and(|c| {
                c.is_alphanumeric()
                    || matches!(c, '\'' | '_' | '.' | '/' | '?')
                    || (*c == '#' && has_closing_fragment_quote(tail))
            });
    }
    match following.first() {
        Some(c) if c.is_alphanumeric() || *c == '_' => true,
        Some('.' | '?') => true,
        Some('#') => has_closing_fragment_quote(following),
        Some(';') => starts_literal_url_parameters(following),
        _ => false,
    }
}

fn starts_shell_word_concatenation(chars: &[char], start: usize, end: usize) -> bool {
    if start < 2 || chars[start - 1] != '\'' || !chars[start - 2].is_whitespace() {
        return false;
    }
    // A bare command and its options identify a shell argument. Keep this
    // separate from quoted prose and expressions, where rock'n'roll is data.
    // A shell prompt (e.g. `user@host:~$ `) decorates the line, not the
    // command, so only the text after its terminator is checked.
    let line = &chars[..start - 1];
    let command_start = line
        .windows(2)
        .rposition(|pair| {
            matches!(pair[0], '$' | '%' | '#' | '>' | '❯' | '➜' | 'λ' | '»')
                && pair[1].is_whitespace()
        })
        .map_or(0, |i| i + 2);
    let command = &line[command_start..];
    if !command.iter().any(|c| c.is_alphanumeric())
        || !command.iter().all(|c| {
            c.is_alphanumeric() || c.is_whitespace() || matches!(c, '_' | '-' | '.' | '/' | '=')
        })
    {
        return false;
    }
    let following = &chars[end + 1..];
    let word_end = following
        .iter()
        .take_while(|c| c.is_alphanumeric() || **c == '_')
        .count();
    word_end > 0
        && following.get(word_end) == Some(&'\'')
        // A quote before a shell operator closes the URL; a later statement's
        // quote cannot turn it into the opening of a concatenated fragment.
        && !matches!(following.get(word_end + 1), Some(';' | '&' | '|' | '<' | '>'))
        && single_quote_end(following, word_end, false).is_some()
}

/// Unmatched closing brackets end a link even when prose follows them.
/// Balanced brackets in paths and IPv6 authorities belong to the URL.
fn url_end(
    chars: &[char],
    start: usize,
    scheme_len: usize,
    single_quote_width: Option<usize>,
    sql_quotes: bool,
) -> usize {
    // A directly quoted URL can be embedded in another string, as in JSON
    // command logs. Its own delimiter still bounds the clickable URI.
    let single_quote_width =
        single_quote_width.or_else(|| (start > 0 && chars[start - 1] == '\'').then_some(1));
    let authority_start = start + scheme_len;
    let userinfo_end = chars[authority_start..]
        .iter()
        .take_while(|&&c| !is_run_boundary(c) && !matches!(c, '/' | '?' | '#'))
        .position(|&c| c == '@')
        .map(|offset| authority_start + offset);
    let host_start = userinfo_end.map_or(authority_start, |end| end + 1);
    let mut brackets = Vec::new();
    let mut in_component = false;
    let mut in_query_or_fragment = false;
    let mut end = authority_start;

    while let Some(&c) = chars.get(end) {
        let in_userinfo = userinfo_end.is_some_and(|last| end < last);
        if is_run_boundary(c) {
            break;
        }
        if c == '\'' {
            if single_quote_width == Some(3) {
                if chars[end..].starts_with(&['\''; 3]) {
                    break;
                }
            } else {
                if single_quote_width == Some(1)
                    && (starts_shell_word_concatenation(chars, start, end)
                        || !is_internal_quoted_apostrophe(chars, end, &brackets, sql_quotes))
                {
                    break;
                }
                // Punctuation before a delimiter closes a quoted link, but an
                // apostrophe before URL data (e.g. name'.txt) stays in the URL.
                let following = chars[end + 1..]
                    .iter()
                    .find(|&&next| !TRAILING_PUNCTUATION.contains(&next));
                if !in_component
                    || (brackets.is_empty() && starts_quoted_item_or_call(&chars[end + 1..]))
                    || following.is_none_or(|&next| {
                        is_run_boundary(next)
                            || match next {
                                ')' => brackets.last() != Some(&'('),
                                ']' => brackets.last() != Some(&'['),
                                '}' => brackets.last() != Some(&'{'),
                                _ => false,
                            }
                    })
                {
                    break;
                }
            }
        }
        if !in_userinfo
            && !in_query_or_fragment
            && matches!(c, ',' | ';')
            && scheme_len_at(chars, end + 1).is_some()
        {
            break;
        }
        if !in_userinfo && !in_component && brackets.is_empty() {
            if matches!(c, ',' | ';')
                || (c == ':'
                    && chars.get(end + 1).is_none_or(|next| {
                        !next.is_ascii_digit() && !matches!(next, '/' | '?' | '#')
                    }))
            {
                break;
            }
            // A path/query/fragment needs an authority before it.
            if end == authority_start && matches!(c, '/' | '?' | '#') {
                break;
            }
        }
        match c {
            '(' | '[' | '{' => {
                if !(in_userinfo || in_component || c == '[' && end == host_start) {
                    break;
                }
                brackets.push(c);
            }
            ')' | ']' | '}' => {
                let opening = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if brackets.last() != Some(&opening) {
                    break;
                }
                brackets.pop();
            }
            '/' | '?' | '#' => {
                in_component = true;
                in_query_or_fragment |= matches!(c, '?' | '#');
            }
            _ => {}
        }
        end += 1;
    }

    // Query/fragment punctuation can be data (including signed parameters).
    // Keep the sentence-punctuation heuristic for hosts and paths only.
    if !in_query_or_fragment {
        while end > authority_start && TRAILING_PUNCTUATION.contains(&chars[end - 1]) {
            end -= 1;
        }
    }
    end
}

/// Find the `https?://` run covering `column` in `row`, if any.
pub fn detect_url_at_column(row: &Row, column: u16) -> Option<UrlMatch> {
    let mut text = String::new();
    let mut cell_x = Vec::new();
    for (x, cell) in row.cells.iter().enumerate() {
        if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
            continue;
        }
        cell.push_text_to(&mut text);
        cell_x.extend(std::iter::repeat_n(x as u16, cell.text_chars().count()));
    }
    let chars: Vec<char> = text.chars().collect();
    let sql_quotes = starts_sql_statement(&chars);

    // Visit URLs in text order and skip their contents, so a redirect URL
    // embedded in a query cannot override the surrounding link on hover.
    let mut char_start = 0;
    while char_start < chars.len() {
        let Some(scheme_len) = scheme_len_at(&chars, char_start) else {
            char_start += 1;
            continue;
        };
        let quote_width = open_single_quote_width(&chars, char_start, sql_quotes);
        let end = url_end(&chars, char_start, scheme_len, quote_width, sql_quotes);
        if end > char_start + scheme_len {
            let start_x = cell_x[char_start];
            let last_x = cell_x[end - 1];
            let end_x =
                last_x + u16::from(row.cells[last_x as usize].attrs.contains(CellAttrs::WIDE));
            if start_x <= column && column <= end_x {
                return Some(UrlMatch {
                    start_x,
                    end_x,
                    uri: chars[char_start..end].iter().collect(),
                });
            }
        }
        char_start = end;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;

    fn row_from_str(text: &str) -> Row {
        let cols = text.chars().count() as u16;
        let mut row = Row::new(cols.max(1));
        for (x, ch) in text.chars().enumerate() {
            row.cells[x] = Cell {
                ch,
                ..Cell::default()
            };
        }
        row
    }

    #[test]
    fn detects_plain_https_url() {
        let row = row_from_str("see https://example.com/path here");
        let m = detect_url_at_column(&row, 10).expect("column inside the URL should match");
        assert_eq!(m.uri, "https://example.com/path");
        assert_eq!(row.cells[m.start_x as usize].ch, 'h');
        assert_eq!(row.cells[m.end_x as usize].ch, 'h'); // last char of "path"
    }

    #[test]
    fn detects_plain_http_url_at_start_of_row() {
        let row = row_from_str("http://x.io rest");
        let m = detect_url_at_column(&row, 0).expect("column 0 is the scheme's 'h'");
        assert_eq!(m.uri, "http://x.io");
        assert_eq!(m.start_x, 0);
    }

    #[test]
    fn trims_trailing_sentence_punctuation() {
        let row = row_from_str("go to https://example.com/page, now.");
        let m = detect_url_at_column(&row, 10).unwrap();
        assert_eq!(m.uri, "https://example.com/page");
    }

    #[test]
    fn keeps_balanced_trailing_paren_but_trims_unbalanced_one() {
        let row = row_from_str("(https://en.wikipedia.org/wiki/Rust_(lang)))");
        let m = detect_url_at_column(&row, 6).unwrap();
        assert_eq!(m.uri, "https://en.wikipedia.org/wiki/Rust_(lang)");
    }

    #[test]
    fn maps_columns_correctly_past_a_wide_cjk_run() {
        // "見" occupies columns 0-1 (WIDE + WIDE_SPACER); a space is at
        // column 2; the URL starts at column 3.
        let mut row = Row::new(20);
        row.cells[0] = Cell {
            ch: '見',
            attrs: CellAttrs::WIDE,
            ..Cell::default()
        };
        row.cells[1] = Cell {
            ch: ' ',
            attrs: CellAttrs::WIDE_SPACER,
            ..Cell::default()
        };
        for (i, ch) in " http://x.io".chars().enumerate() {
            row.cells[2 + i] = Cell {
                ch,
                ..Cell::default()
            };
        }

        assert_eq!(
            detect_url_at_column(&row, 0),
            None,
            "the CJK cell itself is not part of the URL"
        );
        let m = detect_url_at_column(&row, 3).expect("column 3 is the scheme's 'h'");
        assert_eq!(m.uri, "http://x.io");
        assert_eq!(m.start_x, 3);
        assert_eq!(m.end_x, 13);
    }

    #[test]
    fn no_match_on_plain_text_row() {
        let row = row_from_str("no links on this row at all");
        assert_eq!(detect_url_at_column(&row, 5), None);
    }

    #[test]
    fn no_match_when_column_is_outside_the_url_run() {
        let row = row_from_str("prefix https://example.com suffix words");
        assert_eq!(detect_url_at_column(&row, 0), None);
        assert_eq!(detect_url_at_column(&row, row.cells.len() as u16 - 1), None);
    }

    fn assert_url(text: &str, expected: &str) {
        assert_urls(text, &[expected]);
    }

    fn assert_urls(text: &str, expected: &[&str]) {
        let mut terminal = crate::Terminal::new(noa_core::GridSize::new(256, 2));
        let mut stream = noa_vt::Stream::new();
        let mut byte_end = 0;
        let mut spans = Vec::new();
        for &uri in expected {
            let byte_start = byte_end + text[byte_end..].find(uri).expect("literal URI in fixture");
            stream.feed(&text.as_bytes()[byte_end..byte_start], &mut terminal);
            let start = terminal.primary.cursor.x;
            stream.feed(uri.as_bytes(), &mut terminal);
            spans.push((start, terminal.primary.cursor.x, uri));
            byte_end = byte_start + uri.len();
        }
        stream.feed(&text.as_bytes()[byte_end..], &mut terminal);
        assert_eq!(terminal.primary.cursor.y, 0, "fixture must fit on one row");
        let row = &terminal.primary.grid[0];
        for column in 0..256 {
            let found = detect_url_at_column(row, column);
            if let Some(&(start, end, uri)) = spans
                .iter()
                .find(|&&(start, end, _)| (start..end).contains(&column))
            {
                let found = found.unwrap_or_else(|| panic!("{text:?}, column {column}"));
                assert_eq!(found.uri, uri, "{text:?}, column {column}");
                assert_eq!((found.start_x, found.end_x), (start, end - 1));
            } else {
                assert_eq!(found, None, "{text:?}, column {column}");
            }
        }
    }

    #[test]
    fn preserves_quote_context_after_unquoted_url_data() {
        for first in ["https://a.example/*", "https://a.example/path'/next"] {
            for second in ["https://b.example/path", "https://b.example/日本語"] {
                assert_urls(&format!("{first} '{second}'+suffix('x')"), &[first, second]);
            }
        }
    }

    #[test]
    fn separates_quoted_urls_across_contexts_and_following_expressions() {
        for uri in [
            "https://example.com/path",
            "https://example.com/日本語",
            "https://example.com/name%27s",
            "https://example.com/cafe\u{301}",
            "https://example.com/it's",
            "https://example.com/name'.txt",
            "https://example.com/path';param=value+",
            "https://user:pass@[::1]:/path?q=1#fragment",
        ] {
            for suffix in [
                "+suffix('x')",
                ".length+'suffix'",
                ".length??'fallback'",
                ".length?'yes':'no'",
                ".length?value:'fallback'",
                ".length ?? 'fallback'",
                ".length ? 'yes' : 'no'",
            ] {
                for delimiter in ["'", "\"", "`", "'''", "\"\"\""] {
                    let expression = format!("{delimiter}{uri}{delimiter}{suffix}");
                    for text in [
                        format!("const u={expression};"),
                        format!("consume(\"users' data\",{expression})"),
                        format!("consume(`users' data`,{expression})"),
                        format!("/* users' data */ const u={expression};"),
                        format!("print('''users' data''',{expression})"),
                        format!("print(\"\"\"users' data\"\"\",{expression})"),
                    ] {
                        assert_url(&text, uri);
                    }
                }
            }
        }
    }

    #[test]
    fn excludes_statement_literals_and_calls_after_matching_brackets() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            for operator in ["+", "-", "*", "/", "%", "??"] {
                assert_url(&format!("url='{uri}';result=value{operator}'suffix'"), uri);
            }
        }
        for (opening, uri, closing) in [
            ("len(", "https://example.com/?q=(foo", ")"),
            ("lookup[", "https://example.com/?q=[foo", "]"),
            ("{", "https://example.com/?q={foo", "}"),
        ] {
            for suffix in [
                "/measure('x')",
                "/measure(x)+'suffix'",
                ".length??'fallback'",
            ] {
                assert_url(&format!("{opening}'{uri}'{closing}{suffix}"), uri);
            }
        }
        for uri in [
            "https://example.com/(James')/tail",
            "https://example.com/(James')/fn(foo)",
            "https://example.com/(James')/fn('foo')",
            "https://example.com/(James')/tail+",
            "https://example.com/name'.txt?q=value",
            "https://example.com/path';param=value+",
        ] {
            assert_url(uri, uri);
            assert_url(&format!("'{uri}'"), uri);
            assert_url(&format!("'{uri}'+suffix('x')"), uri);
        }
    }

    #[test]
    fn stops_at_surrounding_decoration_and_following_text() {
        for text in [
            "(https://example.com)です",
            "（https://example.com）です",
            "「https://example.com」",
            "『https://example.com』",
            "【https://example.com】",
            "[https://example.com]next",
            "{https://example.com}next",
            "`https://example.com`",
            "“https://example.com”",
            "‘https://example.com’",
            "https://example.com。続き",
            "https://example.com、続き",
            "https://example.com…",
            "https://example.com|next",
            "https://example.com｜次",
            "https://example.com→次",
            "[説明](https://example.com)を参照",
            "https://example.com(補足)",
            "https://example.com,続き",
            "https://example.com:説明",
            "https://example.com;next",
        ] {
            assert_url(text, "https://example.com");
        }
    }

    #[test]
    fn separates_adjacent_urls_regardless_of_scheme_order() {
        for (first, second) in [
            ("https://a.example", "https://b.example"),
            ("http://a.example", "https://b.example"),
            ("https://a.example", "http://b.example"),
            ("https://a.example/path", "http://b.example/path"),
        ] {
            for separator in [",", ";", "、", ")("] {
                let text = format!("{first}{separator}{second}");
                assert_urls(&text, &[first, second]);
            }
        }
    }

    #[test]
    fn preserves_url_components_inside_delimiters() {
        for uri in [
            "https://example.com/wiki/Rust_(lang)",
            "https://example.com/a(b(c)d)",
            "https://example.com/a[b]{c}",
            "https://[::1]:8080/path",
            "http://localhost:8080/",
            "https://user:pass@example.com:8080/path",
            "https://user,a;b:pass@example.com",
            "https://例え.テスト/path",
            "https://example.com/%EF%BC%89",
            "https://example.com/日本語",
            "https://example.com/pathです",
            "https://example.com/it's",
            "https://example.com/search?q=what?",
            "https://example.com/search?q=hello!",
            "http://example.com/?next=https://other.example/path",
            "https://example.com/?next=http://other.example/path",
            "https://example.com/?items=a,b;c",
        ] {
            assert_url(uri, uri);
            assert_url(&format!("({uri})"), uri);
        }
        assert_url("https://example.com/a(()", "https://example.com/a(()");
        assert_url("'https://example.com/path'", "https://example.com/path");
    }

    #[test]
    fn excludes_closing_apostrophes_before_punctuation() {
        assert_url(
            "HTTPError('404 Client Error: Not Found for url: https://example.com/path')",
            "https://example.com/path",
        );
        for suffix in [
            "')", "']", "'}", "'.", "', next", "'; next", "': next", "'!", "'?", "'。", "'”",
        ] {
            assert_url(
                &format!("url: https://example.com/path{suffix}"),
                "https://example.com/path",
            );
        }
    }

    #[test]
    fn preserves_internal_apostrophes() {
        for uri in [
            "https://example.com/it's",
            "https://example.com/rock'n'roll",
            "https://example.com/?name=O'Reilly",
            "https://example.com/#it's",
            "https://example.com/name'.txt",
            "https://example.com/path'?q=value",
            "https://example.com/path';param=value",
            "https://example.com/(James')",
            "https://example.com/?filter=('value')",
        ] {
            assert_url(uri, uri);
            assert_url(&format!("HTTPError('url: {uri}')"), uri);
            assert_url(&format!("'{uri}'"), uri);
        }
    }

    #[test]
    fn separates_single_quoted_urls_without_whitespace() {
        for (first, second) in [
            ("https://a.example/path", "https://b.example/path"),
            ("https://a.example/it's", "https://b.example/O'Reilly"),
        ] {
            let text = format!("['{first}','{second}']");
            assert_urls(&text, &[first, second]);
        }
    }

    #[test]
    fn regression_shell_word_concatenation_ends_at_url_quote() {
        for command in ["curl", "wget -O output", "./fetch --url"] {
            for suffix in [
                "foo'bar'",
                "123'bar'",
                "_name'bar'",
                "foo''",
                "foo'bar baz'",
            ] {
                for prompt in ["", "$ ", "% ", "# ", "user@host:~/src$ ", "❯ ", "PS C:\\> "] {
                    assert_url(
                        &format!("{prompt}{command} 'https://example.com/path'{suffix}"),
                        "https://example.com/path",
                    );
                }
            }
        }
    }

    #[test]
    fn stops_at_outer_single_quotes_before_shell_operators() {
        for suffix in [";echo done", "&&echo done", "||echo done", ">out", "suffix"] {
            assert_url(
                &format!("curl 'https://example.com/path'{suffix}"),
                "https://example.com/path",
            );
        }
        assert_url(
            "curl 'https://example.com/it's';echo done",
            "https://example.com/it's",
        );
    }

    #[test]
    fn separates_quoted_items_after_a_url_inside_a_string() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            assert_url(
                &format!("{{'error':'Not found for url: {uri}','status':404}}"),
                uri,
            );
            assert_url(&format!("['Not found for url: {uri}','next']"), uri);
        }
    }

    #[test]
    fn ends_a_quoted_url_before_method_calls_with_quoted_arguments() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            for suffix in [
                ".format(x='x')",
                ".replace('path','next')",
                ".strip().replace('a','b')",
                ".format (x='x')",
            ] {
                assert_url(&format!("'{uri}'{suffix}"), uri);
                assert_url(&format!("'Not found for url: {uri}'{suffix}"), uri);
            }
        }
    }

    #[test]
    fn ends_a_quoted_url_before_arguments_and_expressions() {
        for uri in ["https://example.com/api", "https://example.com/it's"] {
            for value in [uri.to_string(), format!("Not found for url: {uri}")] {
                for text in [
                    format!("fetch('{value}',{{method:'POST'}})"),
                    format!("fetch('{value}',[{{method:'POST'}}])"),
                    format!("fetch('{value}',options('POST'))"),
                    format!("'{value}'+suffix('x')"),
                    format!("'{value}'-offset('x')"),
                    format!("'{value}'*factor('x')"),
                    format!("'{value}'/divisor('x')"),
                ] {
                    assert_url(&text, uri);
                }
            }
        }
    }

    #[test]
    fn preserves_apostrophes_before_operators_in_unquoted_urls() {
        for uri in [
            "https://example.com/rock'-n-roll",
            "https://example.com/path'+suffix",
            "https://example.com/path',part",
            "https://example.com/path'/next",
        ] {
            assert_url(uri, uri);
        }
    }

    #[test]
    fn ends_a_quoted_url_before_a_following_statement() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            for statement in ["print('done')", "result='done'", "result=render('done')"] {
                assert_url(&format!("url='{uri}';{statement}"), uri);
                assert_url(
                    &format!("message='Not found for url: {uri}';{statement}"),
                    uri,
                );
            }
        }
    }

    #[test]
    fn ends_a_quoted_url_before_adjacent_prefixed_literals() {
        let uri = "https://example.com/path";
        for prefix in [
            "r", "f", "u", "b", "R", "F", "U", "B", "rf", "fr", "rb", "br", "rF", "Br",
        ] {
            for delimiter in ["'", "\"", "'''", "\"\"\""] {
                assert_url(
                    &format!("url='{uri}'{prefix}{delimiter}suffix{delimiter}"),
                    uri,
                );
            }
        }
    }

    #[test]
    fn ends_a_quoted_url_before_adjacent_formatted_literals() {
        let uri = "https://example.com/path";
        assert_url(&format!("url='{uri}'f'{{suffix}}'"), uri);
    }

    #[test]
    fn recognizes_string_prefixes_before_the_opening_quote() {
        for prefix in [
            "r", "f", "b", "u", "R", "F", "B", "U", "rf", "fr", "br", "rb",
        ] {
            for uri in ["https://example.com/path", "https://example.com/it's"] {
                assert_url(&format!("{prefix}'{uri}'+suffix('x')"), uri);
                assert_url(
                    &format!("value={prefix}'Not found for url: {uri}'+suffix('x')"),
                    uri,
                );
            }
        }
        assert_url(
            "There's no quote: https://example.com/rock'-n-roll",
            "https://example.com/rock'-n-roll",
        );
    }

    #[test]
    fn ignores_apostrophes_inside_double_quoted_context() {
        for message in [
            r#""users' data""#,
            r#""users' \"data\"""#,
            r#"'users" data'"#,
        ] {
            for uri in ["https://example.com/path", "https://example.com/it's"] {
                assert_url(&format!("print({message},'{uri}'+suffix('x'))"), uri);
            }
        }
    }

    #[test]
    fn excludes_a_closing_quote_after_unclosed_url_brackets() {
        for (prefix, uri, closing) in [
            ("requests.get(", "https://example.com/?q=(foo", ")"),
            ("lookup[", "https://example.com/?q=[foo", "]"),
            ("{", "https://example.com/?q={foo", "}"),
        ] {
            for suffix in ["", "+suffix('x')", ".format(x='x')"] {
                assert_url(&format!("{prefix}'{uri}'{closing}{suffix}"), uri);
            }
        }
        for uri in [
            "https://example.com/(James')/tail",
            "https://example.com/a[b']/tail",
            "https://example.com/a{c'}/tail",
        ] {
            assert_url(&format!("'{uri}'"), uri);
        }
    }

    #[test]
    fn ends_a_quoted_url_before_ternaries_with_unquoted_true_branches() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            for branch in ["value", "object.value", "42", "resolve(value)"] {
                for value in [uri.to_string(), format!("url: {uri}")] {
                    for fallback in ["'fallback'", "fallback+suffix('x')"] {
                        assert_url(
                            &format!("const u='{value}'.length?{branch}:{fallback}"),
                            uri,
                        );
                    }
                }
            }
        }
        for uri in [
            "https://example.com/name'.txt?download",
            "https://example.com/name'.txt?q=urn:value",
            "https://example.com/name'.txt?q=https://other.example/path",
        ] {
            assert_url(&format!("'{uri}'"), uri);
        }
    }

    #[test]
    fn separates_urls_after_sql_escaped_quote_runs() {
        for preceding in ["''''", "''''''", "''''''''", "E''''", "N''''", "'O''Brien'"] {
            for uri in [
                "https://example.com/path?q=1",
                "https://example.com/日本語?q=1",
            ] {
                assert_url(&format!("SELECT {preceding}, '{uri}';"), uri);
                assert_url(&format!("SELECT {preceding}, 'url: {uri}'::text;"), uri);
                assert_url(&format!("SELECT {preceding}, '{uri}'||'suffix';"), uri);
            }
        }
        for preceding in [
            "''''''",
            "\"\"\"\"\"\"",
            "'''users' data'''",
            "\"\"\"users' data\"\"\"",
        ] {
            assert_url(
                &format!("print({preceding},'https://example.com/path?q=1'+suffix('x'))"),
                "https://example.com/path?q=1",
            );
        }
        assert_url(
            "print(''''https://example.com/path?q=1'''+suffix('x'))",
            "https://example.com/path?q=1",
        );
    }

    #[test]
    fn recognizes_sql_string_prefixes_before_urls() {
        for prefix in ["E", "e", "N", "n"] {
            for uri in ["https://example.com/path", "https://example.com/日本語"] {
                for value in [uri.to_string(), format!("url: {uri}")] {
                    assert_url(&format!("SELECT {prefix}'{value}'::text;"), uri);
                    assert_url(
                        &format!("SELECT COALESCE({prefix}'{value}'::text,'fallback');"),
                        uri,
                    );
                    assert_url(&format!("SELECT {prefix}'{value}'||suffix('x');"), uri);
                }
            }
        }
    }

    #[test]
    fn regression_property_terminators_separate_urls_from_following_code() {
        for uri in [
            "https://example.com/path",
            "https://example.com/it's",
            "https://example.com/日本語",
        ] {
            let second = "https://other.example/path";
            assert_urls(&format!("['{uri}'.length,'{second}']"), &[uri, second]);
            for text in [
                format!("consume('{uri}'.length,'next')"),
                format!("consume('{uri}'.length)+'suffix'"),
                format!("['{uri}'.length]+'suffix'"),
                format!("({{key:'{uri}'.length}})+'suffix'"),
            ] {
                assert_url(&text, uri);
            }
        }
    }

    #[test]
    fn regression_prefixed_assignments_do_not_extend_quoted_urls() {
        for prefix in [
            "f", "r", "b", "u", "F", "R", "B", "U", "rf", "fr", "br", "rb",
        ] {
            for uri in ["https://example.com/path", "https://example.com/it's"] {
                assert_url(&format!("url='{uri}';name={prefix}'value'"), uri);
                assert_url(&format!("url='{uri}';name=value+{prefix}'suffix'"), uri);
                assert_url(&format!("message='url: {uri}';name={prefix}'value'"), uri);
            }
        }
        for uri in [
            "https://example.com/path';param=f",
            "https://example.com/path';param=r",
            "https://example.com/path';param=rf",
            "https://example.com/path';param=profile",
        ] {
            assert_url(&format!("'{uri}'"), uri);
            assert_url(&format!("'{uri}'+suffix('x')"), uri);
        }
    }

    #[test]
    fn regression_independent_sql_literals_do_not_share_triple_quotes() {
        for preceding in ["''''", "E''''", "N''''", "'O''Brien'", "'''quoted'"] {
            for uri in ["https://example.com/path", "https://example.com/日本語?q=1"] {
                for value in [uri.to_string(), format!("url: {uri}")] {
                    assert_url(&format!("SELECT {preceding}, '{value}'::text, '''';"), uri);
                    assert_url(
                        &format!("  select {preceding}, '{value}'::text, '''';"),
                        uri,
                    );
                    assert_url(
                        &format!("VALUES ({preceding}, '{value}'::text, '''');"),
                        uri,
                    );
                }
            }
        }
        assert_url(
            "print('''SELECT users' data''','https://example.com/path'+suffix('x'))",
            "https://example.com/path",
        );
        assert_url(
            "values='''https://example.com/path';param=value+'''+suffix('x')",
            "https://example.com/path';param=value+",
        );
    }

    #[test]
    fn regression_property_semicolons_end_urls_before_following_statements() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            assert_url(&format!("const n='{uri}'.length;v='x'"), uri);
            let second = "https://other.example/path";
            assert_urls(
                &format!("const n='{uri}'.length;v='{second}'"),
                &[uri, second],
            );
        }
    }

    #[test]
    fn regression_shell_assignment_concatenation_stays_out_of_urls() {
        for assignment in ["FOO=bar'baz'", "foo=bar'baz'", "FOO=bar''"] {
            for uri in ["https://example.com/path", "https://example.com/it's"] {
                assert_url(&format!("curl '{uri}';{assignment}"), uri);
            }
        }
    }

    #[test]
    fn regression_python_keyword_expressions_stay_out_of_urls() {
        for keyword in ["in", "and", "or", "is"] {
            assert_url(
                &format!("['https://example.com/path'{keyword}['x']]"),
                "https://example.com/path",
            );
        }
        for expression in [
            "if['x']else'y'",
            "not in['x']",
            "is not['x']",
            "in'x'",
            "in''",
            "in'foo bar'",
            "in'foo\\'bar'",
        ] {
            assert_url(
                &format!("['https://example.com/path'{expression}]"),
                "https://example.com/path",
            );
        }
        for uri in [
            "https://example.com/it's",
            "https://example.com/rock'inside",
            "https://example.com/path'android",
            "https://example.com/path'ifconfig",
            "https://example.com/sign'in",
            "https://example.com/log'and",
        ] {
            assert_url(&format!("'{uri}'"), uri);
            assert_url(&format!("'{uri}'+suffix('x')"), uri);
            assert_url(&format!("'{uri}'+'suffix'"), uri);
            assert_url(&format!("'{uri}'+suffix('')"), uri);
        }
    }

    #[test]
    fn regression_shell_fragments_with_leading_separators_stay_out_of_urls() {
        for fragment in [
            " baz",
            " ",
            "/tmp",
            "--flag",
            "!",
            "(x)",
            "+flag",
            "\"quoted\"",
            "[x",
            "(x",
            "+f(",
            "\\",
        ] {
            assert_url(
                &format!("curl 'https://example.com/path';FOO=bar'{fragment}'"),
                "https://example.com/path",
            );
        }
        let uri = "https://example.com/path';param=value";
        for suffix in [
            "",
            "+suffix('x')",
            "+suffix('')",
            "+suffix(')')",
            "+suffix(' ')",
            "+suffix(')',x)",
            ".format(x=')')",
            "+'suffix'",
        ] {
            assert_url(&format!("'{uri}'{suffix}"), uri);
            assert_url(&format!("HTTPError('url: {uri}'){suffix}"), uri);
        }
    }

    #[test]
    fn regression_quote_rescans_preserve_each_url_boundary() {
        for expression in [
            "in['x']",
            "and['x']",
            "if['x']else'y'",
            "in'x'",
            "in''",
            "in'x'and['y']",
            "in r'x'",
        ] {
            for first in ["https://a.example/path", "https://a.example/it's"] {
                for second in ["https://b.example/path", "https://b.example/日本語"] {
                    assert_urls(
                        &format!("['{first}'{expression},'{second}'+suffix('x')]"),
                        &[first, second],
                    );
                    let third = "http://c.example/path";
                    assert_urls(
                        &format!(
                            "['{first}'{expression},'{second}'+suffix('x'),'{third}'.strip()]"
                        ),
                        &[first, second, third],
                    );
                }
            }
        }
    }

    #[test]
    fn regression_sql_keyword_boundaries_ignore_ascii_case() {
        for keyword in ["IN", "in", "In", "AND", "aNd", "OR", "IS", "LIKE", "ILIKE"] {
            assert_url(
                &format!("SELECT 'https://example.com/path'{keyword}('x');"),
                "https://example.com/path",
            );
        }
        let uri = "https://example.com/sign'IN";
        assert_url(&format!("'{uri}'+suffix('x')"), uri);
    }

    #[test]
    fn regression_language_operators_lua_end_quoted_urls() {
        let first = "https://example.com/path";
        let second = "https://other.example/path";
        assert_url(&format!("local u='{first}'..suffix('x')"), first);
        assert_urls(&format!("local u='{first}'..'{second}'"), &[first, second]);
        let uri = "https://example.com/it's";
        assert_url(&format!("local u='{uri}'..suffix('x')"), uri);
    }

    #[test]
    fn regression_language_operators_typescript_end_quoted_urls() {
        let first = "https://example.com/path";
        let second = "https://other.example/path";
        assert_urls(&format!("'{first}'as'{second}'"), &[first, second]);
        assert_url(&format!("'{first}'as const"), first);
        let uri = "https://example.com/it's";
        assert_url(&format!("'{uri}'as'string'"), uri);
    }

    #[test]
    fn regression_word_operators_before_comments_end_quoted_urls() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            assert_urls(&format!("'{uri}'in/*keys*/{{'{uri}':1}}"), &[uri, uri]);
        }
    }

    #[test]
    fn regression_type_operators_end_quoted_urls() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            assert_urls(&format!("'{uri}'satisfies('{uri}')"), &[uri, uri]);
            assert_url(&format!("'{uri}'instanceof(String)?'yes':'no'"), uri);
        }
    }

    #[test]
    fn regression_ternary_comparisons_after_properties_end_quoted_urls() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            for comparison in ["1===1", "1==1", "1!==2", "1!=2", "1<=2", "2>=1"] {
                assert_url(&format!("'{uri}'.length?{comparison}:'fallback'"), uri);
                let second = "https://other.example/path";
                assert_urls(
                    &format!("'{uri}'.length?{comparison}:'{second}'"),
                    &[uri, second],
                );
            }
        }
    }

    #[test]
    fn regression_shell_operator_fragments_allow_unquoted_suffixes() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            for fragment in ["/", "+", "-", "*", "%", "?", "=", ":"] {
                for suffix in ["bin", "bin'more'", "bin''"] {
                    assert_url(&format!("curl '{uri}';PATH=$HOME'{fragment}'{suffix}"), uri);
                }
            }
        }
    }

    #[test]
    fn regression_shell_assignment_fragments_allow_unquoted_suffixes() {
        let uri = "https://example.com/path";
        for suffix in ["qux", "_tail", "123", "'next'", "qux'more'"] {
            assert_url(&format!("curl '{uri}';FOO=bar'baz'{suffix}"), uri);
        }
    }

    #[test]
    fn regression_log_strings_preserve_nested_message_quotes() {
        let uri = "https://example.com/path";
        for message in ["Visit ", "user's link: "] {
            assert_url(
                &format!(r#"{{"command":"echo '{message}{uri}';FOO='bar'"}}"#),
                uri,
            );
            let second = "https://other.example/path";
            assert_urls(
                &format!(
                    r#"{{"command":"echo '{message}{uri}';echo 'Visit {second}';FOO='bar'"}}"#
                ),
                &[uri, second],
            );
        }
        assert_url(&format!(r#""users' data" '{uri}'+suffix('x')"#), uri);
    }

    #[test]
    fn regression_comment_apostrophes_do_not_extend_urls() {
        let uri = "https://example.com/path";
        for comment in [
            "user's comment",
            "user's",
            "'quoted'",
            "user's 'quoted'",
            "note='value'",
        ] {
            assert_url(&format!("url='{uri}'#{comment}"), uri);
        }
        for uri in [
            "https://example.com/path#section",
            "https://example.com/path#user's",
            "https://example.com/path'#section",
            "https://example.com/path'#it's",
        ] {
            assert_url(&format!("url='{uri}'"), uri);
        }
    }

    #[test]
    fn regression_sql_alias_tokens_separate_urls() {
        let first = "https://a.example/path";
        let second = "https://b.example/path";
        for alias in ["link", "other_name", "_link"] {
            assert_urls(
                &format!("SELECT '{first}'{alias},'{second}';"),
                &[first, second],
            );
        }
        for keyword in ["COLLATE", "collate"] {
            assert_url(&format!("SELECT '{first}'{keyword}'nocase';"), first);
        }
    }

    #[test]
    fn regression_direct_ternaries_separate_urls() {
        let first = "https://a.example/path";
        let second = "https://b.example/path";
        for operand in ["value", "123", "fn(x)", "'yes'"] {
            assert_urls(
                &format!("const u='{first}'?{operand}:'{second}';"),
                &[first, second],
            );
        }
        for uri in [
            "https://a.example/path'?q=value",
            "https://a.example/path'?next=https://b.example/path",
        ] {
            assert_url(&format!("'{uri}'"), uri);
        }
    }

    #[test]
    fn regression_direct_url_quotes_inside_log_strings() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            assert_url(&format!(r#"{{"command":"curl '{uri}';echo'ok'"}}"#), uri);
            let second = "https://other.example/path";
            assert_urls(
                &format!(r#"{{"command":"curl '{uri}';curl '{second}';echo'ok'"}}"#),
                &[uri, second],
            );
        }
    }

    #[test]
    fn regression_javascript_keywords_open_url_literals() {
        let uri = "https://example.com/path";
        for prefix in ["", "url: "] {
            assert_url(&format!("switch(x){{case'{prefix}{uri}':f();break;}}"), uri);
            assert_url(&format!("throw'{prefix}{uri}'+suffix('x')"), uri);
        }
    }

    #[test]
    fn regression_sql_prefixes_preserve_quote_boundaries() {
        let uri = "https://example.com/path";
        for prefix in ["psql=> ", "[INFO] ", "2026-10-16 SQL: ", "/* query */ "] {
            assert_url(&format!("{prefix}SELECT '{uri}'LIKE'x';"), uri);
        }
        let uri = "https://example.com/sign'LIKE'x";
        for prefix in ["", "[INFO] ", "/* SELECT */ "] {
            assert_url(&format!("{prefix}values='{uri}'+suffix('y')"), uri);
        }
    }

    #[test]
    fn regression_sql_aliases_and_glob_end_url_literals() {
        let uri = "https://example.com/path";
        for keyword in ["AS", "as", "GLOB", "glob"] {
            assert_url(&format!("SELECT '{uri}'{keyword}'link';"), uri);
        }
    }

    #[test]
    fn regression_sql_between_ends_a_quoted_url() {
        let uri = "https://example.com/path";
        for between in ["BETWEEN", "between", "BeTwEeN", "NOT BETWEEN"] {
            assert_url(&format!("SELECT '{uri}'{between}'a'AND'z';"), uri);
        }
        let uri = "https://example.com/path'BETWEEN'a";
        assert_url(&format!("'{uri}'+suffix('x')"), uri);
    }

    #[test]
    fn regression_python_keywords_open_url_literals() {
        for keyword in ["return", "yield", "raise", "assert", "await"] {
            for uri in ["https://example.com/path", "https://example.com/it's"] {
                assert_url(&format!("{keyword}'{uri}'+suffix('x')"), uri);
                let second = "https://other.example/path";
                assert_urls(
                    &format!("{keyword}'{uri}'+suffix('x'),'{second}'+suffix('y')"),
                    &[uri, second],
                );
            }
        }
        let uri = "https://example.com/path'return'x";
        assert_url(&format!("'{uri}'+suffix('y')"), uri);
    }

    #[test]
    fn regression_property_colons_end_a_quoted_url() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            for member in [".length", ".foo.bar", ".length "] {
                assert_url(&format!("const n=ok?'{uri}'{member}:'fallback';"), uri);
                let second = "https://other.example/path";
                assert_urls(
                    &format!("const n=ok?'{uri}'{member}:'{second}';"),
                    &[uri, second],
                );
            }
        }
    }

    #[test]
    fn ends_a_quoted_url_before_property_expressions() {
        for uri in ["https://example.com/path", "https://example.com/it's"] {
            for suffix in [
                ".length+'suffix'",
                ".length+suffix('x')",
                "?.length+'suffix'",
                "?.[key('x')]",
                ".length?.toString('x')",
                ".length[lookup('x')]",
            ] {
                assert_url(&format!("'{uri}'{suffix}"), uri);
                assert_url(&format!("'url: {uri}'{suffix}"), uri);
            }
        }
        assert_url(
            "https://example.com/name'.txt+suffix",
            "https://example.com/name'.txt+suffix",
        );
    }

    #[test]
    fn preserves_components_after_an_empty_port() {
        for uri in [
            "https://example.com:/path",
            "https://example.com:?q=1",
            "https://example.com:#fragment",
            "http://user:pass@[::1]:/path",
            "https://[::1]:?q=1",
            "https://example.com:/path?q=1#fragment",
        ] {
            assert_url(uri, uri);
            assert_url(&format!("({uri})です"), uri);
        }
    }

    #[test]
    fn detects_ipv6_hosts_after_userinfo() {
        for uri in [
            "https://user:pass@[::1]:8080/path",
            "http://user@[2001:db8::1]/path",
            "https://user:@[::1]",
        ] {
            assert_url(uri, uri);
            assert_url(&format!("({uri})です"), uri);
        }
    }

    #[test]
    fn does_not_detect_a_scheme_without_a_host() {
        for text in [
            "https://",
            "http://",
            "https:///path",
            "https://?q=1",
            "https://#x",
        ] {
            let row = row_from_str(text);
            for column in 0..row.cells.len() {
                assert_eq!(detect_url_at_column(&row, column as u16), None, "{text}");
            }
        }
    }

    #[test]
    fn recognizes_mixed_case_schemes() {
        assert_url("(HTTPS://example.com)next", "HTTPS://example.com");
        assert_url("Http://example.com", "Http://example.com");
    }

    #[test]
    fn includes_both_columns_of_a_wide_url_character() {
        let mut terminal = crate::Terminal::new(noa_core::GridSize::new(80, 2));
        noa_vt::Stream::new().feed(
            "「https://example.com/日本語」です".as_bytes(),
            &mut terminal,
        );
        let row = &terminal.primary.grid[0];
        for column in 2..28 {
            let found = detect_url_at_column(row, column).expect("both halves should match");
            assert_eq!(found.uri, "https://example.com/日本語");
            assert_eq!((found.start_x, found.end_x), (2, 27));
        }
        assert_eq!(detect_url_at_column(row, 28), None);
    }
}
