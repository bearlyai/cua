//! Exact range-based insertion, with in-window AX focus and no app activation.

use crate::ax::{
    bindings::*,
    text_range::{self, InsertAt, TextRange},
};
use cua_driver_core::protocol::ToolResult;

trait TextControl {
    /// None includes unreadable or demonstrably truncated values.
    fn value(&self) -> Option<String>;
    fn range(&self) -> Option<TextRange>;
    fn set_range(&self, range: TextRange) -> bool;
    fn insert(&self, text: &str) -> bool;
    fn focused(&self) -> Option<bool> {
        None
    }
    fn focus(&self) -> bool {
        false
    }
}

pub(super) fn refusal(code: &str) -> ToolResult {
    ToolResult::error(format!(
        "Background insertion refused before writing text: {code}."
    ))
    .with_structured(
        serde_json::json!({"code":code,"effect":"refused","path":"ax","verified":false}),
    )
}

fn focused_refusal(code: &str) -> ToolResult {
    ToolResult::error(format!("Field focused first inside its background window; text insertion refused before writing: {code}."))
        .with_structured(serde_json::json!({"code":code,"effect":"refused","path":"ax","verified":false,"focused_first":true}))
}

fn transact(control: &impl TextControl, mode: InsertAt, text: &str, trusted: bool) -> ToolResult {
    let Some(before) = control.value() else {
        return refusal("BACKGROUND_TEXT_VALUE_UNREADABLE");
    };
    let prior_range = control.range();
    let focused_first = control.focused() == Some(false);
    if focused_first {
        if !trusted {
            return refusal("BACKGROUND_TEXT_FOCUS_UNSUPPORTED");
        }
        if !control.focus() || control.focused() != Some(true) {
            return refusal("BACKGROUND_TEXT_FOCUS_NOT_RETAINED");
        }
        if control.value().as_deref() != Some(before.as_str()) {
            return focused_refusal("BACKGROUND_TEXT_VALUE_CHANGED");
        }
        // Focusing a single-line Cocoa field commonly selects its entire
        // value. That new selection is not a remembered caret or a selection
        // the caller observed. Only an explicit append may collapse it.
        if !before.is_empty()
            && mode != InsertAt::End
            && control.range().is_some_and(|r| r.length > 0)
            && !(mode == InsertAt::Selection && control.range() == prior_range)
        {
            return focused_refusal("BACKGROUND_TEXT_FOCUS_SELECTED_VALUE");
        }
    }
    let refuse = |code| {
        if focused_first {
            focused_refusal(code)
        } else {
            refusal(code)
        }
    };
    let original_range = control.range();
    let range = match mode.range(&before, original_range) {
        Ok(r) => r,
        Err(code) => return refuse(code),
    };
    let expected = text_range::splice(&before, range, text).expect("range was validated");
    // A never-focused field may echo {0,0} even when it ignores all range sets.
    // First establish that it can retain a different range, then set the caret.
    let probe = if !before.is_empty()
        && range
            == (TextRange {
                location: 0,
                length: 0,
            }) {
        Some(TextRange {
            location: before.encode_utf16().count(),
            length: 0,
        })
    } else {
        None
    };
    let holds = |r| control.set_range(r) && control.range() == Some(r);
    if probe.is_some_and(|r| !holds(r)) || !holds(range) {
        if let Some(r) = original_range {
            control.set_range(r);
        }
        return refuse("BACKGROUND_TEXT_RANGE_NOT_RETAINED");
    }
    // Setting a range must not edit the value; also catch concurrent edits.
    if control.value().as_deref() != Some(before.as_str()) {
        return refuse("BACKGROUND_TEXT_VALUE_CHANGED");
    }
    let accepted = control.insert(text);
    // Native apps may update AXValue on their next main-loop turn. Poll only
    // readback: this non-idempotent operation is never retried.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    let mut after = control.value();
    while accepted
        && after.as_deref() == Some(before.as_str())
        && before != expected
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
        after = control.value();
    }
    let exact = after.as_deref() == Some(expected.as_str());
    let changed = after.as_deref().is_some_and(|s| s != before);
    let effect = if exact && trusted {
        "confirmed"
    } else if after.is_none() || exact {
        "unverifiable"
    } else if !changed {
        "suspected_noop"
    } else {
        "unverifiable"
    };
    let code = if exact {
        "BACKGROUND_TEXT_EXACT_READBACK"
    } else if after.is_none() {
        "BACKGROUND_TEXT_READBACK_UNAVAILABLE"
    } else if changed {
        "BACKGROUND_TEXT_UNEXPECTED_VALUE"
    } else {
        "BACKGROUND_TEXT_UNCHANGED"
    };
    let focus_note = if focused_first {
        " Field focused first inside its background window."
    } else {
        ""
    };
    let mut result = if exact || after.is_none() {
        ToolResult::text(format!(
            "Background insertion: {effect} ({code}).{focus_note}"
        ))
    } else {
        ToolResult::error(format!(
            "Background insertion: {effect} ({code}); inspect the field before retrying."
        ))
    };
    result = result.with_structured(serde_json::json!({
        "path":"ax", "effect":effect, "code":code, "verified":exact && trusted,
        "requested_chars":text.chars().count(), "posted_keystrokes":0,
        "insertion_range":range, "ax_accepted":accepted,
        "readback_available":after.is_some(), "value_changed":changed, "focused_first":focused_first,
    }));
    result
}

