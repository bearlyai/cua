//! Exact range-based insertion, with in-window AX focus and no app activation.

use crate::ax::{
    bindings::*,
    text_range::{self, InsertAt, TextRange},
};
use cua_driver_core::protocol::ToolResult;

pub(super) trait TextControl {
    /// None includes unreadable or demonstrably truncated values.
    fn value(&self) -> Option<String>;
    /// `AXValue` as the element reports it, without the truncation check.
    fn reported_value(&self) -> Option<String> {
        self.value()
    }
    /// `AXSelectedText`, when readable.
    fn selected_text(&self) -> Option<String> {
        None
    }
    /// Whether the whole `AXValue` is settable.
    fn value_settable(&self) -> bool {
        false
    }
    /// Set the whole `AXValue`; true when the write was accepted.
    fn set_value(&self, _text: &str) -> bool {
        false
    }
    fn range(&self) -> Option<TextRange>;
    fn set_range(&self, range: TextRange) -> bool;
    fn insert(&self, text: &str) -> Write;
    fn focused(&self) -> Option<bool> {
        None
    }
    /// Ask for focus; true when the request itself was accepted.
    fn focus(&self) -> bool {
        false
    }
    /// Role, subrole and protected-content flag of the element.
    fn role_facts(&self) -> (String, Option<String>, Option<bool>);
    /// Whether the element reports `AXSelectedText` as settable.
    fn selected_text_settable(&self) -> bool;
}

/// How the text write itself answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Write {
    Accepted,
    /// The element refused the write outright: nothing was written.
    Rejected,
    /// Any other error, such as a busy app's messaging timeout: the write may
    /// still land.
    Uncertain,
}

/// Up to 100 ms for an accepted focus request to read back as focused.
fn focus_retained(control: &impl TextControl) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    while control.focused() != Some(true) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    control.focused() == Some(true)
}

/// What the background route does with an accessibility target.
pub(super) enum Plan {
    /// The range-verified accessibility write ran, or refused before writing.
    Reported(ToolResult),
    /// The field takes no `AXSelectedText` write: use the keystroke step.
    Keystrokes,
    /// The field needs keystrokes, which the caller does not allow for this
    /// call; nothing was sent. Says whether the focus step ran and, when the
    /// driver had moved the range, whether it was put back.
    NoKeystrokes {
        focused_first: bool,
        range_restored: Option<bool>,
    },
}

/// Set `to` and read it back.
fn put_back(control: &impl TextControl, to: TextRange) -> bool {
    control.set_range(to) && control.range() == Some(to)
}

/// `addressed`: the element came from an element token, not from the app's
/// focused element. `keystrokes_allowed`: false when the caller already knows
/// it may not send keystrokes for this call (the keystroke budget, or keyboard
/// ambiguity within the app), so that is settled before anything moves.
fn plan(
    control: &impl TextControl,
    mode: InsertAt,
    text: &str,
    trusted: bool,
    addressed: bool,
    keystrokes_allowed: bool,
) -> Plan {
    let (role, subrole, protected) = control.role_facts();
    // An unfocused AppKit field reports AXSelectedText as settable only once
    // it is focused, so a native unfocused field goes through the focus step
    // and its settability is checked again there.
    if text_range::is_text_entry(&role, subrole.as_deref(), protected)
        && (control.selected_text_settable() || (trusted && control.focused() == Some(false)))
    {
        return transact(control, mode, text, trusted, addressed, keystrokes_allowed);
    }
    if !keystrokes_allowed {
        return Plan::NoKeystrokes {
            focused_first: false,
            range_restored: None,
        };
    }
    // The background keystroke step never sets focus, so an addressed element
    // must already read focused. Focusing it here could select its value for
    // the keystrokes to replace, and web content must not be focused at all.
    if addressed && control.focused() != Some(true) {
        return Plan::Reported(refusal("BACKGROUND_TEXT_FOCUS_UNSUPPORTED"));
    }
    let prior = control.range();
    match keystroke_gate(control, mode) {
        Ok(()) => Plan::Keystrokes,
        Err((code, moved)) => {
            let mut result = refusal(code);
            if moved {
                let restored = prior.is_some_and(|prior| put_back(control, prior));
                if let Some(structured) = result.structured_content.as_mut() {
                    structured["range_restored"] = restored.into();
                }
            }
            Plan::Reported(result)
        }
    }
}

