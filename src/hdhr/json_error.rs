//! Value-free JSON failures at the device protocol boundary.

use std::fmt;

/// A fixed parse-failure category, independent of device-chosen text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonFailureCategory {
    Io,
    Syntax,
    Data,
    EndOfInput,
}

impl fmt::Display for JsonFailureCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Io => "JSON input failure",
            Self::Syntax => "JSON syntax error",
            Self::Data => "JSON field type or shape mismatch",
            Self::EndOfInput => "incomplete JSON input",
        })
    }
}

/// Safe structural details only. The original serde error is discarded, so
/// neither Debug nor an error source chain can recover its value-bearing text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{category} at line {line}, column {column}")]
pub struct JsonParseError {
    category: JsonFailureCategory,
    line: usize,
    column: usize,
}

impl JsonParseError {
    #[must_use]
    pub const fn category(self) -> JsonFailureCategory {
        self.category
    }

    #[must_use]
    pub const fn line(self) -> usize {
        self.line
    }

    #[must_use]
    pub const fn column(self) -> usize {
        self.column
    }
}

impl From<serde_json::Error> for JsonParseError {
    fn from(error: serde_json::Error) -> Self {
        let category = match error.classify() {
            serde_json::error::Category::Io => JsonFailureCategory::Io,
            serde_json::error::Category::Syntax => JsonFailureCategory::Syntax,
            serde_json::error::Category::Data => JsonFailureCategory::Data,
            serde_json::error::Category::Eof => JsonFailureCategory::EndOfInput,
        };
        Self {
            category,
            line: error.line(),
            column: error.column(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdhr::test_support::{JSON_SECRET_MARKER, assert_value_free_error};

    #[test]
    fn original_serde_value_is_discarded_in_every_error_representation() {
        let input = serde_json::to_string(JSON_SECRET_MARKER).unwrap();
        let raw = serde_json::from_str::<u8>(&input).unwrap_err();
        assert!(raw.to_string().contains(JSON_SECRET_MARKER));
        let location = (raw.line(), raw.column());
        let safe = JsonParseError::from(raw);
        assert_eq!(safe.category(), JsonFailureCategory::Data);
        assert_eq!((safe.line(), safe.column()), location);
        assert_value_free_error(&safe);
    }

    #[test]
    fn fixed_categories_preserve_syntax_eof_and_io_classification() {
        for (input, category) in [
            ("{]", JsonFailureCategory::Syntax),
            ("{", JsonFailureCategory::EndOfInput),
        ] {
            let safe =
                JsonParseError::from(serde_json::from_str::<serde_json::Value>(input).unwrap_err());
            assert_eq!(safe.category(), category);
            assert_value_free_error(&safe);
        }
        struct FailedInput;
        impl std::io::Read for FailedInput {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other(JSON_SECRET_MARKER))
            }
        }
        let safe = JsonParseError::from(
            serde_json::from_reader::<_, serde_json::Value>(FailedInput).unwrap_err(),
        );
        assert_eq!(safe.category(), JsonFailureCategory::Io);
        assert_value_free_error(&safe);
    }
}
