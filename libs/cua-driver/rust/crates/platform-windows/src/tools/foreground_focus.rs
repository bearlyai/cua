//! Foreground focus for keys and text addressed by an element token.
//!
//! The snapshot store accepts a token while its snapshot is the latest for the
//! window, but the app can remove or replace the control after the read. The
//! foreground routes therefore read the element live through UIA before they
//! activate the window, and the click fallback for a failed UIA `SetFocus`
//! clicks the centre of the element's live rectangle, never the centre cached
//! at read time: whatever now sits at a cached point would be clicked.
//!
//! Pure decisions live here, ungated, so their tests run on any host; the UIA
//! and SendInput calls are supplied by `impl_` through [`FocusTarget`].

use cua_driver_core::protocol::ToolResult;
use std::time::Duration;

/// UIA's `UIA_E_ELEMENTNOTAVAILABLE`: the element is no longer in the tree.
pub(crate) const UIA_E_ELEMENTNOTAVAILABLE: u32 = 0x8004_0201;

/// The element's rectangle as UIA reports it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveBounds {
    /// Current `(left, top, right, bottom)` in screen coordinates.
    Rect((i32, i32, i32, i32)),
    /// `UIA_E_ELEMENTNOTAVAILABLE`: the control no longer exists.
    NotAvailable,
    /// Any other failure, or an element UIA cannot read (MSAA). It proves
    /// neither that the control is gone nor where it is.
    Unknown,
}

/// Classify a live `CurrentBoundingRectangle` read; the error is its HRESULT.
pub(crate) fn live_bounds_from(read: Result<(i32, i32, i32, i32), u32>) -> LiveBounds {
    match read {
        Ok(rect) => LiveBounds::Rect(rect),
        Err(UIA_E_ELEMENTNOTAVAILABLE) => LiveBounds::NotAvailable,
        Err(_) => LiveBounds::Unknown,
    }
}

/// The centre of a non-empty rectangle, computed the way the tree walk
/// computes the cached centre.
pub(crate) fn rect_center((left, top, right, bottom): (i32, i32, i32, i32)) -> Option<(i32, i32)> {
    (right > left && bottom > top).then(|| ((left + right) / 2, (top + bottom) / 2))
}

/// The step was refused because its element no longer exists. Tools turn
/// this into the snapshot store's `stale_element_token` refusal.
#[derive(Debug)]
pub(crate) struct DeadElement;

impl std::fmt::Display for DeadElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stale_element_token: the element no longer exists; no input was sent")
    }
}

impl std::error::Error for DeadElement {}

/// A finished refusal raised inside the foreground transaction, such as the
/// on-screen check's `element_not_visible`; no input was sent.
#[derive(Debug)]
pub(crate) struct FocusRefused(pub ToolResult);

impl std::fmt::Display for FocusRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = self
            .0
            .content
            .iter()
            .filter_map(|content| match content {
                cua_driver_core::protocol::Content::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        f.write_str(&text)
    }
}

impl std::error::Error for FocusRefused {}

/// A failure inside the foreground keyboard transaction, after the window was
/// activated (Windows may also have needed the driver's no-op activation-assist
/// key for that). No keys or text were sent, but the activation happened, so
/// this is never the no-effects `stale_element_token` refusal.
#[derive(Debug)]
pub(crate) struct FailedAfterActivation {
    /// `foreground_element_lost` when the element was gone after activation,
    /// otherwise `foreground_unavailable`.
    pub code: &'static str,
    pub message: String,
    pub hwnd: u64,
    pub element_index: usize,
    /// What became of the focus fallback click.
    pub focus_click: FocusClick,
}

/// Whether the focus fallback click went out, as the reply's `focus_click`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FocusClick {
    /// No click was attempted.
    NotSent,
    /// The click was sent.
    Sent,
    /// The click returned an error: part of its mouse input, or all of it,
    /// may have gone out.
    Uncertain,
}

impl FocusClick {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            FocusClick::NotSent => "not_sent",
            FocusClick::Sent => "sent",
            FocusClick::Uncertain => "uncertain",
        }
    }
}

/// The element was gone after activation.
pub(crate) const FOREGROUND_ELEMENT_LOST: &str = "foreground_element_lost";
/// Focus could not be confirmed after activation.
pub(crate) const FOREGROUND_UNAVAILABLE: &str = "foreground_unavailable";

impl std::fmt::Display for FailedAfterActivation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FailedAfterActivation {}

impl FailedAfterActivation {
    fn result(&self) -> ToolResult {
        ToolResult::error(self.to_string()).with_structured(serde_json::json!({
            "code": self.code,
            "effect": "failed",
            "activated": true,
            "keystrokes_sent": false,
            "focus_click": self.focus_click.as_str(),
            "window_id": self.hwnd,
            "element_index": self.element_index,
            "retryable": false,
            "escalation": {
                "recommended": "verify_state",
                "reason": "The window was brought to the front and no keys or text were sent; read the window again before acting.",
            },
        }))
    }
}

/// Before a foreground route activates the window: refuse an element UIA
/// reports as no longer available. Any other read proceeds as before.
pub(crate) fn require_live(bounds: LiveBounds) -> anyhow::Result<()> {
    match bounds {
        LiveBounds::NotAvailable => Err(DeadElement.into()),
        LiveBounds::Rect(_) | LiveBounds::Unknown => Ok(()),
    }
}

/// The rectangle a foreground step addressed by an element sends input into,
/// read live. A control that is gone, or whose live position cannot be read or
/// is empty, gets no input: the only other point is the one cached at read
/// time, and whatever sits there now would receive it.
pub(crate) fn live_target_rect(bounds: LiveBounds) -> Result<(i32, i32, i32, i32), DeadElement> {
    match bounds {
        LiveBounds::Rect(rect) if rect_center(rect).is_some() => Ok(rect),
        LiveBounds::Rect(_) | LiveBounds::NotAvailable | LiveBounds::Unknown => Err(DeadElement),
    }
}

/// What one foreground tap on an element needs from Windows.
pub(crate) trait TapTarget {
    /// Read the element's rectangle live.
    fn live_bounds(&mut self) -> LiveBounds;
    /// The on-screen check (and scroll into view) for the chosen point;
    /// `Err` is a refusal.
    fn onscreen_point(&mut self, point: (i32, i32)) -> Result<(i32, i32), ToolResult>;
    /// Activate the window and send the input at a screen point.
    fn send(&mut self, point: (i32, i32)) -> anyhow::Result<()>;
    /// Start restoring the earlier foreground window. Called once `send` was
    /// attempted, whether it succeeded or failed part-way; never for a step
    /// refused before dispatch, which changed nothing to restore.
    fn restore_foreground(&mut self);
}

/// A foreground tap (click, double-click, right-click) on an element: read
/// its rectangle live, choose the point in it with `pick`, check that point is
/// on screen, then send. A control that is gone or cannot be located live is
/// refused as stale before anything is activated or sent. Returns the point.
pub(crate) fn foreground_tap(
    target: &mut impl TapTarget,
    pick: impl FnOnce((i32, i32, i32, i32)) -> (i32, i32),
) -> anyhow::Result<(i32, i32)> {
    let rect = live_target_rect(target.live_bounds())?;
    let point = target.onscreen_point(pick(rect)).map_err(FocusRefused)?;
    let sent = target.send(point);
    target.restore_foreground();
    sent?;
    Ok(point)
}

/// Where a cached-point preflight applies. A foreground step checks the live
/// position instead (in [`foreground_tap`]), so it gets the cached point back
/// unchecked, for overlay feedback only; a background step keeps the cached
/// visibility preflight it always had.
pub(crate) fn cached_point_preflight(
    foreground: bool,
    cached: (i32, i32),
    check: impl FnOnce((i32, i32)) -> Result<(i32, i32), ToolResult>,
) -> Result<(i32, i32), ToolResult> {
    if foreground {
        Ok(cached)
    } else {
        check(cached)
    }
}