/// Caret rules for a field that takes keystrokes instead of an
/// `AXSelectedText` write. Keystrokes replace a selection and type at the
/// caret, so a readable selection is replaced only on request, and `end`
/// types only once the caret provably sits at the end. A refusal says whether
/// the gate itself asked to move the caret.
fn keystroke_gate(control: &impl TextControl, mode: InsertAt) -> Result<(), (&'static str, bool)> {
    let range = control.range();
    if mode != InsertAt::Selection && range.is_some_and(|r| r.length > 0) {
        return Err((
            "BACKGROUND_TEXT_SELECTION_REQUIRES_EXPLICIT_REPLACEMENT",
            false,
        ));
    }
    match mode {
        InsertAt::End => {
            let end = control.value().map(|v| TextRange {
                location: v.encode_utf16().count(),
                length: 0,
            });
            if !end.is_some_and(|end| put_back(control, end)) {
                return Err(("BACKGROUND_TEXT_CARET_UNAVAILABLE", end.is_some()));
            }
        }
        InsertAt::Selection if range.is_none() => {
            return Err(("BACKGROUND_TEXT_SELECTION_UNAVAILABLE", false));
        }
        // An unreadable caret keeps the keystroke step it always had.
        _ => {}
    }
    Ok(())
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

fn transact(
    control: &impl TextControl,
    mode: InsertAt,
    text: &str,
    trusted: bool,
    addressed: bool,
    keystrokes_allowed: bool,
) -> Plan {
    let Some(before) = control.value() else {
        return Plan::Reported(refusal("BACKGROUND_TEXT_VALUE_UNREADABLE"));
    };
    let prior_range = control.range();
    let focused_first = control.focused() == Some(false);
    // A refusal after the driver moved the range (its focus step or a range
    // set) puts back the range the field had before this call, or a caret at
    // the end when that was unreadable, so no later call or keystroke can
    // replace a selection the driver created.
    let restore_to = prior_range.unwrap_or(TextRange {
        location: before.encode_utf16().count(),
        length: 0,
    });
    let range_moved = std::cell::Cell::new(false);
    let restore_if_moved = || {
        (range_moved.get() || (focused_first && control.range() != prior_range))
            .then(|| put_back(control, restore_to))
    };
    let refuse = |code: &str| {
        let mut result = if focused_first {
            focused_refusal(code)
        } else {
            refusal(code)
        };
        if let Some(restored) = restore_if_moved() {
            if let Some(structured) = result.structured_content.as_mut() {
                structured["range_restored"] = restored.into();
            }
        }
        result
    };
    if focused_first {
        if !trusted {
            return Plan::Reported(refusal("BACKGROUND_TEXT_FOCUS_UNSUPPORTED"));
        }
        if !control.focus() {
            return Plan::Reported(refusal("BACKGROUND_TEXT_FOCUS_NOT_RETAINED"));
        }
        // The request was accepted, so focus may have moved even when it
        // does not read back in time.
        if !focus_retained(control) {
            return Plan::Reported(refuse("BACKGROUND_TEXT_FOCUS_NOT_RETAINED"));
        }
        if control.value().as_deref() != Some(before.as_str()) {
            return Plan::Reported(refuse("BACKGROUND_TEXT_VALUE_CHANGED"));
        }
    }
    // A toolkit may apply its select-on-focus a moment after AXFocused reads
    // true. One settled range read drives both the focus guard and the write.
    let Some(original_range) = settled_range(control) else {
        return Plan::Reported(refuse("BACKGROUND_TEXT_RANGE_UNSTABLE"));
    };
    // Focusing a single-line Cocoa field commonly selects its entire
    // value. That new selection is not a remembered caret or a selection
    // the caller observed. Only an explicit append may collapse it.
    if focused_first
        && !before.is_empty()
        && mode != InsertAt::End
        && original_range.is_some_and(|r| r.length > 0)
        && !(mode == InsertAt::Selection && original_range == prior_range)
    {
        return Plan::Reported(refuse("BACKGROUND_TEXT_FOCUS_SELECTED_VALUE"));
    }
    // A field that still takes no AXSelectedText write once focused gets the
    // keystroke step, under the same caret rules.
    if !control.selected_text_settable() {
        if addressed && control.focused() != Some(true) {
            return Plan::Reported(refuse("BACKGROUND_TEXT_FOCUS_UNSUPPORTED"));
        }
        if !keystrokes_allowed {
            return Plan::NoKeystrokes {
                focused_first,
                range_restored: restore_if_moved(),
            };
        }
        return match keystroke_gate(control, mode) {
            Ok(()) => Plan::Keystrokes,
            Err((code, moved)) => {
                range_moved.set(moved);
                Plan::Reported(refuse(code))
            }
        };
    }
    let range = match mode.range(&before, original_range) {
        Ok(r) => r,
        Err(code) => return Plan::Reported(refuse(code)),
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
    let holds = |r| {
        range_moved.set(true);
        control.set_range(r) && control.range() == Some(r)
    };
    if probe.is_some_and(|r| !holds(r)) || !holds(range) {
        return Plan::Reported(refuse("BACKGROUND_TEXT_RANGE_NOT_RETAINED"));
    }
    // Setting a range must not edit the value; also catch concurrent edits.
    // Re-read value, then range, immediately before the write: the decision
    // holds only for exactly this value and range.
    if control.value().as_deref() != Some(before.as_str()) {
        return Plan::Reported(refuse("BACKGROUND_TEXT_VALUE_CHANGED"));
    }
    if control.range() != Some(range) {
        return Plan::Reported(refuse("BACKGROUND_TEXT_RANGE_NOT_RETAINED"));
    }
    let write = control.insert(text);
    let accepted = write == Write::Accepted;
    // Native apps may update AXValue on their next main-loop turn. Poll only
    // readback: this non-idempotent operation is never retried. A write that
    // errored without a clear rejection may still land, so it is polled too.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    let mut after = control.value();
    while write != Write::Rejected
        && after.as_deref() == Some(before.as_str())
        && before != expected
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
        after = control.value();
    }
    let exact = after.as_deref() == Some(expected.as_str());
    let changed = after.as_deref().is_some_and(|s| s != before);
    // Unchanged proves nothing landed only after a clear rejection, or an
    // accepted write on a native target (AppKit applies it synchronously).
    // Web content can accept the write and echo it after this poll ends.
    let landing_unknown = write == Write::Uncertain || (accepted && !trusted);
    let effect = if exact && trusted {
        "confirmed"
    } else if after.is_none() || exact || landing_unknown {
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
    } else if landing_unknown {
        "BACKGROUND_TEXT_WRITE_UNCONFIRMED"
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
    Plan::Reported(result)
}

/// After the driver's own focus step: when focusing turned the range into one
/// covering the whole non-empty value and it did not cover it before, put the
/// pre-focus range back (or a caret at the end when it was unreadable) and read
/// it back. `Ok(true)`: focus had selected everything and the range is
/// restored; `Ok(false)`: focus selected nothing new; `Err(())`: the restore
/// did not hold.
fn undo_focus_select_all(
    control: &impl TextControl,
    before_focus: Option<TextRange>,
) -> Result<bool, ()> {
    let Some(value) = control.value().filter(|v| !v.is_empty()) else {
        return Ok(false);
    };
    let end = value.encode_utf16().count();
    let whole = TextRange {
        location: 0,
        length: end,
    };
    if control.range() != Some(whole) || before_focus == Some(whole) {
        return Ok(false);
    }
    let target = before_focus.unwrap_or(TextRange {
        location: end,
        length: 0,
    });
    if control.set_range(target) && control.range() == Some(target) {
        Ok(true)
    } else {
        Err(())
    }
}

/// What the foreground focus step did to the field it focused.
#[derive(Default)]
pub(super) struct ForegroundFocus {
    /// Focusing selected the whole value, and the caret was put back.
    focus_selected_all: bool,
}

impl ForegroundFocus {
    /// The long-text accessibility write, which never sets the whole value
    /// for a selection this focus step created.
    pub(super) fn atomic_insert(
        &self,
        field: &impl TextControl,
        text: &str,
        before: Option<&str>,
    ) -> (bool, Option<usize>) {
        super::type_text::atomic_insert(field, text, before, self.focus_selected_all)
    }
}

/// The foreground step's focus on an addressed field: focus it, once more
/// after `reapply_after` when it did not take, wait `settle`, then undo a
/// whole-value selection the focus created. `Err(())`: that selection could
/// not be undone, so nothing may be typed.
pub(super) fn foreground_focus(
    control: &impl TextControl,
    is_focused: impl Fn() -> bool,
    reapply_after: std::time::Duration,
    settle: std::time::Duration,
) -> Result<ForegroundFocus, ()> {
    let before_focus = control.range();
    control.focus();
    if !is_focused() {
        std::thread::sleep(reapply_after);
        control.focus();
    }
    std::thread::sleep(settle);
    undo_focus_select_all(control, before_focus)
        .map(|focus_selected_all| ForegroundFocus { focus_selected_all })
}

/// The foreground step refuses before any keystroke: focusing the field
/// selected its whole value and the caret could not be put back.
pub(super) fn foreground_focus_refusal(path: &str, requested: usize) -> ToolResult {
    let code = "BACKGROUND_TEXT_FOCUS_SELECTED_VALUE";
    ToolResult::error(format!(
        "Focusing the field selected its whole value and the caret could not be restored; nothing was typed: {code}."
    ))
    .with_structured(serde_json::json!({
        "code": code, "effect": "refused", "path": path, "verified": false,
        "focused_first": true, "range_restored": false,
        "requested_chars": requested, "posted_keystrokes": 0,
    }))
}

/// The selected range once two consecutive reads agree (unreadable counts as
/// a reading), polled for at most 150 ms; `None` when it never settles.
fn settled_range(control: &impl TextControl) -> Option<Option<TextRange>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(150);
    let mut last = control.range();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(10));
        let next = control.range();
        if next == last {
            return Some(next);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        last = next;
    }
}

