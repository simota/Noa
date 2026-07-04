//! Readable expected-vs-actual rendering for fixture failures.

/// Render a unified-diff-style, line-paired comparison. Both dump modes are
/// line-oriented with stable line positions (text mode is exactly
/// `rows + 1` lines), so pairing by index reads well.
pub fn render_diff(expected: &str, actual: &str) -> String {
    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.lines().collect();
    let mut out = vec!["--- expected".to_string(), "+++ actual".to_string()];
    for i in 0..expected.len().max(actual.len()) {
        let exp = expected.get(i);
        let act = actual.get(i);
        if exp == act {
            out.push(format!("  {}", exp.expect("index bounded by max")));
            continue;
        }
        if let Some(line) = exp {
            out.push(format!("- {line}"));
        }
        if let Some(line) = act {
            out.push(format!("+ {line}"));
        }
    }
    out.join("\n")
}
