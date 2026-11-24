//! Built-in API list prices (USD per million tokens), Anthropic first-party
//! prices as published (cached 2026-11-03). Codex models have no table: their
//! tokens count as unpriced. An estimate of what an API key would have paid,
//! not a subscription bill.

use super::types::Tokens;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

/// Cache writes are 1.25× (5 min TTL) / 2× (1 h TTL) input, reads 0.1× input.
const TABLE: [(&str, Price); 4] = [
    (
        "claude-opus-5-5",
        Price {
            input: 4.00,
            output: 20.00,
            cache_write_5m: 5.00,
            cache_write_1h: 8.00,
            cache_read: 0.20,
        },
    ),
    (
        "claude-sonnet-5-5",
        Price {
            input: 2.00,
            output: 10.00,
            cache_write_5m: 2.50,
            cache_write_1h: 4.00,
            cache_read: 0.10,
        },
    ),
    (
        // derived: input/output are the ≤ 100K-prompt rates; the cache rows
        // come from the standard multipliers.
        "claude-haiku-5-5",
        Price {
            input: 0.10,
            output: 0.50,
            cache_write_5m: 0.125, // derived
            cache_write_1h: 0.20,  // derived
            cache_read: 0.01,      // derived
        },
    ),
    (
        "claude-fable-5-2",
        Price {
            input: 8.00,
            output: 40.00,
            cache_write_5m: 10.00,
            cache_write_1h: 16.00,
            cache_read: 0.20,
        },
    ),
];

/// A `-<date>` suffix: `-20261103` or `-2026-11-03`.
fn is_date_suffix(rest: &str) -> bool {
    let Some(date) = rest.strip_prefix('-') else {
        return false;
    };
    let digits = date.bytes().filter(u8::is_ascii_digit).count();
    match date.len() {
        8 => digits == 8,
        10 => {
            digits == 8
                && date.as_bytes()[4] == b'-'
                && date.as_bytes()[7] == b'-'
                && date.bytes().filter(|b| *b == b'-').count() == 2
        }
        _ => false,
    }
}

/// The price of a model id: the exact id, or the id followed by `-<date>`.
pub fn lookup(model: &str) -> Option<&'static Price> {
    TABLE.iter().find_map(|(id, price)| {
        let rest = model.strip_prefix(id)?;
        (rest.is_empty() || is_date_suffix(rest)).then_some(price)
    })
}

/// Estimated cost of `tokens` on `model`; `None` when the model is unpriced.
/// Cache writes are priced by TTL: `cache_write_1h` at the 1 h rate, the rest
/// of `cache_write` at the 5 min rate.
pub fn cost(model: &str, tokens: &Tokens) -> Option<f64> {
    let p = lookup(model)?;
    let per = |n: u64, usd: f64| n as f64 * usd / 1_000_000.0;
    let write_1h = tokens.cache_write_1h.min(tokens.cache_write);
    Some(
        per(tokens.input, p.input)
            + per(tokens.output, p.output)
            + per(tokens.cache_read, p.cache_read)
            + per(tokens.cache_write - write_1h, p.cache_write_5m)
            + per(write_1h, p.cache_write_1h),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dated_model_ids_match() {
        let opus = lookup("claude-opus-5-5").unwrap();
        assert_eq!(opus.input, 4.00);
        assert_eq!(lookup("claude-opus-5-5-20261103"), Some(opus));
        assert_eq!(lookup("claude-opus-5-5-2026-11-03"), Some(opus));
        assert_eq!(
            lookup("claude-haiku-5-5-20261001").unwrap().cache_read,
            0.01
        );
        for unpriced in [
            "claude-opus-5-55",
            "claude-opus-5-5-latest",
            "claude-opus-5-5-2026",
            "claude-opus-5",
            "gpt-6.2-sol",
            "codex-auto-review",
            "<synthetic>",
            "",
        ] {
            assert_eq!(lookup(unpriced), None, "{unpriced}");
        }
        let t = Tokens {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_write: 3_000_000,
            cache_write_1h: 1_000_000,
            reasoning: 0,
        };
        let usd = cost("claude-sonnet-5-5-20261103", &t).unwrap();
        assert!((usd - (2.0 + 10.0 + 0.1 + 2.0 * 2.5 + 4.0)).abs() < 1e-9);
        assert_eq!(cost("gpt-6.2-sol", &t), None);
    }
}