/// A live accessibility element; the caller keeps it alive.
pub(super) struct AxTextControl(pub(super) AXUIElementRef);
impl TextControl for AxTextControl {
    fn reported_value(&self) -> Option<String> {
        unsafe { copy_string_attr(self.0, "AXValue") }
    }
    fn selected_text(&self) -> Option<String> {
        unsafe { copy_string_attr(self.0, "AXSelectedText") }
    }
    fn value_settable(&self) -> bool {
        unsafe { is_attribute_settable(self.0, "AXValue") }
    }
    fn set_value(&self, text: &str) -> bool {
        unsafe { set_string_attr(self.0, "AXValue", text) == kAXErrorSuccess }
    }
    fn focused(&self) -> Option<bool> {
        unsafe { copy_bool_attr(self.0, "AXFocused") }
    }
    fn focus(&self) -> bool {
        unsafe { set_bool_attr_true(self.0, "AXFocused") == kAXErrorSuccess }
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
    fn insert(&self, text: &str) -> Write {
        match unsafe { set_string_attr(self.0, "AXSelectedText", text) } {
            error if error == kAXErrorSuccess => Write::Accepted,
            // Unsupported attribute, illegal argument, invalid element,
            // action unsupported, not implemented, API disabled.
            -25205 | -25201 | -25202 | -25206 | -25208 | -25211 => Write::Rejected,
            _ => Write::Uncertain,
        }
    }
    fn role_facts(&self) -> (String, Option<String>, Option<bool>) {
        unsafe {
            (
                copy_string_attr(self.0, "AXRole").unwrap_or_default(),
                copy_string_attr(self.0, "AXSubrole"),
                copy_bool_attr(self.0, "AXProtectedContent"),
            )
        }
    }
    fn selected_text_settable(&self) -> bool {
        unsafe { is_attribute_settable(self.0, "AXSelectedText") }
    }
}

/// # Safety
/// The retained element must remain live for the complete transaction.
pub(super) unsafe fn insert(
    element: AXUIElementRef,
    mode: InsertAt,
    text: &str,
    trusted: bool,
    addressed: bool,
    keystrokes_allowed: bool,
) -> Plan {
    plan(
        &AxTextControl(element),
        mode,
        text,
        trusted,
        addressed,
        keystrokes_allowed,
    )
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
        role: RefCell<String>,
        selected_text_settable: Cell<bool>,
        /// A select-all that lands this many value/range reads after focus.
        late_select_after: Cell<Option<usize>>,
        late_armed: Cell<Option<usize>>,
        focus_sticks: Cell<bool>,
        write_result: Cell<Write>,
        settable_once_focused: Cell<bool>,
        /// The write is accepted but the value echo has not caught up.
        echo_lags: Cell<bool>,
        focus_calls: Cell<usize>,
        /// The next range set lands here instead of where it was asked to.
        misplace_next_set: Cell<Option<TextRange>>,
        value_settable: Cell<bool>,
    }
    impl Control {
        fn tick(&self) {
            match self.late_armed.get() {
                Some(0) => {
                    let len = self
                        .value
                        .borrow()
                        .as_deref()
                        .map_or(0, |v| v.encode_utf16().count());
                    self.range.set(Some(TextRange {
                        location: 0,
                        length: len,
                    }));
                    self.late_armed.set(None);
                }
                Some(n) => self.late_armed.set(Some(n - 1)),
                None => {}
            }
        }
    }
    impl TextControl for Control {
        fn role_facts(&self) -> (String, Option<String>, Option<bool>) {
            (self.role.borrow().clone(), None, None)
        }
        fn selected_text_settable(&self) -> bool {
            self.selected_text_settable.get()
                || (self.settable_once_focused.get() && self.focused.get() == Some(true))
        }
        fn focused(&self) -> Option<bool> {
            self.focused.get()
        }
        fn focus(&self) -> bool {
            self.focus_calls.set(self.focus_calls.get() + 1);
            if !self.focus_supported.get() {
                return false;
            }
            if !self.focus_sticks.get() {
                return true;
            }
            self.focused.set(Some(true));
            self.range.set(self.focus_range.get());
            self.late_armed.set(self.late_select_after.get());
            true
        }
        fn value(&self) -> Option<String> {
            self.tick();
            if self.hide_after_write.get() && self.writes.get() > 0 {
                return None;
            }
            self.value.borrow().clone()
        }
        fn range(&self) -> Option<TextRange> {
            self.tick();
            self.range.get()
        }
        fn set_range(&self, r: TextRange) -> bool {
            if let Some(wrong) = self.misplace_next_set.take() {
                self.range.set(Some(wrong));
            } else if self.holds {
                self.range.set(Some(r));
            }
            true
        }
        fn selected_text(&self) -> Option<String> {
            let range = self.range()?;
            let value = self.value()?;
            Some(
                value
                    .chars()
                    .skip(range.location)
                    .take(range.length)
                    .collect(),
            )
        }
        fn value_settable(&self) -> bool {
            self.value_settable.get()
        }
        fn set_value(&self, text: &str) -> bool {
            *self.value.borrow_mut() = Some(text.into());
            true
        }
        fn insert(&self, t: &str) -> Write {
            self.writes.set(self.writes.get() + 1);
            if self.write_result.get() != Write::Accepted || self.echo_lags.get() {
                return self.write_result.get();
            }
            // The write itself is not a read: a late selection that lands
            // after the last check cannot be observed by any reader.
            let v = (!self.hide_after_write.get())
                .then(|| self.value.borrow().clone())
                .flatten();
            *self.value.borrow_mut() = v.and_then(|v| {
                if self.wrong {
                    Some(t.to_string())
                } else {
                    text_range::splice(&v, self.range.get()?, t)
                }
            });
            Write::Accepted
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
            focused: Cell::new(Some(true)),
            focus_range: Cell::new(None),
            focus_supported: Cell::new(true),
            hide_after_write: Cell::new(false),
            role: RefCell::new("AXTextField".into()),
            selected_text_settable: Cell::new(true),
            late_select_after: Cell::new(None),
            late_armed: Cell::new(None),
            focus_sticks: Cell::new(true),
            write_result: Cell::new(Write::Accepted),
            settable_once_focused: Cell::new(false),
            echo_lags: Cell::new(false),
            focus_calls: Cell::new(0),
            misplace_next_set: Cell::new(None),
            value_settable: Cell::new(true),
        }
    }
    /// An element addressed by token.
    fn plan(c: &impl TextControl, mode: InsertAt, text: &str, trusted: bool) -> Plan {
        super::plan(c, mode, text, trusted, true, true)
    }
    fn transact_result(
        control: &impl TextControl,
        mode: InsertAt,
        text: &str,
        trusted: bool,
    ) -> ToolResult {
        match transact(control, mode, text, trusted, true, true) {
            Plan::Reported(result) => result,
            _ => panic!("expected the accessibility route"),
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
            let result = transact_result(&c, mode, "!", true);
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
            state(transact_result(&c, InsertAt::Selection, "!", true))["effect"],
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
            state(transact_result(&c, InsertAt::Caret, "!", true))["effect"],
            "confirmed"
        );
        assert_eq!(c.value().as_deref(), Some("!"));
        let c = control(true, false);
        c.focused.set(Some(false));
        c.focus_supported.set(false);
        assert_eq!(
            state(transact_result(&c, InsertAt::End, "!", true))["code"],
            "BACKGROUND_TEXT_FOCUS_NOT_RETAINED"
        );
        assert_eq!(c.writes.get(), 0);
    }