struct AxTextControl(AXUIElementRef);
impl TextControl for AxTextControl {
    fn focused(&self) -> Option<bool> {
        unsafe { copy_bool_attr(self.0, "AXFocused") }
    }
    fn focus(&self) -> bool {
        unsafe {
            if set_bool_attr_true(self.0, "AXFocused") != kAXErrorSuccess {
                return false;
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
            while self.focused() != Some(true) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            self.focused() == Some(true)
        }
    }
    fn value(&self) -> Option<String> {
        unsafe {
            let value = copy_string_attr(self.0, "AXValue")?;
            if copy_number_attr(self.0, "AXNumberOfCharacters")
                .is_some_and(|n| n != value.encode_utf16().count() as f64)
            {
                return None;
            }
            Some(value)
        }
    }
    fn range(&self) -> Option<TextRange> {
        unsafe { copy_text_range(self.0) }
    }
    fn set_range(&self, r: TextRange) -> bool {
        unsafe { set_text_range(self.0, r) == kAXErrorSuccess }
    }
    fn insert(&self, text: &str) -> bool {
        unsafe { set_string_attr(self.0, "AXSelectedText", text) == kAXErrorSuccess }
    }
}

/// # Safety
/// The retained element must remain live for the complete transaction.
pub(super) unsafe fn insert(
    element: AXUIElementRef,
    mode: InsertAt,
    text: &str,
    trusted: bool,
) -> ToolResult {
    let role = copy_string_attr(element, "AXRole").unwrap_or_default();
    if !text_range::is_text_entry(
        &role,
        copy_string_attr(element, "AXSubrole").as_deref(),
        copy_bool_attr(element, "AXProtectedContent"),
    ) {
        return refusal("BACKGROUND_TEXT_TARGET_UNSUPPORTED");
    }
    transact(&AxTextControl(element), mode, text, trusted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    struct Control {
        value: RefCell<Option<String>>,
        range: Cell<Option<TextRange>>,
        holds: bool,
        writes: Cell<usize>,
        wrong: bool,
        focused: Cell<Option<bool>>,
        focus_range: Cell<Option<TextRange>>,
        focus_supported: Cell<bool>,
        hide_after_write: Cell<bool>,
    }
    impl TextControl for Control {
        fn focused(&self) -> Option<bool> {
            self.focused.get()
        }
        fn focus(&self) -> bool {
            if !self.focus_supported.get() {
                return false;
            }
            self.focused.set(Some(true));
            self.range.set(self.focus_range.get());
            true
        }
        fn value(&self) -> Option<String> {
            if self.hide_after_write.get() && self.writes.get() > 0 {
                return None;
            }
            self.value.borrow().clone()
        }
        fn range(&self) -> Option<TextRange> {
            self.range.get()
        }
        fn set_range(&self, r: TextRange) -> bool {
            if self.holds {
                self.range.set(Some(r));
            }
            true
        }
        fn insert(&self, t: &str) -> bool {
            self.writes.set(self.writes.get() + 1);
            let v = self.value();
            *self.value.borrow_mut() = v.and_then(|v| {
                if self.wrong {
                    Some(t.to_string())
                } else {
                    text_range::splice(&v, self.range.get()?, t)
                }
            });
            true
        }
    }
    fn control(holds: bool, wrong: bool) -> Control {
        Control {
            value: RefCell::new(Some("Original".into())),
            range: Cell::new(Some(TextRange {
                location: 0,
                length: 0,
            })),
            holds,
            writes: Cell::new(0),
            wrong,
            focused: Cell::new(None),
            focus_range: Cell::new(None),
            focus_supported: Cell::new(true),
            hide_after_write: Cell::new(false),
        }
    }
    fn state(r: ToolResult) -> serde_json::Value {
        serde_json::to_value(r).unwrap()["structuredContent"].clone()
    }
    #[test]
    fn background_focus_allows_append_and_empty_but_not_a_new_selection() {
        for mode in [InsertAt::Caret, InsertAt::Selection, InsertAt::End] {
            let c = control(true, false);
            c.focused.set(Some(false));
            c.focus_range.set(Some(TextRange {
                location: 0,
                length: 8,
            }));
            let result = transact(&c, mode, "!", true);
            let s = state(result);
            if mode == InsertAt::End {
                assert_eq!(s["effect"], "confirmed");
                assert_eq!(c.value().as_deref(), Some("Original!"));
                assert_eq!(s["focused_first"], true);
            } else {
                assert_eq!(s["code"], "BACKGROUND_TEXT_FOCUS_SELECTED_VALUE");
                assert_eq!(c.writes.get(), 0);
            }
        }
        let c = control(true, false);
        c.range.set(Some(TextRange {
            location: 3,
            length: 2,
        }));
        c.focused.set(Some(false));
        c.focus_range.set(c.range.get());
        assert_eq!(
            state(transact(&c, InsertAt::Selection, "!", true))["effect"],
            "confirmed"
        );
        assert_eq!(c.value().as_deref(), Some("Ori!nal"));
        let c = control(true, false);
        *c.value.borrow_mut() = Some("".into());
        c.focused.set(Some(false));
        c.focus_range.set(Some(TextRange {
            location: 0,
            length: 0,
        }));
        assert_eq!(
            state(transact(&c, InsertAt::Caret, "!", true))["effect"],
            "confirmed"
        );
        assert_eq!(c.value().as_deref(), Some("!"));
        let c = control(true, false);
        c.focused.set(Some(false));
        c.focus_supported.set(false);
        assert_eq!(
            state(transact(&c, InsertAt::End, "!", true))["code"],
            "BACKGROUND_TEXT_FOCUS_NOT_RETAINED"
        );
        assert_eq!(c.writes.get(), 0);
    }

    #[test]
    fn ignored_range_set_refuses_before_text_write_even_at_zero() {
        for mode in [InsertAt::Caret, InsertAt::End] {
            let c = control(false, false);
            let s = state(transact(&c, mode, "!", true));
            assert_eq!(s["code"], "BACKGROUND_TEXT_RANGE_NOT_RETAINED");
            assert_eq!(s["effect"], "refused");
            assert_eq!(c.writes.get(), 0);
            assert_eq!(c.value().as_deref(), Some("Original"));
        }
    }
    #[test]
    fn exact_expected_value_is_required_and_write_is_never_retried() {
        let c = control(true, true);
        let s = state(transact(&c, InsertAt::End, "!", true));
        assert_eq!(s["effect"], "unverifiable");
        assert_eq!(s["code"], "BACKGROUND_TEXT_UNEXPECTED_VALUE");
        assert_eq!(s["verified"], false);
        assert_eq!(c.writes.get(), 1);
        let c = control(true, false);
        let s = state(transact(&c, InsertAt::End, "!", true));
        assert_eq!(s["effect"], "confirmed");
        assert_eq!(c.value().as_deref(), Some("Original!"));
    }
    #[test]
    fn unreadable_values_and_untrusted_echo_never_confirm() {
        let c = control(true, false);
        c.hide_after_write.set(true);
        let s = state(transact(&c, InsertAt::End, "!", true));
        assert_eq!(s["effect"], "unverifiable");
        assert_eq!(s["code"], "BACKGROUND_TEXT_READBACK_UNAVAILABLE");
        assert_eq!(s["readback_available"], false);
        assert_eq!(c.writes.get(), 1);

        let c = control(true, false);
        *c.value.borrow_mut() = None;
        let s = state(transact(&c, InsertAt::End, "!", true));
        assert_eq!(s["code"], "BACKGROUND_TEXT_VALUE_UNREADABLE");
        assert_eq!(c.writes.get(), 0);
        let c = control(true, false);
        let s = state(transact(&c, InsertAt::End, "!", false));
        assert_eq!(s["effect"], "unverifiable");
        assert_eq!(s["verified"], false);
    }
}
