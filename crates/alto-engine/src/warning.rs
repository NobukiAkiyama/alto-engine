//! Conditions a document opened with, reported by kind so the host words them.
use serde::Serialize;
use std::collections::BTreeMap;

/// `code` names the condition; `details` carries its values (hashes, font families)
/// and, for conditions read from damaged data, an English `detail` for developers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub details: BTreeMap<&'static str, String>,
}

impl Warning {
    pub fn new(code: &'static str) -> Self {
        Self {
            code,
            details: BTreeMap::new(),
        }
    }
    pub fn with(mut self, key: &'static str, value: impl ToString) -> Self {
        self.details.insert(key, value.to_string());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_code_and_details() {
        let w = Warning::new("FONT_MISSING").with("family", "Noto Sans JP");
        assert_eq!(
            serde_json::to_value(&w).unwrap(),
            serde_json::json!({"code": "FONT_MISSING", "details": {"family": "Noto Sans JP"}})
        );
    }
}