    #[test]
    fn ignored_range_set_refuses_before_text_write_even_at_zero() {
        for mode in [InsertAt::Caret, InsertAt::End] {
            let c = control(false, false);
            let s = state(transact_result(&c, mode, "!", true));
            assert_eq!(s["code"], "BACKGROUND_TEXT_RANGE_NOT_RETAINED");
            assert_eq!(s["effect"], "refused");
            assert_eq!(c.writes.get(), 0);
            assert_eq!(c.value().as_deref(), Some("Original"));
        }
    }
    #[test]
    fn exact_expected_value_is_required_and_write_is_never_retried() {
        let c = control(true, true);
        let s = state(transact_result(&c, InsertAt::End, "!", true));
        assert_eq!(s["effect"], "unverifiable");
        assert_eq!(s["code"], "BACKGROUND_TEXT_UNEXPECTED_VALUE");
        assert_eq!(s["verified"], false);
        assert_eq!(c.writes.get(), 1);
        let c = control(true, false);
        let s = state(transact_result(&c, InsertAt::End, "!", true));
        assert_eq!(s["effect"], "confirmed");
        assert_eq!(c.value().as_deref(), Some("Original!"));
    }
    #[test]
    fn unreadable_values_and_untrusted_echo_never_confirm() {
        let c = control(true, false);
        c.hide_after_write.set(true);
        let s = state(transact_result(&c, InsertAt::End, "!", true));
        assert_eq!(s["effect"], "unverifiable");
        assert_eq!(s["code"], "BACKGROUND_TEXT_READBACK_UNAVAILABLE");
        assert_eq!(s["readback_available"], false);
        assert_eq!(c.writes.get(), 1);

        let c = control(true, false);
        *c.value.borrow_mut() = None;
        let s = state(transact_result(&c, InsertAt::End, "!", true));
        assert_eq!(s["code"], "BACKGROUND_TEXT_VALUE_UNREADABLE");
        assert_eq!(c.writes.get(), 0);
        let c = control(true, false);
        let s = state(transact_result(&c, InsertAt::End, "!", false));
        assert_eq!(s["effect"], "unverifiable");
        assert_eq!(s["verified"], false);
    }

