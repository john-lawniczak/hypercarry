use std::{error::Error, fmt};

/// Stable CLI failure categories and their process exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    Internal,
    #[allow(
        dead_code,
        reason = "reserved so the documented CLI exit contract remains backward compatible"
    )]
    Unimplemented,
    Cancelled,
    Configuration,
    Network,
    Schema,
    #[allow(
        dead_code,
        reason = "reserved now so M1.4 storage failures keep a stable exit code"
    )]
    Storage,
    PartialData,
    Output,
}

impl ErrorCategory {
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Internal => 1,
            Self::Unimplemented => 3,
            Self::Cancelled => 130,
            Self::Configuration => 10,
            Self::Network => 11,
            Self::Schema => 12,
            Self::Storage => 13,
            Self::PartialData => 14,
            Self::Output => 15,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Unimplemented => "unimplemented",
            Self::Cancelled => "cancelled",
            Self::Configuration => "configuration",
            Self::Network => "network",
            Self::Schema => "schema",
            Self::Storage => "storage",
            Self::PartialData => "partial-data",
            Self::Output => "output",
        }
    }
}

#[derive(Debug)]
pub struct CliError {
    category: ErrorCategory,
    message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl CliError {
    pub fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source<E>(category: ErrorCategory, message: impl Into<String>, source: E) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        Self {
            category,
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn with_boxed_source(
        category: ErrorCategory,
        message: impl Into<String>,
        source: Box<dyn Error + Send + Sync>,
    ) -> Self {
        Self {
            category,
            message: message.into(),
            source: Some(source),
        }
    }

    pub const fn category(&self) -> ErrorCategory {
        self.category
    }

    pub const fn exit_code(&self) -> u8 {
        self.category.exit_code()
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)?;
        if let Some(source) = &self.source {
            write!(formatter, ": {source}")?;
        }
        Ok(())
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_error_categories_have_distinct_documented_exit_codes() {
        let categories = [
            (ErrorCategory::Internal, 1),
            (ErrorCategory::Unimplemented, 3),
            (ErrorCategory::Cancelled, 130),
            (ErrorCategory::Configuration, 10),
            (ErrorCategory::Network, 11),
            (ErrorCategory::Schema, 12),
            (ErrorCategory::Storage, 13),
            (ErrorCategory::PartialData, 14),
            (ErrorCategory::Output, 15),
        ];

        for (category, expected) in categories {
            assert_eq!(category.exit_code(), expected);
            assert!(!category.as_str().is_empty());
        }
    }
}
