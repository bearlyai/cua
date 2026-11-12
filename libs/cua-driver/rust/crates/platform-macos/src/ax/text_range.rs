//! Text ranges reported by macOS accessibility use UTF-16 code units.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TextRange {
    pub location: usize,
    pub length: usize,
}

pub fn is_text_entry(role: &str, subrole: Option<&str>, protected: Option<bool>) -> bool {
    protected != Some(true)
        && !matches!(subrole, Some("AXSecureTextField" | "AXSecureTextArea"))
        && matches!(
            role,
            "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField"
        )
}

/// Convert only UTF-16 boundaries that correspond to complete Unicode scalars.
fn byte_index(value: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in value.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += ch.len_utf16();
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(value.len())
}

pub fn splice(value: &str, range: TextRange, text: &str) -> Option<String> {
    let start = byte_index(value, range.location)?;
    let end = byte_index(value, range.location.checked_add(range.length)?)?;
    Some(format!("{}{}{}", &value[..start], text, &value[end..]))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InsertAt {
    #[default]
    Caret,
    End,
    Selection,
}

impl InsertAt {
    pub fn parse(value: Option<&serde_json::Value>) -> Result<Self, &'static str> {
        match value {
            None => Ok(Self::Caret),
            Some(serde_json::Value::String(s)) => match s.as_str() {
                "caret" => Ok(Self::Caret),
                "end" => Ok(Self::End),
                "selection" => Ok(Self::Selection),
                _ => Err("insert_at must be caret, end, or selection"),
            },
            _ => Err("insert_at must be caret, end, or selection"),
        }
    }

    pub fn range(
        self,
        value: &str,
        selected: Option<TextRange>,
    ) -> Result<TextRange, &'static str> {
        let range = match self {
            Self::End => TextRange {
                location: value.encode_utf16().count(),
                length: 0,
            },
            Self::Caret => match selected {
                Some(r) if r.length == 0 => r,
                Some(_) => return Err("BACKGROUND_TEXT_SELECTION_REQUIRES_EXPLICIT_REPLACEMENT"),
                None if value.is_empty() => TextRange {
                    location: 0,
                    length: 0,
                },
                None => return Err("BACKGROUND_TEXT_CARET_UNAVAILABLE"),
            },
            Self::Selection => selected.ok_or("BACKGROUND_TEXT_SELECTION_UNAVAILABLE")?,
        };
        splice(value, range, "").ok_or("BACKGROUND_TEXT_RANGE_INVALID")?;
        Ok(range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_splicing_preserves_surrounding_text() {
        assert_eq!(
            splice(
                "a😀b",
                TextRange {
                    location: 3,
                    length: 0
                },
                "é"
            )
            .as_deref(),
            Some("a😀éb")
        );
        assert_eq!(
            splice(
                "a😀b",
                TextRange {
                    location: 1,
                    length: 2
                },
                "X"
            )
            .as_deref(),
            Some("aXb")
        );
        for range in [
            TextRange {
                location: 2,
                length: 0,
            },
            TextRange {
                location: 0,
                length: 2,
            },
            TextRange {
                location: 9,
                length: 0,
            },
            TextRange {
                location: 1,
                length: usize::MAX,
            },
        ] {
            assert_eq!(splice("a😀b", range, "!"), None);
        }
    }
    #[test]
    fn caret_requires_a_collapsed_readable_range() {
        assert_eq!(
            InsertAt::Caret.range("text", None),
            Err("BACKGROUND_TEXT_CARET_UNAVAILABLE")
        );
        assert_eq!(
            InsertAt::Caret.range(
                "text",
                Some(TextRange {
                    location: 1,
                    length: 2
                })
            ),
            Err("BACKGROUND_TEXT_SELECTION_REQUIRES_EXPLICIT_REPLACEMENT")
        );
        assert_eq!(
            InsertAt::Caret.range("", None),
            Ok(TextRange {
                location: 0,
                length: 0
            })
        );
        assert_eq!(
            InsertAt::Caret.range(
                "text",
                Some(TextRange {
                    location: 2,
                    length: 0
                })
            ),
            Ok(TextRange {
                location: 2,
                length: 0
            })
        );
    }
    #[test]
    fn explicit_end_and_selection_have_distinct_semantics() {
        assert_eq!(
            InsertAt::End.range("😀", None),
            Ok(TextRange {
                location: 2,
                length: 0
            })
        );
        assert_eq!(
            InsertAt::Selection.range("text", None),
            Err("BACKGROUND_TEXT_SELECTION_UNAVAILABLE")
        );
        assert_eq!(
            InsertAt::Selection.range(
                "text",
                Some(TextRange {
                    location: 1,
                    length: 2
                })
            ),
            Ok(TextRange {
                location: 1,
                length: 2
            })
        );
        assert_eq!(
            InsertAt::End.range(
                "text",
                Some(TextRange {
                    location: 0,
                    length: 4
                })
            ),
            Ok(TextRange {
                location: 4,
                length: 0
            })
        );
    }
    #[test]
    fn insertion_argument_rejects_unknown_and_non_string_values() {
        assert_eq!(InsertAt::parse(None), Ok(InsertAt::Caret));
        for (name, mode) in [
            ("caret", InsertAt::Caret),
            ("end", InsertAt::End),
            ("selection", InsertAt::Selection),
        ] {
            assert_eq!(InsertAt::parse(Some(&serde_json::json!(name))), Ok(mode));
        }
        for value in [
            serde_json::json!(null),
            serde_json::json!(1),
            serde_json::json!("replace"),
        ] {
            assert!(InsertAt::parse(Some(&value)).is_err());
        }
    }
    #[test]
    fn only_unprotected_text_entries_expose_selection() {
        for role in ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"] {
            assert!(is_text_entry(role, None, None));
            assert!(!is_text_entry(role, Some("AXSecureTextField"), None));
            assert!(!is_text_entry(role, None, Some(true)));
        }
        assert!(!is_text_entry("AXButton", None, None));
    }
}