    fn keystrokes(p: Plan) -> bool {
        match p {
            Plan::Keystrokes => true,
            Plan::Reported(r) => {
                eprintln!("got accessibility result {}", state(r));
                false
            }
            Plan::NoKeystrokes { .. } => false,
        }
    }
    /// The focused-element path: no element token was passed.
    fn plan_focused(c: &impl TextControl, mode: InsertAt) -> Plan {
        super::plan(c, mode, "!", true, false, true)
    }
    fn reported(p: Plan) -> serde_json::Value {
        match p {
            Plan::Reported(r) => state(r),
            _ => panic!("expected a refusal before the keystroke step"),
        }
    }
    #[test]
    fn fields_without_a_settable_selected_text_get_the_keystroke_step() {
        let c = control(true, false);
        c.selected_text_settable.set(false);
        assert!(
            keystrokes(plan(&c, InsertAt::Caret, "!", false)),
            "a field that takes no AXSelectedText write must reach the keystroke step"
        );
        assert_eq!(
            c.writes.get(),
            0,
            "no AXSelectedText write may be attempted"
        );
        let c = control(true, false);
        *c.role.borrow_mut() = "AXGroup".into();
        assert!(
            keystrokes(plan(&c, InsertAt::Caret, "!", true)),
            "an unsupported role must reach the keystroke step"
        );
        assert_eq!(
            c.writes.get(),
            0,
            "no AXSelectedText write may be attempted"
        );
    }
    #[test]
    fn keystroke_step_keeps_the_caret_rules() {
        // A readable selection is replaced only on request.
        let c = control(true, false);
        c.selected_text_settable.set(false);
        c.range.set(Some(TextRange {
            location: 0,
            length: 8,
        }));
        for mode in [InsertAt::Caret, InsertAt::End] {
            let s = reported(plan(&c, mode, "!", true));
            assert_eq!(
                s["code"],
                "BACKGROUND_TEXT_SELECTION_REQUIRES_EXPLICIT_REPLACEMENT"
            );
        }
        assert!(keystrokes(plan(&c, InsertAt::Selection, "!", true)));
        // end moves the caret to the end and proves it held.
        let c = control(true, false);
        c.selected_text_settable.set(false);
        assert!(keystrokes(plan(&c, InsertAt::End, "!", true)));
        assert_eq!(
            c.range.get(),
            Some(TextRange {
                location: 8,
                length: 0
            })
        );
        let c = control(false, false);
        c.selected_text_settable.set(false);
        assert_eq!(
            reported(plan(&c, InsertAt::End, "!", true))["code"],
            "BACKGROUND_TEXT_CARET_UNAVAILABLE"
        );
        // An unreadable range keeps the default keystroke step.
        let c = control(true, false);
        c.selected_text_settable.set(false);
        c.range.set(None);
        assert!(keystrokes(plan(&c, InsertAt::Caret, "!", true)));
        assert_eq!(
            reported(plan(&c, InsertAt::Selection, "!", true))["code"],
            "BACKGROUND_TEXT_SELECTION_UNAVAILABLE"
        );
        assert_eq!(c.writes.get(), 0);
    }
    #[test]
    fn a_selection_that_lands_after_focus_is_never_replaced() {
        for mode in [InsertAt::Caret, InsertAt::Selection, InsertAt::End] {
            for late in 0..12 {
                let c = control(true, false);
                c.focused.set(Some(false));
                c.focus_range.set(Some(TextRange {
                    location: 8,
                    length: 0,
                }));
                c.late_select_after.set(Some(late));
                let s = state(transact_result(&c, mode, "!", true));
                let value = c.value.borrow().clone().unwrap();
                assert!(
                    value.contains("Original"),
                    "{mode:?} with a select-all {late} reads after focus replaced the value: {value:?} ({s})"
                );
            }
        }
    }
    #[test]
    fn a_refusal_never_leaves_behind_a_selection_the_driver_created() {
        let c = control(true, false);
        c.focused.set(Some(false));
        c.focus_range.set(Some(TextRange {
            location: 0,
            length: 8,
        }));
        let first = state(transact_result(&c, InsertAt::Caret, "!", true));
        let _second = state(transact_result(&c, InsertAt::Caret, "!", true));
        let third = state(transact_result(&c, InsertAt::Selection, "!", true));
        let value = c.value.borrow().clone().unwrap();
        assert!(
            value.contains("Original"),
            "default, default, selection replaced the value: {value:?} ({third})"
        );
        assert_eq!(first["code"], "BACKGROUND_TEXT_FOCUS_SELECTED_VALUE");
        assert_eq!(
            first["range_restored"], true,
            "the refusal must restore the range focus changed: {first}"
        );
    }
    #[test]
    fn a_value_change_after_the_range_was_set_restores_the_caret() {
        // A concurrent edit lands while the driver sets the caret.
        let c = control(true, false);
        c.range.set(Some(TextRange {
            location: 3,
            length: 0,
        }));
        let s = state(transact_with_edit(&c, InsertAt::End, "!"));
        assert_eq!(s["code"], "BACKGROUND_TEXT_VALUE_CHANGED");
        assert_eq!(s["range_restored"], true, "{s}");
        assert_eq!(
            c.range.get(),
            Some(TextRange {
                location: 3,
                length: 0
            })
        );
        assert_eq!(c.writes.get(), 0);
    }
    /// Runs a transaction on a control whose value changes on its first
    /// range set, as a concurrent edit would.
    fn transact_with_edit(c: &Control, mode: InsertAt, text: &str) -> ToolResult {
        struct Editing<'a>(&'a Control, Cell<bool>);
        impl TextControl for Editing<'_> {
            fn value(&self) -> Option<String> {
                self.0.value()
            }
            fn range(&self) -> Option<TextRange> {
                self.0.range()
            }
            fn set_range(&self, r: TextRange) -> bool {
                if !self.1.replace(true) {
                    *self.0.value.borrow_mut() = Some("Origin".into());
                }
                self.0.set_range(r)
            }
            fn insert(&self, t: &str) -> Write {
                self.0.insert(t)
            }
            fn focused(&self) -> Option<bool> {
                self.0.focused()
            }
            fn focus(&self) -> bool {
                self.0.focus()
            }
            fn role_facts(&self) -> (String, Option<String>, Option<bool>) {
                self.0.role_facts()
            }
            fn selected_text_settable(&self) -> bool {
                true
            }
        }
        transact_result(&Editing(c, Cell::new(false)), mode, text, true)
    }
    #[test]
    fn a_whole_value_selection_from_focus_is_undone() {
        let all = TextRange {
            location: 0,
            length: 8,
        };
        let caret = TextRange {
            location: 3,
            length: 0,
        };
        let c = control(true, false);
        c.range.set(Some(all));
        assert_eq!(undo_focus_select_all(&c, Some(caret)), Ok(true));
        assert_eq!(c.range.get(), Some(caret), "the pre-focus caret comes back");
        let c = control(true, false);
        c.range.set(Some(all));
        assert_eq!(undo_focus_select_all(&c, None), Ok(true));
        assert_eq!(
            c.range.get(),
            Some(TextRange {
                location: 8,
                length: 0
            }),
            "an unreadable pre-focus range becomes a caret at the end"
        );
        let c = control(false, false);
        c.range.set(Some(all));
        assert_eq!(undo_focus_select_all(&c, Some(caret)), Err(()));
        // A selection the field already had, or a partial one, is left alone.
        let c = control(true, false);
        c.range.set(Some(all));
        assert_eq!(undo_focus_select_all(&c, Some(all)), Ok(false));
        c.range.set(Some(TextRange {
            location: 0,
            length: 3,
        }));
        assert_eq!(undo_focus_select_all(&c, Some(caret)), Ok(false));
    }
    #[test]
    fn a_write_error_that_is_not_a_rejection_is_unverifiable() {
        let c = control(true, false);
        c.write_result.set(Write::Uncertain);
        let s = state(transact_result(&c, InsertAt::End, "!", true));
        assert_eq!(
            s["effect"], "unverifiable",
            "a timed-out write may still land, so it must not invite a retry: {s}"
        );
        assert_eq!(s["code"], "BACKGROUND_TEXT_WRITE_UNCONFIRMED");
        assert_eq!(c.writes.get(), 1);
        let c = control(true, false);
        c.write_result.set(Write::Rejected);
        let s = state(transact_result(&c, InsertAt::End, "!", true));
        assert_eq!(s["effect"], "suspected_noop");
        assert_eq!(s["code"], "BACKGROUND_TEXT_UNCHANGED");
    }
    #[test]
    fn focus_that_was_set_but_not_retained_reports_focused_first() {
        let c = control(true, false);
        c.focused.set(Some(false));
        c.focus_sticks.set(false);
        let s = state(transact_result(&c, InsertAt::End, "!", true));
        assert_eq!(s["code"], "BACKGROUND_TEXT_FOCUS_NOT_RETAINED");
        assert_eq!(
            s["focused_first"], true,
            "an accepted focus request may have moved focus: {s}"
        );
        let c = control(true, false);
        c.focused.set(Some(false));
        c.focus_supported.set(false);
        let s = state(transact_result(&c, InsertAt::End, "!", true));
        assert_eq!(s["code"], "BACKGROUND_TEXT_FOCUS_NOT_RETAINED");
        assert!(s.get("focused_first").is_none(), "{s}");
        assert_eq!(c.writes.get(), 0);
    }
    #[test]
    fn an_unfocused_native_field_is_judged_settable_after_its_focus_step() {
        // AppKit reports AXSelectedText settable only once the field is focused.
        let c = control(true, false);
        c.focused.set(Some(false));
        c.selected_text_settable.set(false);
        c.settable_once_focused.set(true);
        let s = reported(plan(&c, InsertAt::End, "!", true));
        assert_eq!(s["effect"], "confirmed", "{s}");
        assert_eq!(c.value().as_deref(), Some("Original!"));
        // Still not settable once focused: keystrokes, caret proven at the end.
        let c = control(true, false);
        c.focused.set(Some(false));
        c.selected_text_settable.set(false);
        assert!(keystrokes(plan(&c, InsertAt::End, "!", true)));
        assert_eq!(c.writes.get(), 0);
        assert_eq!(
            c.range.get(),
            Some(TextRange {
                location: 8,
                length: 0
            })
        );
        // Untrusted targets are never focused, and keystrokes never reach a
        // field that does not already read focused.
        let c = control(true, false);
        c.focused.set(Some(false));
        c.selected_text_settable.set(false);
        c.settable_once_focused.set(true);
        let s = reported(plan(&c, InsertAt::Caret, "!", false));
        assert_eq!(s["code"], "BACKGROUND_TEXT_FOCUS_UNSUPPORTED", "{s}");
        assert_eq!(c.focus_calls.get(), 0);
        assert_eq!(c.focused.get(), Some(false));
    }
    #[test]
    fn the_background_keystroke_step_types_only_into_a_focused_field() {
        let all = TextRange {
            location: 0,
            length: 8,
        };
        // An unfocused AppKit field whose parent walk came back incomplete:
        // untrusted, and not settable until focused. Focusing it would select
        // the whole value for the keystrokes to replace.
        for mode in [InsertAt::Caret, InsertAt::End, InsertAt::Selection] {
            let c = control(true, false);
            c.focused.set(Some(false));
            c.selected_text_settable.set(false);
            c.settable_once_focused.set(true);
            c.focus_range.set(Some(all));
            let s = reported(plan(&c, mode, "!", false));
            assert_eq!(
                s["code"], "BACKGROUND_TEXT_FOCUS_UNSUPPORTED",
                "{mode:?}: {s}"
            );
            assert_eq!(c.focus_calls.get(), 0, "{mode:?}: focus must not move");
            assert_eq!(
                c.range.get(),
                Some(TextRange {
                    location: 0,
                    length: 0
                })
            );
        }
        // An unfocused browser field addressed by element: web content takes
        // no AXSelectedText write and is never focused in the background.
        let c = control(true, false);
        c.focused.set(Some(false));
        c.selected_text_settable.set(false);
        let s = reported(plan(&c, InsertAt::Caret, "!", false));
        assert_eq!(s["code"], "BACKGROUND_TEXT_FOCUS_UNSUPPORTED", "{s}");
        assert_eq!(c.focus_calls.get(), 0);
        // Unreadable focus is not focus.
        let c = control(true, false);
        c.focused.set(None);
        c.selected_text_settable.set(false);
        let s = reported(plan(&c, InsertAt::Caret, "!", false));
        assert_eq!(s["code"], "BACKGROUND_TEXT_FOCUS_UNSUPPORTED", "{s}");
        // A native field that stays non-settable after its guarded focus step
        // arrives at the keystroke step focused, focused exactly once, and
        // only after the select-all check.
        let c = control(true, false);
        c.focused.set(Some(false));
        c.selected_text_settable.set(false);
        c.focus_range.set(Some(all));
        let s = reported(plan(&c, InsertAt::Caret, "!", true));
        assert_eq!(s["code"], "BACKGROUND_TEXT_FOCUS_SELECTED_VALUE", "{s}");
        assert_eq!(s["range_restored"], true, "{s}");
        let c = control(true, false);
        c.focused.set(Some(false));
        c.selected_text_settable.set(false);
        assert!(keystrokes(plan(&c, InsertAt::End, "!", true)));
        assert_eq!(c.focus_calls.get(), 1);
        assert_eq!(c.focused.get(), Some(true));
        // The focused element itself (no element token) keeps its keystrokes.
        let c = control(true, false);
        c.focused.set(None);
        c.selected_text_settable.set(false);
        assert!(keystrokes(plan_focused(&c, InsertAt::Caret)));
    }
    #[test]
    fn an_accepted_write_with_no_echo_is_a_noop_only_on_a_native_target() {
        // Web content can accept the write and echo it after the readback
        // poll ends: the text may still land, so it must not invite a retry.
        let c = control(true, false);
        c.echo_lags.set(true);
        let s = state(transact_result(&c, InsertAt::End, "!", false));
        assert_eq!(
            s["effect"], "unverifiable",
            "an accepted write on web content may still land: {s}"
        );
        assert_eq!(s["code"], "BACKGROUND_TEXT_WRITE_UNCONFIRMED");
        assert_eq!(c.writes.get(), 1);
        // AppKit applies the write synchronously: unchanged means nothing landed.
        let c = control(true, false);
        c.echo_lags.set(true);
        let s = state(transact_result(&c, InsertAt::End, "!", true));
        assert_eq!(s["effect"], "suspected_noop", "{s}");
        assert_eq!(s["code"], "BACKGROUND_TEXT_UNCHANGED");
        // A clear rejection wrote nothing, on any target.
        let c = control(true, false);
        c.write_result.set(Write::Rejected);
        let s = state(transact_result(&c, InsertAt::End, "!", false));
        assert_eq!(s["effect"], "suspected_noop", "{s}");
        assert_eq!(s["code"], "BACKGROUND_TEXT_UNCHANGED");
    }
    /// Keystrokes are not allowed for this call (the keystroke budget or the
    /// same-app keyboard ambiguity), as the caller decided before planning.
    fn plan_without_keystrokes(c: &impl TextControl, mode: InsertAt, trusted: bool) -> Plan {
        super::plan(c, mode, "!", trusted, true, false)
    }
    #[test]
    fn a_keystroke_refusal_never_leaves_the_caret_or_range_moved() {
        let caret = TextRange {
            location: 3,
            length: 0,
        };
        // end on a focused field over the keystroke budget: decided before
        // the caret moves.
        let c = control(true, false);
        c.selected_text_settable.set(false);
        c.range.set(Some(caret));
        let p = plan_without_keystrokes(&c, InsertAt::End, false);
        assert_eq!(
            c.range.get(),
            Some(caret),
            "the caret moved before keystrokes were refused"
        );
        assert!(
            matches!(
                p,
                Plan::NoKeystrokes {
                    focused_first: false,
                    range_restored: None
                }
            ),
            "nothing moved, so there is nothing to report"
        );
        assert_eq!(c.focus_calls.get(), 0);
        // After the guarded focus step selected everything, the keystroke
        // refusal puts the range back and says that focus moved.
        let c = control(true, false);
        c.focused.set(Some(false));
        c.selected_text_settable.set(false);
        c.focus_range.set(Some(TextRange {
            location: 0,
            length: 8,
        }));
        let p = plan_without_keystrokes(&c, InsertAt::End, true);
        assert!(
            matches!(
                p,
                Plan::NoKeystrokes {
                    focused_first: true,
                    range_restored: Some(true)
                }
            ),
            "the refusal must say focus moved and the range was put back"
        );
        assert_eq!(
            c.range.get(),
            Some(TextRange {
                location: 0,
                length: 0
            })
        );
        // end whose caret move does not hold: the old caret comes back.
        let c = control(true, false);
        c.selected_text_settable.set(false);
        c.range.set(Some(caret));
        c.misplace_next_set.set(Some(TextRange {
            location: 5,
            length: 0,
        }));
        let s = reported(plan(&c, InsertAt::End, "!", false));
        assert_eq!(s["code"], "BACKGROUND_TEXT_CARET_UNAVAILABLE");
        assert_eq!(s["range_restored"], true, "{s}");
        assert_eq!(c.range.get(), Some(caret));
    }
    #[test]
    fn the_foreground_focus_step_never_lets_text_replace_the_value() {
        let zero = std::time::Duration::ZERO;
        let all = TextRange {
            location: 0,
            length: 8,
        };
        // A fresh Cocoa field: focusing it selects the whole value.
        let fresh = || {
            let c = control(true, false);
            c.focused.set(Some(false));
            c.focus_range.set(Some(all));
            c
        };
        // Short text is typed at the caret the focus step leaves behind.
        let c = fresh();
        let focus = foreground_focus(&c, || true, zero, zero).expect("restorable");
        assert_eq!(
            c.range.get(),
            Some(TextRange {
                location: 0,
                length: 0
            }),
            "keystrokes would replace a whole-value selection left by focus"
        );
        // Long text: the selected-text write does not land, so only the
        // whole-value fallback is left.
        c.write_result.set(Write::Rejected);
        let _ = focus.atomic_insert(&c, "!", Some("Original"));
        assert_eq!(c.value().as_deref(), Some("Original"));
        // The field selects everything again after the caret was put back:
        // the fallback still never replaces a selection focus created.
        let c = fresh();
        c.late_select_after.set(Some(3));
        c.write_result.set(Write::Rejected);
        let focus = foreground_focus(&c, || true, zero, zero).expect("restorable");
        let _ = focus.atomic_insert(&c, "!", Some("Original"));
        assert_eq!(
            c.value().as_deref(),
            Some("Original"),
            "the long-text fallback replaced a selection the focus step created"
        );
        // A selection the field already had is the caller's to replace.
        let c = control(true, false);
        c.range.set(Some(all));
        c.focus_range.set(Some(all));
        c.write_result.set(Write::Rejected);
        let focus = foreground_focus(&c, || true, zero, zero).expect("restorable");
        let _ = focus.atomic_insert(&c, "!", Some("Original"));
        assert_eq!(c.value().as_deref(), Some("!"));
        // A focus selection that cannot be undone types nothing.
        let c = fresh();
        c.misplace_next_set.set(Some(all));
        assert!(foreground_focus(&c, || true, zero, zero).is_err());
    }
}
