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
    target.send(point)?;
    Ok(point)
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

    match target.live_bounds() {
        LiveBounds::NotAvailable => return Err(DeadElement.into()),
        LiveBounds::Rect(rect) => {
            if let Some(center) = rect_center(rect) {
                let point = target.onscreen_point(center).map_err(FocusRefused)?;
                target.click(point)?;
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
    anyhow::bail!(
        "foreground_unavailable: Windows did not confirm keyboard focus on exact UIA element \
         [{element_index}] in HWND 0x{hwnd:x}{detail}; no keyboard input was sent"
    )
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
        clicked: Vec<(i32, i32)>,
        reads: Vec<&'static str>,
    }

    impl FakeElement {
        fn new(live: LiveBounds) -> Self {
            Self {
                set_focus_ok: false,
                focused_after_click: true,
                live,
                clicked: Vec::new(),
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
            self.live
        }

        fn onscreen_point(&mut self, center: (i32, i32)) -> Result<(i32, i32), ToolResult> {
            self.reads.push("onscreen");
            Ok(center)
        }

        fn click(&mut self, point: (i32, i32)) -> anyhow::Result<()> {
            self.clicked.push(point);
            Ok(())
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
        assert!(error.is::<DeadElement>(), "{error}");
        let result = error_result(error, || {
            ToolResult::error("stale").with_structured(serde_json::json!({
                "status": "refused",
                "refusal": { "code": "stale_element_token" },
            }))
        });
        assert_eq!(
            result.structured_content.unwrap()["refusal"]["code"],
            "stale_element_token"
        );
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
        assert_eq!(error.to_string(), "not visible");
        let result = error_result(error, || unreachable!("not a dead element"));
        assert_eq!(
            result.structured_content.unwrap()["code"],
            "element_not_visible"
        );
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
            Ok(())
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
            let mut tap = FakeTap {
                live,
                sent: Vec::new(),
            };
            let error = foreground_tap(&mut tap, live_center).unwrap_err();
            assert!(tap.sent.is_empty(), "{live:?}: sent {:?}", tap.sent);
            assert!(error.is::<DeadElement>(), "{live:?}: {error}");
        }
    }

    /// Foreground click on an element that moved since the read: the tap goes
    /// to the centre of its live rectangle, not the cached centre.
    #[test]
    fn a_foreground_click_on_a_moved_element_taps_the_live_centre() {
        let mut tap = FakeTap {
            live: LiveBounds::Rect((300, 400, 360, 420)),
            sent: Vec::new(),
        };
        let point = foreground_tap(&mut tap, live_center).unwrap();
        assert_eq!(tap.sent, vec![(330, 410)]);
        assert_eq!(point, (330, 410));
        assert_ne!(point, rect_center(CACHED_RECT).unwrap());
        // The MSAA dropdown half is picked inside the live rectangle too.
        let mut tap = FakeTap {
            live: LiveBounds::Rect((300, 400, 360, 420)),
            sent: Vec::new(),
        };
        foreground_tap(&mut tap, dropdown_half).unwrap();
        assert_eq!(tap.sent, vec![(356, 410)]);
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
