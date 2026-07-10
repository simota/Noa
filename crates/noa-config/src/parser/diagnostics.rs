use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
}
pub(super) fn unknown_key_diagnostic(path: &Path, key: &str) -> Diagnostic {
    Diagnostic {
        message: format!("config {}: unsupported key `{key}` ignored", path.display()),
    }
}

pub(super) fn config_file_diagnostic(path: &Path, value: &str, reason: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `config-file = {value}` {reason}; include ignored",
            path.display()
        ),
    }
}

pub(super) fn invalid_value_diagnostic(path: &Path, key: &str, value: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: invalid value for `{key}`: `{value}`; using default",
            path.display()
        ),
    }
}

pub(super) fn theme_pair_diagnostic(path: &Path) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `theme = light:...,dark:...` must set both `light:` and `dark:` to a \
             non-empty theme name; value ignored",
            path.display()
        ),
    }
}

pub(super) fn empty_family_diagnostic(path: &Path, key: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `{key}` requires a non-empty font family name; value ignored",
            path.display()
        ),
    }
}

pub(super) fn invalid_font_feature_diagnostic(path: &Path, value: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: invalid value for `font-feature`: `{value}`; expected a 4-character \
             OpenType tag, optionally prefixed with `-` to disable (e.g. `calt`, `-liga`)",
            path.display()
        ),
    }
}

pub(super) fn invalid_font_variation_diagnostic(path: &Path, key: &str, value: &str) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: invalid value for `{key}`: `{value}`; expected `AXIS=VALUE` with a \
             4-character axis tag and a numeric value (e.g. `wght=700`)",
            path.display()
        ),
    }
}

pub(super) fn window_pair_diagnostic(path: &Path) -> Diagnostic {
    Diagnostic {
        message: format!(
            "config {}: `window-width` and `window-height` must be set together; ignoring both",
            path.display()
        ),
    }
}
