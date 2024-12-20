//! Special Unicode characters used for display with ASCII fallbacks
//! in case we're not in a UTF-8 locale.

use std::fmt::Display;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Glyph {
    BlackCircle,
}

impl Glyph {
    // TODO: Add support for non-Unicode output
    #[allow(dead_code)]
    pub(crate) fn as_ascii(&self) -> &'static str {
        match self {
            Glyph::BlackCircle => "*",
        }
    }

    pub(crate) fn as_utf8(&self) -> &'static str {
        match self {
            Glyph::BlackCircle => "●",
        }
    }
}

impl Display for Glyph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_utf8())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glyph() {
        assert_eq!(Glyph::BlackCircle.as_utf8(), "●");
    }
}