/// What the element's own UIA scroll did. It is `amount` separate
/// `ScrollPattern::Scroll` calls, so it can stop part-way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SemanticScroll {
    /// Every `Scroll` call was accepted.
    Done,
    /// No `Scroll` call was made: not a UIA element, no `ScrollPattern`, a
    /// failed cast, an unknown direction, or the requested axis reads as
    /// unscrollable (see [`axis_cannot_scroll`]).
    NotDispatched,
    /// `completed` steps (at least one) count as applied: calls the provider
    /// accepted before a later one errored, or a first call that errored after
    /// the scroll position changed. An accepted call at an end of the range
    /// may move nothing.
    Partial { completed: u32, error: String },
    /// The first `Scroll` call errored and the scroll position doesn't show
    /// whether it moved (unchanged, or unreadable). It may have scrolled, so
    /// no wheel may follow and no stale refusal may replace it.
    Uncertain { error: String },
}

/// Where a run of `ScrollPattern::Scroll` calls stopped on an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollStop {
    /// Before any `Scroll` call: nothing was sent.
    BeforeScroll,
    /// The first `Scroll` call returned an error. `moved` when the scroll
    /// position read before it and after the error differs on either axis.
    FirstCall { moved: bool },
    /// A later call returned an error after `completed` accepted calls.
    Later { completed: u32 },
}

/// Classify the element's own scroll from where its `Scroll` calls stopped.
pub(crate) fn semantic_scroll_from(result: Result<(), (ScrollStop, String)>) -> SemanticScroll {
    match result {
        Ok(()) => SemanticScroll::Done,
        Err((ScrollStop::BeforeScroll, _)) => SemanticScroll::NotDispatched,
        Err((ScrollStop::FirstCall { moved: true }, error)) => SemanticScroll::Partial {
            completed: 1,
            error,
        },
        Err((ScrollStop::FirstCall { moved: false }, error)) => SemanticScroll::Uncertain { error },
        Err((ScrollStop::Later { completed }, error)) => {
            SemanticScroll::Partial { completed, error }
        }
    }
}

/// A `ScrollPattern`'s scroll position, `(horizontal, vertical)` percent. An
/// axis is `None` when its getter failed or returned a non-finite value.
pub(crate) type ScrollReading = (Option<f64>, Option<f64>);

/// UIA's `UIA_ScrollPatternNoScroll`: the axis does not scroll.
pub(crate) const UIA_SCROLL_PATTERN_NO_SCROLL: f64 = -1.0;

/// A [`ScrollReading`] from the two getters' values; NaN and infinity count
/// as unreadable.
pub(crate) fn scroll_reading(horizontal: Option<f64>, vertical: Option<f64>) -> ScrollReading {
    let finite = |value: Option<f64>| value.filter(|value| value.is_finite());
    (finite(horizontal), finite(vertical))
}

/// Whether two readings show movement: either axis has both readings and
/// they differ. An unreadable axis says nothing, and hides nothing on the
/// other axis. `-1.0` compares like any other value.
pub(crate) fn movement_seen(before: ScrollReading, after: ScrollReading) -> bool {
    let changed = |before: Option<f64>, after: Option<f64>| matches!((before, after), (Some(before), Some(after)) if before != after);
    changed(before.0, after.0) || changed(before.1, after.1)
}

/// Whether the requested axis reads as UIA's "no scroll on this axis". Then
/// no `Scroll` call is made (providers reject it with an error), so the route
/// is "not sent" and the wheel fallback stays open. An unreadable axis is not
/// taken as unscrollable.
pub(crate) fn axis_cannot_scroll(reading: ScrollReading, horizontal: bool) -> bool {
    let axis = if horizontal { reading.0 } else { reading.1 };
    axis == Some(UIA_SCROLL_PATTERN_NO_SCROLL)
}

/// The `ScrollPattern` calls one element scroll makes, supplied by
/// `uia::scroll` on Windows.
pub(crate) trait ScrollPatternCalls {
    /// Read the horizontal and vertical scroll percent; an axis is `None`
    /// when its getter failed.
    fn read_percent(&mut self) -> (Option<f64>, Option<f64>);
    /// One `Scroll` call in the requested direction.
    fn scroll_once(&mut self) -> anyhow::Result<()>;
}

/// `amount.max(1)` `Scroll` calls, stopping at the first error. The position
/// is read before the first call: when the requested axis reads as
/// unscrollable, nothing is called ([`ScrollStop::BeforeScroll`]). It is read
/// again only when the first call errors, to tell whether that call moved
/// anything ([`ScrollStop::FirstCall`]).
pub(crate) fn run_scroll_steps(
    calls: &mut impl ScrollPatternCalls,
    horizontal: bool,
    amount: u32,
) -> Result<(), (ScrollStop, anyhow::Error)> {
    let mut read = || {
        let (horizontal, vertical) = calls.read_percent();
        scroll_reading(horizontal, vertical)
    };
    let before = read();
    if axis_cannot_scroll(before, horizontal) {
        return Err((
            ScrollStop::BeforeScroll,
            anyhow::anyhow!("UIA element does not scroll on this axis"),
        ));
    }
    for completed in 0..amount.max(1) {
        if let Err(error) = calls.scroll_once() {
            let stop = if completed == 0 {
                let (horizontal, vertical) = calls.read_percent();
                ScrollStop::FirstCall {
                    moved: movement_seen(before, scroll_reading(horizontal, vertical)),
                }
            } else {
                ScrollStop::Later { completed }
            };
            return Err((stop, error));
        }
    }
    Ok(())
}

/// The element's own scroll stopped part-way: some steps count as applied
/// (see [`SemanticScroll::Partial`]), so this is neither the no-effects stale
/// refusal nor a reason to send the wheel on top of them. Its effect is `partial`, with the
/// completed steps as `delivery.delivered_count`.
#[derive(Debug)]
pub(crate) struct ScrollIncomplete {
    pub hwnd: u64,
    pub element_index: usize,
    pub completed: u32,
    pub amount: u32,
    pub error: String,
}

/// The code of a [`ScrollIncomplete`].
pub(crate) const SCROLL_INCOMPLETE: &str = "scroll_incomplete";

impl std::fmt::Display for ScrollIncomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{SCROLL_INCOMPLETE}: UIA element [{}] in HWND 0x{:x} scrolled {} of {} step(s), \
             then its UIA scroll failed ({}); no wheel was sent. Read the window again \
             before acting.",
            self.element_index, self.hwnd, self.completed, self.amount, self.error
        )
    }
}

impl std::error::Error for ScrollIncomplete {}

impl ScrollIncomplete {
    fn result(&self) -> ToolResult {
        ToolResult::error(self.to_string()).with_structured(serde_json::json!({
            "code": SCROLL_INCOMPLETE,
            "effect": "partial",
            "path": "uia",
            "delivery": { "delivered_count": self.completed },
            "completed_steps": self.completed,
            "amount": self.amount,
            "wheel_sent": false,
            "window_id": self.hwnd,
            "element_index": self.element_index,
            "retryable": false,
            "escalation": {
                "recommended": "verify_state",
                "reason": "The element scrolled part of the way before its UIA scroll failed, and no wheel was sent; read the window again before scrolling more.",
            },
        }))
    }
}

/// The element's first UIA `Scroll` call returned an error and its position
/// doesn't show whether it moved. It may have scrolled, so no wheel is sent
/// and no count is claimed. Its effect is `unverifiable`.
#[derive(Debug)]
pub(crate) struct ScrollUncertain {
    pub hwnd: u64,
    pub element_index: usize,
    pub amount: u32,
    pub error: String,
}

/// The code of a [`ScrollUncertain`].
pub(crate) const SCROLL_UNCERTAIN: &str = "scroll_uncertain";

impl std::fmt::Display for ScrollUncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{SCROLL_UNCERTAIN}: UIA element [{}] in HWND 0x{:x} returned an error from its \
             first UIA scroll call ({}) and its position does not show whether it moved; no \
             wheel was sent. Read the window again before acting.",
            self.element_index, self.hwnd, self.error
        )
    }
}

