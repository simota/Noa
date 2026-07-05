#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    pub line: usize,
    pub key: String,
    pub value: Option<String>,
}

pub fn parse_directives(source: &str) -> Vec<Directive> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| parse_line(index + 1, line))
        .collect()
}

fn parse_line(line_number: usize, line: &str) -> Option<Directive> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let trimmed_start = line.trim_start();
    if trimmed_start.is_empty() || trimmed_start.starts_with('#') {
        return None;
    }

    let (key, raw_value) = line.split_once('=')?;
    let key = key.trim();
    let value = parse_value(raw_value);

    Some(Directive {
        line: line_number,
        key: key.to_string(),
        value,
    })
}

fn parse_value(raw_value: &str) -> Option<String> {
    let value = raw_value.trim();
    if value.is_empty() {
        return None;
    }

    if is_well_quoted(value) {
        return Some(value[1..value.len() - 1].to_string());
    }

    Some(value.to_string())
}

fn is_well_quoted(value: &str) -> bool {
    value.len() >= 2
        && value.starts_with('"')
        && value.ends_with('"')
        && !value[1..value.len() - 1].contains('"')
}