impl std::error::Error for ScrollUncertain {}

impl ScrollUncertain {
    fn result(&self) -> ToolResult {
        ToolResult::error(self.to_string()).with_structured(serde_json::json!({
            "code": SCROLL_UNCERTAIN,
            "effect": "unverifiable",
            "path": "uia",
            "delivery_mode": "background",
            "amount": self.amount,
            "wheel_sent": false,
            "window_id": self.hwnd,
            "element_index": self.element_index,
            "retryable": false,
            "escalation": {
                "recommended": "verify_state",
                "reason": "The element's UIA scroll returned an error and its position does not show whether it moved; no wheel was sent. Read the window again before scrolling more.",
            },
        }))
    }
}

/// What a foreground scroll addressed by an element needs from Windows.
pub(crate) trait ScrollTarget {
    /// Read the element's rectangle live.
    fn live_bounds(&mut self) -> LiveBounds;
    /// The element's own UIA scroll.
    fn semantic_scroll(&mut self) -> SemanticScroll;
    /// The on-screen check (and scroll into view) for the wheel point.
    fn onscreen_point(&mut self, point: (i32, i32)) -> Result<(i32, i32), ToolResult>;
    /// Send the wheel at a screen point.
    fn wheel(&mut self, point: (i32, i32)) -> anyhow::Result<()>;
}

/// How a foreground element scroll was delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ElementScroll {
    /// The element's own UIA scroll.
    Semantic,
    /// The wheel, at this screen point.
    Wheel((i32, i32)),
}

/// A foreground scroll addressed by an element. A control UIA reports as gone
/// is refused as stale before its own scroll is tried. When that scroll
/// stops part-way, the result is [`ScrollIncomplete`]; when its first call
/// errored without showing whether it moved, [`ScrollUncertain`]. Neither
/// sends a wheel or reads the element again, so a control removed during the
/// call can't turn into a stale refusal. When it dispatched nothing, the element must still be located live (a
/// non-empty live rectangle) before any wheel: one that can't be is refused,
/// never sent to the window centre, even with a caller `pixel`. The wheel then
/// goes to the caller's `pixel` exactly as given, or else to the live centre
/// after the on-screen check.
pub(crate) fn foreground_element_scroll(
    target: &mut impl ScrollTarget,
    pixel: Option<(i32, i32)>,
    hwnd: u64,
    element_index: usize,
    amount: u32,
) -> anyhow::Result<ElementScroll> {
    require_live(target.live_bounds())?;
    match target.semantic_scroll() {
        SemanticScroll::Done => return Ok(ElementScroll::Semantic),
        SemanticScroll::Partial { completed, error } => {
            return Err(ScrollIncomplete {
                hwnd,
                element_index,
                completed,
                amount,
                error,
            }
            .into())
        }
        SemanticScroll::Uncertain { error } => {
            return Err(ScrollUncertain {
                hwnd,
                element_index,
                amount,
                error,
            }
            .into())
        }
        SemanticScroll::NotDispatched => {}
    }
    // Nothing has been sent yet, so a refusal here is still true.
    let rect = live_target_rect(target.live_bounds())?;
    let point = match pixel {
        Some(point) => point,
        None => target
            .onscreen_point(live_center(rect))
            .map_err(FocusRefused)?,
    };
    target.wheel(point)?;
    Ok(ElementScroll::Wheel(point))
}

/// The centre `pick` for [`foreground_tap`].
pub(crate) fn live_center(rect: (i32, i32, i32, i32)) -> (i32, i32) {
    ((rect.0 + rect.2) / 2, (rect.1 + rect.3) / 2)
}

/// The dropdown-arrow half of an MSAA split button, the `pick` for its
/// `action:"expand"`: 4 px inside the right edge, at mid height.
pub(crate) fn dropdown_half((_left, top, right, bottom): (i32, i32, i32, i32)) -> (i32, i32) {
    (right - 4, (top + bottom) / 2)
}

/// The tool reply for an error from a foreground element route.
pub(crate) fn error_result(error: anyhow::Error, stale: impl FnOnce() -> ToolResult) -> ToolResult {
    if error.is::<DeadElement>() {
        return stale();
    }
    if let Some(failed) = error.downcast_ref::<FailedAfterActivation>() {
        return failed.result();
    }
    if let Some(incomplete) = error.downcast_ref::<ScrollIncomplete>() {
        return incomplete.result();
    }
    if let Some(uncertain) = error.downcast_ref::<ScrollUncertain>() {
        return uncertain.result();
    }
    match error.downcast::<FocusRefused>() {
        Ok(FocusRefused(result)) => result,
        Err(error) => ToolResult::error(error.to_string()),
    }
}

/// What establishing focus on the element needs from Windows.
pub(crate) trait FocusTarget {
    /// UIA `SetFocus` on the element.
    fn set_focus(&mut self) -> anyhow::Result<()>;
    /// Wait for the element to report keyboard focus.
    fn wait_for_focus(&mut self, timeout: Duration) -> bool;
    /// Read the element's rectangle live.
    fn live_bounds(&mut self) -> LiveBounds;
    /// The point to click for `center`, after the same on-screen check (and
    /// scroll into view) a click on the element gets; `Err` is a refusal.
    fn onscreen_point(&mut self, center: (i32, i32)) -> Result<(i32, i32), ToolResult>;
    /// A real left click at a screen point.
    fn click(&mut self, point: (i32, i32)) -> anyhow::Result<()>;
}

/// Establish exact UIA child focus after the owning top-level window is
/// already confirmed foreground. Chromium providers sometimes reject UIA
/// `SetFocus`; then a real click at the centre of the element's live
/// rectangle is the bounded fallback. Both routes require keyboard-focus
/// read-back before any keyboard input is allowed to leave the driver.
pub(crate) fn focus_for_foreground(
    target: &mut impl FocusTarget,
    hwnd: u64,
    element_index: usize,
) -> anyhow::Result<()> {
    let set_focus = target.set_focus();
    if set_focus.is_ok() && target.wait_for_focus(Duration::from_millis(350)) {
        return Ok(());
    }

    // Everything below runs after activation: failures say so, and none of
    // them is the no-effects stale refusal.
    let failed = |code, message: String, focus_click| FailedAfterActivation {
        code,
        message,
        hwnd,
        element_index,
        focus_click,
    };
    let mut focus_click = FocusClick::NotSent;
    match target.live_bounds() {
        LiveBounds::NotAvailable => {
            return Err(failed(
                FOREGROUND_ELEMENT_LOST,
                format!(
                    "HWND 0x{hwnd:x} was brought to the front, then UIA element \
                     [{element_index}] was no longer available; no keys or text were sent. \
                     Read the window again before acting."
                ),
                FocusClick::NotSent,
            )
            .into())
        }
        LiveBounds::Rect(rect) => {
            if let Some(center) = rect_center(rect) {
                let point = match target.onscreen_point(center) {
                    Ok(point) => point,
                    Err(refusal) => {
                        return Err(failed(
                            FOREGROUND_UNAVAILABLE,
                            format!(
                                "HWND 0x{hwnd:x} was brought to the front, but UIA element \
                                 [{element_index}] could not take focus and its focus click was \
                                 refused ({}); no keys or text were sent",
                                FocusRefused(refusal)
                            ),
                            FocusClick::NotSent,
                        )
                        .into())
                    }
                };
                if let Err(error) = target.click(point) {
                    // The click may have gone out in part or in full, and a
                    // completed one may have moved the foreground.
                    return Err(failed(
                        FOREGROUND_UNAVAILABLE,
                        format!(
                            "HWND 0x{hwnd:x} was brought to the front, but UIA element \
                             [{element_index}] could not take focus and its focus click \
                             failed ({error}); no keys or text were sent"
                        ),
                        FocusClick::Uncertain,
                    )
                    .into());
                }
                focus_click = FocusClick::Sent;
                if target.wait_for_focus(Duration::from_millis(500)) {
                    return Ok(());
                }
            }
        }
        // No live point to click: a cached one could hit another control.
        LiveBounds::Unknown => {}
    }

    let detail = set_focus
        .err()
        .map(|error| format!("; UIA SetFocus failed: {error}"))
        .unwrap_or_default();
    Err(failed(
        FOREGROUND_UNAVAILABLE,
        format!(
            "Windows did not confirm keyboard focus on exact UIA element [{element_index}] in \
             HWND 0x{hwnd:x}{detail}; the window was brought to the front and no keyboard \
             input was sent"
        ),
        focus_click,
    )
    .into())
}

/// The whole foreground keyboard route for an element: one live read before
/// activation, then the keyboard `transaction` (activation, then `focus`, then
/// the keys). A control UIA reports as gone is refused as stale before the
/// transaction runs, so nothing is activated or sent. With `check_visible`
/// (typing), the live centre must also pass the on-screen check before
/// activation; an unreadable rectangle skips that check, never falling back to
/// the point cached at read time. Failures inside the transaction are
/// [`FailedAfterActivation`].
pub(crate) fn foreground_element_keys(
    target: &mut impl FocusTarget,
    hwnd: u64,
    element_index: usize,
    check_visible: bool,
    transaction: impl FnOnce(&mut dyn FnMut() -> anyhow::Result<()>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    match target.live_bounds() {
        LiveBounds::NotAvailable => return Err(DeadElement.into()),
        LiveBounds::Rect(rect) if check_visible => {
            target
                .onscreen_point(live_center(rect))
                .map_err(FocusRefused)?;
        }
        LiveBounds::Rect(_) | LiveBounds::Unknown => {}
    }
    transaction(&mut || focus_for_foreground(target, hwnd, element_index))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CACHED_RECT: (i32, i32, i32, i32) = (100, 100, 200, 140);

    /// A fake element: `SetFocus` fails, then the live rectangle and the
    /// focus after a click decide the rest. Records every click.
    struct FakeElement {
        set_focus_ok: bool,
        focused_after_click: bool,
        live: LiveBounds,
        /// What every live read after the first returns, when it changes.
        live_later: Option<LiveBounds>,
        live_reads: usize,
        clicked: Vec<(i32, i32)>,
        /// `click` records the click, then returns this error.
        click_error: Option<&'static str>,
        reads: Vec<&'static str>,
    }

    impl FakeElement {
        fn new(live: LiveBounds) -> Self {
            Self {
                set_focus_ok: false,
                focused_after_click: true,
                live,
                live_later: None,
                live_reads: 0,
                clicked: Vec::new(),
                click_error: None,
                reads: Vec::new(),
            }
        }
    }

    impl FocusTarget for FakeElement {
        fn set_focus(&mut self) -> anyhow::Result<()> {
            self.reads.push("set_focus");
            if self.set_focus_ok {
                Ok(())
            } else {
                anyhow::bail!("UIA SetFocus failed: 0x80040201")
            }
        }

        fn wait_for_focus(&mut self, _timeout: Duration) -> bool {
            self.set_focus_ok || (!self.clicked.is_empty() && self.focused_after_click)
        }

        fn live_bounds(&mut self) -> LiveBounds {
            self.reads.push("live_bounds");
            self.live_reads += 1;
            match self.live_later {
                Some(later) if self.live_reads > 1 => later,
                _ => self.live,
            }
        }

        fn onscreen_point(&mut self, center: (i32, i32)) -> Result<(i32, i32), ToolResult> {
            self.reads.push("onscreen");
            Ok(center)
        }

        fn click(&mut self, point: (i32, i32)) -> anyhow::Result<()> {
            self.clicked.push(point);
            match self.click_error {
                Some(error) => anyhow::bail!("{error}"),
                None => Ok(()),
            }
        }
    }

    fn stale() -> ToolResult {
        ToolResult::error("stale").with_structured(serde_json::json!({
            "status": "refused",
            "refusal": { "code": "stale_element_token" },
        }))
    }

    /// The keyboard transaction as `keyboard.rs` runs it: activation, then the
    /// focus callback, then the keys only if focus was established.
    fn transaction<'a>(
        events: &'a std::cell::RefCell<Vec<&'static str>>,
    ) -> impl FnOnce(&mut dyn FnMut() -> anyhow::Result<()>) -> anyhow::Result<()> + 'a {
        move |focus| {
            events.borrow_mut().push("activate");
            focus()?;
            events.borrow_mut().push("keys");
            Ok(())
        }
    }

    /// Activation, then a failed SetFocus, then the element gone: the reply
    /// says the window was activated and no keys were sent. It is not the
    /// no-effects stale refusal and not `status: "refused"`.
    #[test]
    fn an_element_lost_after_activation_is_an_honest_failure_not_stale() {
        for first in [LiveBounds::Rect(CACHED_RECT), LiveBounds::Unknown] {
            let mut element = FakeElement::new(first);
            element.live_later = Some(LiveBounds::NotAvailable);
            let events = std::cell::RefCell::new(Vec::new());
            let error = foreground_element_keys(&mut element, 0x10, 3, false, transaction(&events))
                .unwrap_err();
            assert_eq!(events.into_inner(), vec!["activate"], "{first:?}");
            assert!(element.clicked.is_empty(), "{first:?}: clicked");
            let result = error_result(error, || panic!("{first:?}: mapped to the stale refusal"));
            assert_eq!(result.is_error, Some(true));
            let structured = result.structured_content.unwrap();
            assert_eq!(structured["code"], FOREGROUND_ELEMENT_LOST, "{first:?}");
            assert_eq!(structured["effect"], "failed");
            assert_eq!(structured["activated"], true);
            assert_eq!(structured["keystrokes_sent"], false);
            assert_eq!(structured["retryable"], false);
            assert!(structured.get("status").is_none(), "{structured}");
        }
    }

    /// Activation, then a failed SetFocus, then a fallback click that went out
    /// in part and returned an error: the honest post-activation failure, with
    /// the click marked uncertain, and no keys sent.
    #[test]
    fn a_failed_focus_click_is_an_honest_failure_after_activation() {
        let mut element = FakeElement::new(LiveBounds::Rect(CACHED_RECT));
        element.click_error = Some("SendInput sent 1 of 2 mouse events");
        let events = std::cell::RefCell::new(Vec::new());
        let error = foreground_element_keys(&mut element, 0x10, 3, false, transaction(&events))
            .unwrap_err();
        assert_eq!(events.into_inner(), vec!["activate"]);
        assert_eq!(element.clicked, vec![rect_center(CACHED_RECT).unwrap()]);
        let result = error_result(error, || panic!("mapped to the stale refusal"));
        assert_eq!(result.is_error, Some(true));
        let text = format!("{:?}", result.content);
        assert!(text.contains("SendInput sent 1 of 2"), "{text}");
        assert!(text.contains("no keys or text were sent"), "{text}");
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["code"], FOREGROUND_UNAVAILABLE, "{structured}");
        assert_eq!(structured["effect"], "failed");
        assert_eq!(structured["activated"], true);
        assert_eq!(structured["keystrokes_sent"], false);
        assert_eq!(structured["focus_click"], "uncertain");
        assert_eq!(structured["retryable"], false);
        assert_eq!(structured["escalation"]["recommended"], "verify_state");
        assert!(structured.get("status").is_none(), "{structured}");
    }

    /// `focus_click` says whether the fallback click went out.
    #[test]
    fn the_post_activation_failure_says_whether_the_focus_click_went_out() {
        let focus_click = |element: &mut FakeElement| {
            let error = focus_for_foreground(element, 0x10, 3).unwrap_err();
            error_result(error, || unreachable!())
                .structured_content
                .unwrap()["focus_click"]
                .clone()
        };
        // Clicked, but focus never confirmed.
        let mut element = FakeElement::new(LiveBounds::Rect(CACHED_RECT));
        element.focused_after_click = false;
        assert_eq!(focus_click(&mut element), "sent");
        // No live point, so no click.
        assert_eq!(
            focus_click(&mut FakeElement::new(LiveBounds::Unknown)),
            "not_sent"
        );
        assert_eq!(
            focus_click(&mut FakeElement::new(LiveBounds::NotAvailable)),
            "not_sent"
        );
    }

    /// The element gone at the read before activation: stale refusal, and the
    /// transaction (activation, focus, keys) never runs.
    #[test]
    fn a_dead_element_never_starts_the_keyboard_transaction() {
        for check_visible in [false, true] {
            let mut element = FakeElement::new(LiveBounds::NotAvailable);
            let events = std::cell::RefCell::new(Vec::new());
            let error =
                foreground_element_keys(&mut element, 0x10, 3, check_visible, transaction(&events))
                    .unwrap_err();
            assert!(events.into_inner().is_empty());
            let result = error_result(error, stale);
            assert_eq!(
                result.structured_content.unwrap()["refusal"]["code"],
                "stale_element_token"
            );
        }
    }

    /// Typing checks visibility at the live centre before activation, never
    /// at the cached one; keys don't check, and an unreadable rectangle skips
    /// the check.
    #[test]
    fn typing_checks_visibility_at_the_live_centre_before_activation() {
        struct Watch(FakeElement, Vec<(i32, i32)>);
        impl FocusTarget for Watch {
            fn set_focus(&mut self) -> anyhow::Result<()> {
                Ok(())
            }
            fn wait_for_focus(&mut self, _timeout: Duration) -> bool {
                true
            }
            fn live_bounds(&mut self) -> LiveBounds {
                self.0.live_bounds()
            }
            fn onscreen_point(&mut self, point: (i32, i32)) -> Result<(i32, i32), ToolResult> {
                self.1.push(point);
                Ok(point)
            }
            fn click(&mut self, point: (i32, i32)) -> anyhow::Result<()> {
                self.0.click(point)
            }
        }
        let moved = LiveBounds::Rect((300, 400, 360, 420));
        for (live, check_visible, expected) in [
            (moved, true, vec![(330, 410)]),
            (moved, false, vec![]),
            (LiveBounds::Unknown, true, vec![]),
        ] {
            let mut element = Watch(FakeElement::new(live), Vec::new());
            let events = std::cell::RefCell::new(Vec::new());
            foreground_element_keys(&mut element, 0x10, 3, check_visible, transaction(&events))
                .unwrap();
            assert_eq!(element.1, expected, "{live:?} {check_visible}");
            assert_eq!(events.into_inner(), vec!["activate", "keys"]);
        }
    }

    #[test]
    fn element_not_available_is_dead_and_nothing_else_is() {
        assert_eq!(
            live_bounds_from(Err(UIA_E_ELEMENTNOTAVAILABLE)),
            LiveBounds::NotAvailable
        );
        // E_FAIL, RPC_E_DISCONNECTED, UIA_E_TIMEOUT, E_ACCESSDENIED.
        for other in [0x8000_4005, 0x8001_0108, 0x8013_1505, 0x8007_0005] {
            assert_eq!(
                live_bounds_from(Err(other)),
                LiveBounds::Unknown,
                "{other:x}"
            );
        }
        assert_eq!(
            live_bounds_from(Ok(CACHED_RECT)),
            LiveBounds::Rect(CACHED_RECT)
        );
    }

    /// A dead element is refused before the foreground route activates the
    /// window; an element that is alive or unreadable proceeds.
    #[test]
    fn a_dead_element_is_refused_before_activation() {
        let error = require_live(LiveBounds::NotAvailable).unwrap_err();
        assert!(error.is::<DeadElement>(), "{error}");
        require_live(LiveBounds::Rect(CACHED_RECT)).unwrap();
        require_live(LiveBounds::Unknown).unwrap();
    }

    /// SetFocus fails and the element is gone: no click at all, and the
    /// refusal is the stale-token one.
    #[test]
    fn a_failed_set_focus_on_a_dead_element_sends_no_click() {
        let mut element = FakeElement::new(LiveBounds::NotAvailable);
        let error = focus_for_foreground(&mut element, 0x10, 3).unwrap_err();
        assert!(element.clicked.is_empty(), "clicked {:?}", element.clicked);
        let failed = error
            .downcast_ref::<FailedAfterActivation>()
            .expect("{error}");
        assert_eq!(failed.code, FOREGROUND_ELEMENT_LOST);
    }

    /// SetFocus fails and the element moved since the read: the fallback
    /// click goes to the centre of its live rectangle, after the on-screen
    /// check, never to the centre cached at read time.
    #[test]
    fn a_failed_set_focus_clicks_the_live_centre_not_the_cached_one() {
        let cached_center = rect_center(CACHED_RECT).unwrap();
        let mut element = FakeElement::new(LiveBounds::Rect((300, 400, 360, 420)));
        focus_for_foreground(&mut element, 0x10, 3).unwrap();
        assert_eq!(element.clicked, vec![(330, 410)]);
        assert_ne!(element.clicked[0], cached_center);
        assert_eq!(element.reads, vec!["set_focus", "live_bounds", "onscreen"]);
    }

    /// No live rectangle (an unreadable or MSAA element, or an empty one):
    /// no click, and the existing focus refusal.
    #[test]
    fn no_live_rectangle_means_no_click() {
        for live in [LiveBounds::Unknown, LiveBounds::Rect((5, 5, 5, 9))] {
            let mut element = FakeElement::new(live);
            let error = focus_for_foreground(&mut element, 0x10, 3).unwrap_err();
            assert!(element.clicked.is_empty(), "{live:?}: clicked");
            assert!(!error.is::<DeadElement>(), "{live:?}");
            assert!(
                error.to_string().starts_with("foreground_unavailable"),
                "{error}"
            );
        }
    }

    /// An on-screen refusal of the live centre is returned as is, unclicked.
    #[test]
    fn an_offscreen_live_centre_is_refused_unclicked() {
        struct Offscreen(FakeElement);
        impl FocusTarget for Offscreen {
            fn set_focus(&mut self) -> anyhow::Result<()> {
                self.0.set_focus()
            }
            fn wait_for_focus(&mut self, timeout: Duration) -> bool {
                self.0.wait_for_focus(timeout)
            }
            fn live_bounds(&mut self) -> LiveBounds {
                self.0.live_bounds()
            }
            fn onscreen_point(&mut self, _center: (i32, i32)) -> Result<(i32, i32), ToolResult> {
                Err(ToolResult::error("not visible")
                    .with_structured(serde_json::json!({ "code": "element_not_visible" })))
            }
            fn click(&mut self, point: (i32, i32)) -> anyhow::Result<()> {
                self.0.click(point)
            }
        }
        let mut element = Offscreen(FakeElement::new(LiveBounds::Rect(CACHED_RECT)));
        let error = focus_for_foreground(&mut element, 0x10, 3).unwrap_err();
        assert!(element.0.clicked.is_empty());
        assert!(error.to_string().contains("not visible"), "{error}");
        // After activation, so an honest failure, not the refusal envelope.
        let result = error_result(error, || unreachable!("not a dead element"));
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["code"], FOREGROUND_UNAVAILABLE);
        assert_eq!(structured["activated"], true);
        assert!(structured.get("status").is_none());
    }

    /// A foreground click goes into the live rectangle; a control that is
    /// gone, unreadable or empty gets no input.
    #[test]
    fn a_foreground_click_targets_only_a_live_rectangle() {
        assert_eq!(
            live_target_rect(LiveBounds::Rect((300, 400, 360, 420))).unwrap(),
            (300, 400, 360, 420)
        );
        for refused in [
            LiveBounds::NotAvailable,
            LiveBounds::Unknown,
            LiveBounds::Rect((5, 5, 5, 9)),
        ] {
            assert!(live_target_rect(refused).is_err(), "{refused:?}");
        }
    }

    /// Records the taps a foreground click would send.
    struct FakeTap {
        live: LiveBounds,
        sent: Vec<(i32, i32)>,
        send_fails: bool,
        restored: usize,
    }

    impl FakeTap {
        fn new(live: LiveBounds) -> Self {
            Self {
                live,
                sent: Vec::new(),
                send_fails: false,
                restored: 0,
            }
        }
    }

    impl TapTarget for FakeTap {
        fn live_bounds(&mut self) -> LiveBounds {
            self.live
        }
        fn onscreen_point(&mut self, point: (i32, i32)) -> Result<(i32, i32), ToolResult> {
            Ok(point)
        }
        fn send(&mut self, point: (i32, i32)) -> anyhow::Result<()> {
            self.sent.push(point);
            if self.send_fails {
                anyhow::bail!("foreground_unavailable: activation failed part-way");
            }
            Ok(())
        }
        fn restore_foreground(&mut self) {
            self.restored += 1;
        }
    }

    /// Foreground click on an element that is no longer available: nothing
    /// is sent, and the refusal is the stale-token one.
    #[test]
    fn a_foreground_click_on_a_dead_element_sends_nothing() {
        for live in [
            LiveBounds::NotAvailable,
            LiveBounds::Unknown,
            LiveBounds::Rect((5, 5, 5, 9)),
        ] {
            let mut tap = FakeTap::new(live);
            let error = foreground_tap(&mut tap, live_center).unwrap_err();
            assert!(tap.sent.is_empty(), "{live:?}: sent {:?}", tap.sent);
            assert!(error.is::<DeadElement>(), "{live:?}: {error}");
            assert_eq!(tap.restored, 0, "{live:?}: restore started for a refusal");
        }
    }

    /// Foreground click on an element that moved since the read: the tap goes
    /// to the centre of its live rectangle, not the cached centre.
    #[test]
    fn a_foreground_click_on_a_moved_element_taps_the_live_centre() {
        let mut tap = FakeTap::new(LiveBounds::Rect((300, 400, 360, 420)));
        let point = foreground_tap(&mut tap, live_center).unwrap();
        assert_eq!(tap.sent, vec![(330, 410)]);
        assert_eq!(point, (330, 410));
        assert_ne!(point, rect_center(CACHED_RECT).unwrap());
        // The MSAA dropdown half is picked inside the live rectangle too.
        let mut tap = FakeTap::new(LiveBounds::Rect((300, 400, 360, 420)));
        foreground_tap(&mut tap, dropdown_half).unwrap();
        assert_eq!(tap.sent, vec![(356, 410)]);
    }

    /// The restore of the earlier foreground window starts once dispatch was
    /// attempted, including a send that failed part-way, and never for a
    /// step refused before dispatch (dead, unreadable, or off-screen).
    #[test]
    fn the_foreground_restore_starts_only_after_dispatch() {
        let mut tap = FakeTap::new(LiveBounds::Rect(CACHED_RECT));
        foreground_tap(&mut tap, live_center).unwrap();
        assert_eq!(tap.restored, 1);
        let mut tap = FakeTap::new(LiveBounds::Rect(CACHED_RECT));
        tap.send_fails = true;
        foreground_tap(&mut tap, live_center).unwrap_err();
        assert_eq!(
            tap.restored, 1,
            "a part-way failure still needs the restore"
        );
        struct Offscreen(FakeTap);
        impl TapTarget for Offscreen {
            fn live_bounds(&mut self) -> LiveBounds {
                self.0.live_bounds()
            }
            fn onscreen_point(&mut self, _point: (i32, i32)) -> Result<(i32, i32), ToolResult> {
                Err(ToolResult::error("not visible"))
            }
            fn send(&mut self, point: (i32, i32)) -> anyhow::Result<()> {
                self.0.send(point)
            }
            fn restore_foreground(&mut self) {
                self.0.restore_foreground()
            }
        }
        let mut tap = Offscreen(FakeTap::new(LiveBounds::Rect(CACHED_RECT)));
        foreground_tap(&mut tap, live_center).unwrap_err();
        assert_eq!((tap.0.restored, tap.0.sent.len()), (0, 0));
    }

    /// Foreground double/right-click by element: the cached point is not
    /// checked, so a control that moved with its window is not refused for
    /// its old position and a dead one reaches the live (stale) check.
    /// Background keeps the cached check.
    #[test]
    fn only_background_runs_the_cached_point_preflight() {
        let refuse = |_: (i32, i32)| -> Result<(i32, i32), ToolResult> {
            Err(ToolResult::error("element_not_visible"))
        };
        assert_eq!(
            cached_point_preflight(true, (150, 120), refuse).unwrap(),
            (150, 120)
        );
        assert!(cached_point_preflight(false, (150, 120), refuse).is_err());
        // Composed with the tap: a moved element gets the live centre; a dead
        // one is stale even though its cached point would have been refused.
        let cached = cached_point_preflight(true, (150, 120), refuse).unwrap();
        let mut tap = FakeTap::new(LiveBounds::Rect((300, 400, 360, 420)));
        assert_eq!(foreground_tap(&mut tap, live_center).unwrap(), (330, 410));
        assert_ne!(cached, (330, 410));
        let mut tap = FakeTap::new(LiveBounds::NotAvailable);
        assert!(foreground_tap(&mut tap, live_center)
            .unwrap_err()
            .is::<DeadElement>());
    }

    /// Records a foreground element scroll, including what the element's own
    /// scroll moved before it returned.
    struct FakeScroll {
        live: LiveBounds,
        /// What every live read after the first returns, when it changes.
        live_later: Option<LiveBounds>,
        live_reads: usize,
        semantic: SemanticScroll,
        /// `Scroll` steps the element's own scroll applied.
        scrolled_steps: u32,
        events: Vec<String>,
    }

    impl FakeScroll {
        fn new(live: LiveBounds, semantic: SemanticScroll) -> Self {
            Self {
                live,
                live_later: None,
                live_reads: 0,
                semantic,
                scrolled_steps: 0,
                events: Vec::new(),
            }
        }

        fn wheels(&self) -> usize {
            self.events
                .iter()
                .filter(|e| e.starts_with("wheel"))
                .count()
        }
    }

    impl ScrollTarget for FakeScroll {
        fn live_bounds(&mut self) -> LiveBounds {
            self.events.push("live".into());
            self.live_reads += 1;
            match self.live_later {
                Some(later) if self.live_reads > 1 => later,
                _ => self.live,
            }
        }
        fn semantic_scroll(&mut self) -> SemanticScroll {
            self.events.push("semantic".into());
            self.scrolled_steps += match &self.semantic {
                SemanticScroll::Done => 3,
                SemanticScroll::NotDispatched | SemanticScroll::Uncertain { .. } => 0,
                SemanticScroll::Partial { completed, .. } => *completed,
            };
            self.semantic.clone()
        }
        fn onscreen_point(&mut self, point: (i32, i32)) -> Result<(i32, i32), ToolResult> {
            self.events.push("onscreen".into());
            Ok(point)
        }
        fn wheel(&mut self, point: (i32, i32)) -> anyhow::Result<()> {
            self.events.push(format!("wheel {},{}", point.0, point.1));
            Ok(())
        }
    }

    fn scroll(target: &mut FakeScroll, pixel: Option<(i32, i32)>) -> anyhow::Result<ElementScroll> {
        foreground_element_scroll(target, pixel, 0x10, 3, 3)
    }

    /// A dead element is refused before its own scroll; a live one whose own
    /// scroll dispatched nothing gets the wheel at its live centre, never the
    /// window centre; one that can't be located live gets no wheel.
    #[test]
    fn a_foreground_element_scroll_wheels_only_at_the_live_element() {
        use SemanticScroll::{Done, NotDispatched};
        let mut target = FakeScroll::new(LiveBounds::NotAvailable, Done);
        assert!(scroll(&mut target, None).unwrap_err().is::<DeadElement>());
        assert_eq!(target.events, vec!["live"]);
        let moved = LiveBounds::Rect((300, 400, 360, 420));
        let mut target = FakeScroll::new(moved, NotDispatched);
        assert_eq!(
            scroll(&mut target, None).unwrap(),
            ElementScroll::Wheel((330, 410))
        );
        assert_eq!(
            target.events,
            vec!["live", "semantic", "live", "onscreen", "wheel 330,410"]
        );
        let mut target = FakeScroll::new(moved, Done);
        assert_eq!(scroll(&mut target, None).unwrap(), ElementScroll::Semantic);
        assert_eq!(target.wheels(), 0);
        for unlocatable in [LiveBounds::Unknown, LiveBounds::Rect((5, 5, 5, 9))] {
            let mut target = FakeScroll::new(unlocatable, NotDispatched);
            let error = scroll(&mut target, None).unwrap_err();
            assert!(error.is::<DeadElement>(), "{unlocatable:?}");
            assert_eq!(target.wheels(), 0, "{:?}", target.events);
        }
        // A live element with the caller's x,y: the wheel goes there exactly,
        // without an on-screen check of that point.
        let mut target = FakeScroll::new(moved, NotDispatched);
        assert_eq!(
            scroll(&mut target, Some((7, 8))).unwrap(),
            ElementScroll::Wheel((7, 8))
        );
        assert_eq!(target.events, vec!["live", "semantic", "live", "wheel 7,8"]);
    }

    /// The caller's x,y picks where the wheel goes but doesn't exempt the
    /// element from being located live: unreadable, empty, or gone after its
    /// own scroll dispatched nothing, it gets no wheel and the stale refusal.
    #[test]
    fn scroll_coordinates_do_not_skip_the_live_element() {
        let mut gone_later = FakeScroll::new(
            LiveBounds::Rect((300, 400, 360, 420)),
            SemanticScroll::NotDispatched,
        );
        gone_later.live_later = Some(LiveBounds::NotAvailable);
        for mut target in [
            FakeScroll::new(LiveBounds::Unknown, SemanticScroll::NotDispatched),
            FakeScroll::new(
                LiveBounds::Rect((5, 5, 5, 9)),
                SemanticScroll::NotDispatched,
            ),
            gone_later,
        ] {
            let error = scroll(&mut target, Some((7, 8))).unwrap_err();
            assert_eq!(target.wheels(), 0, "{:?}", target.events);
            let result = error_result(error, stale);
            assert_eq!(
                result.structured_content.unwrap()["refusal"]["code"],
                "stale_element_token",
                "{:?}",
                target.events
            );
        }
    }

    /// The element's own scroll moved one step, then failed, and the element
    /// is gone: no wheel on top of what moved, and a `partial` result that
    /// counts the step, never the no-effects stale refusal.
    #[test]
    fn a_partly_completed_element_scroll_is_reported_partial_not_stale() {
        for pixel in [None, Some((7, 8))] {
            let mut target = FakeScroll::new(
                LiveBounds::Rect((300, 400, 360, 420)),
                SemanticScroll::Partial {
                    completed: 1,
                    error: "UIA scroll failed: 0x80040201".into(),
                },
            );
            target.live_later = Some(LiveBounds::NotAvailable);
            let error = scroll(&mut target, pixel).unwrap_err();
            assert_eq!(target.scrolled_steps, 1);
            assert_eq!(target.wheels(), 0, "{pixel:?}: {:?}", target.events);
            let result = error_result(error, || panic!("{pixel:?}: mapped to the stale refusal"));
            assert_eq!(result.is_error, Some(true));
            let structured = result.structured_content.unwrap();
            assert_eq!(structured["code"], SCROLL_INCOMPLETE, "{structured}");
            assert_eq!(structured["effect"], "partial");
            assert_eq!(
                structured["delivery"],
                serde_json::json!({ "delivered_count": 1 })
            );
            assert_eq!(structured["completed_steps"], 1);
            assert_eq!(structured["amount"], 3);
            assert_eq!(structured["wheel_sent"], false);
            assert_eq!(structured["retryable"], false);
            assert_eq!(structured["escalation"]["recommended"], "verify_state");
            assert!(structured.get("status").is_none(), "{structured}");
            assert!(structured.get("refusal").is_none(), "{structured}");
        }
    }

    /// Not sent is `NotDispatched`; a first-call error is one known step when
    /// the position moved and `Uncertain` when it doesn't show; a later error
    /// keeps the accepted calls.
    #[test]
    fn semantic_scroll_from_tells_not_sent_known_steps_and_uncertain_apart() {
        let error = || "UIA scroll failed".to_string();
        assert_eq!(semantic_scroll_from(Ok(())), SemanticScroll::Done);
        assert_eq!(
            semantic_scroll_from(Err((ScrollStop::BeforeScroll, error()))),
            SemanticScroll::NotDispatched
        );
        assert_eq!(
            semantic_scroll_from(Err((ScrollStop::FirstCall { moved: true }, error()))),
            SemanticScroll::Partial {
                completed: 1,
                error: error()
            }
        );
        assert_eq!(
            semantic_scroll_from(Err((ScrollStop::FirstCall { moved: false }, error()))),
            SemanticScroll::Uncertain { error: error() }
        );
        for completed in [1, 2] {
            assert_eq!(
                semantic_scroll_from(Err((ScrollStop::Later { completed }, error()))),
                SemanticScroll::Partial {
                    completed,
                    error: error()
                }
            );
        }
    }

    /// A scripted `ScrollPattern`: the readings it returns in order, and the
    /// result of each `Scroll` call. Records every call.
    struct FakePattern {
        readings: Vec<(Option<f64>, Option<f64>)>,
        scrolls: Vec<bool>,
        events: Vec<&'static str>,
    }

    impl FakePattern {
        fn new(readings: Vec<(Option<f64>, Option<f64>)>, scrolls: Vec<bool>) -> Self {
            Self {
                readings,
                scrolls,
                events: Vec::new(),
            }
        }

        fn scroll_calls(&self) -> usize {
            self.events.iter().filter(|e| **e == "scroll").count()
        }
    }

    impl ScrollPatternCalls for FakePattern {
        fn read_percent(&mut self) -> (Option<f64>, Option<f64>) {
            self.events.push("read");
            self.readings.remove(0)
        }
        fn scroll_once(&mut self) -> anyhow::Result<()> {
            self.events.push("scroll");
            if self.scrolls.remove(0) {
                Ok(())
            } else {
                anyhow::bail!("UIA scroll failed: 0x80040201")
            }
        }
    }

    /// The element's own scroll through the pure step loop, classified.
    fn steps(pattern: &mut FakePattern, horizontal: bool, amount: u32) -> SemanticScroll {
        semantic_scroll_from(
            run_scroll_steps(pattern, horizontal, amount)
                .map_err(|(stop, error)| (stop, error.to_string())),
        )
    }

    /// The first call errored after the vertical position moved, and the
    /// horizontal getter then failed: the readable axis still shows the move,
    /// so it is one known step.
    #[test]
    fn a_move_on_one_axis_shows_through_an_unreadable_other_axis() {
        assert!(movement_seen((Some(0.0), Some(40.0)), (None, Some(55.0))));
        assert!(movement_seen((Some(10.0), None), (Some(25.0), Some(40.0))));
        let mut pattern = FakePattern::new(
            vec![(Some(0.0), Some(40.0)), (None, Some(55.0))],
            vec![false],
        );
        assert_eq!(
            steps(&mut pattern, false, 3),
            SemanticScroll::Partial {
                completed: 1,
                error: "UIA scroll failed: 0x80040201".into()
            }
        );
    }

    /// The `-1` axis value compares like any other, on either axis.
    #[test]
    fn a_move_on_either_axis_is_movement_and_an_unchanged_one_is_not() {
        assert!(movement_seen(
            (Some(-1.0), Some(40.0)),
            (Some(-1.0), Some(55.0))
        ));
        assert!(movement_seen(
            (Some(10.0), Some(-1.0)),
            (Some(25.0), Some(-1.0))
        ));
        assert!(!movement_seen(
            (Some(-1.0), Some(40.0)),
            (Some(-1.0), Some(40.0))
        ));
        assert!(!movement_seen((None, None), (Some(0.0), Some(40.0))));
    }

    /// NaN and infinity are unreadable, never a position.
    #[test]
    fn non_finite_scroll_readings_count_as_unreadable() {
        assert_eq!(
            scroll_reading(Some(f64::NAN), Some(f64::INFINITY)),
            (None, None)
        );
        assert_eq!(
            scroll_reading(Some(f64::NEG_INFINITY), Some(40.0)),
            (None, Some(40.0))
        );
        assert_eq!(
            scroll_reading(Some(-1.0), Some(100.0)),
            (Some(-1.0), Some(100.0))
        );
        // A getter that went from a number to infinity shows no move.
        let mut pattern = FakePattern::new(
            vec![(Some(0.0), Some(40.0)), (Some(0.0), Some(f64::INFINITY))],
            vec![false],
        );
        assert!(matches!(
            steps(&mut pattern, false, 3),
            SemanticScroll::Uncertain { .. }
        ));
    }

    /// The scroll reply a foreground element scroll gets for `semantic`,
    /// with the element removed during the call.
    fn reply_after_removal(
        semantic: SemanticScroll,
        pixel: Option<(i32, i32)>,
    ) -> (ToolResult, FakeScroll) {
        let mut target = FakeScroll::new(LiveBounds::Rect((300, 400, 360, 420)), semantic);
        target.live_later = Some(LiveBounds::NotAvailable);
        let error = scroll(&mut target, pixel).unwrap_err();
        let result = error_result(error, || panic!("{pixel:?}: mapped to the stale refusal"));
        (result, target)
    }

    fn assert_scroll_uncertain(result: &ToolResult) {
        assert_eq!(result.is_error, Some(true));
        let text = format!("{:?}", result.content);
        assert!(
            text.contains("returned an error from its first UIA scroll call"),
            "{text}"
        );
        assert!(text.contains("no wheel was sent"), "{text}");
        let structured = result.structured_content.as_ref().unwrap();
        assert_eq!(structured["code"], SCROLL_UNCERTAIN, "{structured}");
        assert_eq!(structured["effect"], "unverifiable");
        assert_eq!(structured["path"], "uia");
        assert_eq!(structured["delivery_mode"], "background");
        assert_eq!(structured["amount"], 3);
        assert_eq!(structured["wheel_sent"], false);
        assert_eq!(structured["window_id"], 0x10);
        assert_eq!(structured["element_index"], 3);
        assert_eq!(structured["retryable"], false);
        assert_eq!(structured["escalation"]["recommended"], "verify_state");
        for absent in ["delivery", "completed_steps", "status", "refusal"] {
            assert!(structured.get(absent).is_none(), "{absent}: {structured}");
        }
    }

    /// The first call errored and both after-readings are unreadable (the
    /// control may have been removed by the call): uncertain, no wheel, and
    /// the element is not read again, so it can't become a stale refusal.
    #[test]
    fn a_first_call_error_with_no_reading_after_is_uncertain_and_never_stale() {
        let mut pattern =
            FakePattern::new(vec![(Some(0.0), Some(40.0)), (None, None)], vec![false]);
        let semantic = steps(&mut pattern, false, 3);
        assert_eq!(pattern.scroll_calls(), 1);
        assert_eq!(
            semantic,
            SemanticScroll::Uncertain {
                error: "UIA scroll failed: 0x80040201".into()
            }
        );
        for pixel in [None, Some((7, 8))] {
            let (result, target) = reply_after_removal(semantic.clone(), pixel);
            assert_scroll_uncertain(&result);
            assert_eq!(target.events, vec!["live", "semantic"], "{pixel:?}");
            assert_eq!(target.wheels(), 0);
        }
    }

    /// The first call errored and the position reads back unchanged: the
    /// provider may update it late, so it is uncertain, and no wheel follows.
    #[test]
    fn a_first_call_error_with_the_position_unchanged_is_uncertain_with_no_wheel() {
        for before in [(Some(0.0), Some(40.0)), (Some(-1.0), Some(40.0))] {
            let mut pattern = FakePattern::new(vec![before, before], vec![false]);
            let semantic = steps(&mut pattern, false, 3);
            assert!(
                matches!(semantic, SemanticScroll::Uncertain { .. }),
                "{before:?}: {semantic:?}"
            );
            let mut target = FakeScroll::new(LiveBounds::Rect((300, 400, 360, 420)), semantic);
            let error = scroll(&mut target, None).unwrap_err();
            assert_eq!(target.wheels(), 0, "{:?}", target.events);
            assert_scroll_uncertain(&error_result(error, || panic!("stale")));
        }
    }

    /// The requested axis reads `-1` ("no scroll on this axis"): no `Scroll`
    /// call, so nothing was sent and the wheel may follow. `-1` on the other
    /// axis, or an unreadable requested axis, still calls `Scroll`.
    #[test]
    fn an_unscrollable_requested_axis_sends_no_scroll_and_allows_the_wheel() {
        assert!(axis_cannot_scroll((Some(-1.0), Some(40.0)), true));
        assert!(axis_cannot_scroll((Some(0.0), Some(-1.0)), false));
        assert!(!axis_cannot_scroll((Some(-1.0), Some(40.0)), false));
        assert!(!axis_cannot_scroll((Some(0.0), Some(-1.0)), true));
        assert!(!axis_cannot_scroll((None, None), true));

        for (reading, horizontal) in [
            ((Some(-1.0), Some(40.0)), true),
            ((Some(0.0), Some(-1.0)), false),
        ] {
            let mut pattern = FakePattern::new(vec![reading], vec![]);
            let semantic = steps(&mut pattern, horizontal, 3);
            assert_eq!(pattern.scroll_calls(), 0, "{reading:?}");
            assert_eq!(semantic, SemanticScroll::NotDispatched, "{reading:?}");
            let mut target = FakeScroll::new(LiveBounds::Rect((300, 400, 360, 420)), semantic);
            assert_eq!(
                scroll(&mut target, None).unwrap(),
                ElementScroll::Wheel((330, 410))
            );
            assert_eq!(target.wheels(), 1);
        }

        for (reading, horizontal) in [
            ((Some(-1.0), Some(40.0)), false),
            ((Some(0.0), Some(-1.0)), true),
            ((None, None), true),
            ((None, None), false),
        ] {
            let mut pattern = FakePattern::new(vec![reading], vec![true, true, true]);
            assert_eq!(
                steps(&mut pattern, horizontal, 3),
                SemanticScroll::Done,
                "{reading:?} {horizontal}"
            );
            assert_eq!(pattern.scroll_calls(), 3, "{reading:?} {horizontal}");
        }
    }

    /// A later call's error keeps the accepted calls, without reading the
    /// position again.
    #[test]
    fn a_later_scroll_error_counts_the_accepted_calls() {
        let mut pattern = FakePattern::new(vec![(Some(0.0), Some(40.0))], vec![true, true, false]);
        assert_eq!(
            steps(&mut pattern, false, 3),
            SemanticScroll::Partial {
                completed: 2,
                error: "UIA scroll failed: 0x80040201".into()
            }
        );
        assert_eq!(pattern.events, vec!["read", "scroll", "scroll", "scroll"]);
    }

    /// A SetFocus that succeeds needs no click and no live read.
    #[test]
    fn a_successful_set_focus_needs_no_click() {
        let mut element = FakeElement::new(LiveBounds::NotAvailable);
        element.set_focus_ok = true;
        focus_for_foreground(&mut element, 0x10, 3).unwrap();
        assert!(element.clicked.is_empty());
        assert_eq!(element.reads, vec!["set_focus"]);
    }
}
