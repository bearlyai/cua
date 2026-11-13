# macOS background text insertion

`type_text` on macOS has three routes. Which one runs decides what the result
can mean.

| Route | When | `path` |
| --- | --- | --- |
| Background accessibility | Background delivery, the target is an unprotected `AXTextField`, `AXTextArea`, `AXComboBox` or `AXSearchField`, and it reports `AXSelectedText` as settable. An unfocused AppKit field reports that only once focused, so a provably native unfocused field goes through the focus step and is checked again there; if it still is not settable, it takes the keystroke route after that focus step | `ax` |
| Background keystrokes | Background delivery and the target takes no `AXSelectedText` write (Chromium / Electron inputs, other roles, secure fields), or no accessibility element was found | `key_events` |
| Foreground | `delivery_mode: "foreground"` | `key_events_fg`, `ax_fg` |

Terminals, remote desktops and desktop scope keep their own routes (a terminal
addressed by `element_token` still has that element focused), outside
this document.

## Background accessibility route

**The range before the call.** Wherever the driver puts the range back, it
puts back the range the field reported before the call, with one exception. A
single-line field that read unfocused has no caret, and focusing it selects
its whole value. When the field read unfocused and the driver's focus then
selected the whole value, the collapsed range it reported (commonly `{0,0}`)
is ignored and the caret goes to the end of the text, where a click after the
text would put it (**changed:** it used to go back to `{0,0}`, so a later
keyboard retry typed at the start). A multi-line text view (`NSTextView`)
keeps a real collapsed caret while unfocused and focusing it selects nothing,
so that caret is what a restore puts back (**changed:** the previous build
moved it to the end). A field that already read focused keeps its own caret
or selection. An unreadable range becomes a caret at the end. A nonempty
selection the field reported before the call is still the person's and is
kept.

The driver reads the old value, settles and reads back an exact range, writes
`AXSelectedText` once, and verifies the exact expected result. It never writes
the whole `AXValue`, activates the application, moves the physical pointer, or
retries the write through keystrokes.

The macOS tool accepts `insert_at: "caret" | "end" | "selection"` (default
`"caret"`). It applies only to the background routes; passing it with desktop
scope, pixel targeting, or foreground delivery is an argument error. Other
platforms retain their existing behavior and tool schemas.

A fresh native field receives `AXFocused=true` inside its own background window
when its readable `AXFocused` was false. The driver waits up to 100 ms for focus
to read back, then waits until two consecutive range reads agree (at most
150 ms). That one settled range drives both the focus guard and the write, so a
toolkit that selects on focus a moment late is still caught. Single-line Cocoa
fields commonly select their entire value: that selection is not a caret or
permission to replace the value. Default `caret` refuses. Explicit `end`
collapses the range at the UTF-16 value end. `selection` may replace a range
that existed before focus, but refuses a new selection created by focusing.
Empty fields may proceed. A missing caret on a nonempty value refuses; it never
means append implicitly. Immediately before the write the driver reads the
value, then the range, again; if either differs from what the decision used, it
refuses and writes nothing.

When a call refuses after the driver itself moved the range (its focus step or
its own range set), it puts back the range the field had before the call, reads
it back, and reports
`range_restored: true|false`. Restoring the range does not undo the focus move
itself. When the restore did not hold (`range_restored: false`), the field may
still hold a selection the driver made, which the next keystroke or a keyboard
retry would replace: that refusal also carries `retryable: false` and
`escalation.recommended: "verify_state"` (**new**), and it must not be retried
with the keyboard until the field has been clicked.

On the background routes in this document, focus moves only on targets that
are provably native, and only in this guarded focus step. The terminal route
and the pixel form's focus click are outside this rule: a terminal addressed
by `element_token` has that element focused, and the pixel form clicks the
field. Provably native means the element's ancestor walk must
reach its window or the app without passing an `AXWebArea`. A walk that ends
early (an unreadable role or parent, or more than 40 levels) counts as web
content: the field is not focused in the background and an accessibility echo
is never reported `confirmed`.

`get_window_state.elements[]` adds these fields for unprotected text entries:

```json
{"focused": false, "selected_text_range": {"location": 3, "length": 0}, "value": "Original"}
```

Focus is the reported `AXFocused` attribute, not an inference from which app is
frontmost. Range offsets and lengths are the accessibility API's UTF-16 code
units. An absent/unreadable attribute is omitted. Secure/protected fields omit
both fields and structured `value`. Readable empty text emits `value: ""`;
unreadable text omits `value`. Text values retain whitespace and never substitute
a placeholder for unreadable/empty text.

A successful public response looks like this (the summary is human-readable):

```json
{
  "content": [{"type": "text", "text": "Background insertion: confirmed (BACKGROUND_TEXT_EXACT_READBACK). Field focused first inside its background window."}],
  "structuredContent": {
    "effect": "confirmed",
    "route": "accessibility",
    "delivery": {"mode": "background"},
    "evidence": [{"kind": "value_readback"}],
    "focused_first": true,
    "summary": "Background insertion: confirmed (BACKGROUND_TEXT_EXACT_READBACK). Field focused first inside its background window."
  }
}
```

`focused_first` is omitted in successful output when the driver did not move
focus, and defaults to `false` in the generated Python and TypeScript
`ActionResult`. No previous focus is restored by activating another element or
app.

### Results after the write

The write ran exactly once. None of these is a refusal, and none may trigger a
blind retry except as the table says.

| `effect` | `code` | Error | Meaning |
| --- | --- | --- | --- |
| `confirmed` | `BACKGROUND_TEXT_EXACT_READBACK` | no | The value reads back as exactly the old value with this edit, on a provably native target. |
| `unverifiable` | `BACKGROUND_TEXT_EXACT_READBACK` | no | Exact readback, but the target is web content or not provably native (changed: an incomplete ancestor walk is now in this row). |
| `unverifiable` | `BACKGROUND_TEXT_READBACK_UNAVAILABLE` | no | The value cannot be read back, or is known truncated. |
| `unverifiable` | `BACKGROUND_TEXT_UNEXPECTED_VALUE` | yes | The value changed, but not into the expected value. |
| `unverifiable` | `BACKGROUND_TEXT_WRITE_UNCONFIRMED` | yes | **New.** The value has not changed yet, but the write may still land: it was accepted (on any target, native included, because a toolkit need not apply it before the 200 ms readback poll ends), or it returned an error that is not a clear rejection (for example a busy app's messaging timeout). Inspect the field, never retry blindly. |
| `suspected_noop` | `BACKGROUND_TEXT_UNCHANGED` | yes | **Changed:** only when the write was clearly rejected (unsupported attribute, illegal argument, invalid element, action unsupported, not implemented, API disabled) and the value reads back unchanged. |

### Refusals before a text write

Every code below returns this envelope, with `CODE` substituted in both places
and `PATH` naming the route that refused (**changed:** keystroke-route
refusals used to say `"ax"`):

```json
{
  "content": [{"type": "text", "text": "Background insertion refused before writing text: CODE."}],
  "isError": true,
  "structuredContent": {"code": "CODE", "effect": "refused", "path": "PATH", "verified": false}
}
```

`PATH` is `"ax"` for the background accessibility route, `"key_events"` for
the background keystroke route's checks (every **Keys** row below, including
`BACKGROUND_TEXT_TARGET_UNSUPPORTED`), and `"key_events_fg"` or `"key_events"`
for the foreground row of `BACKGROUND_TEXT_FOCUS_SELECTED_VALUE`.

If the driver asked for focus first, and the request was accepted, the envelope
instead is:

```json
{
  "content": [{"type": "text", "text": "Field focused first inside its background window; text insertion refused before writing: CODE."}],
  "isError": true,
  "structuredContent": {"code": "CODE", "effect": "refused", "path": "PATH", "verified": false, "focused_first": true}
}
```

When the focus request returned an error that is not a clear rejection, the
same envelope says only that the request may have reached the field
(**changed:** it used to say "Field focused first"):

```json
{
  "content": [{"type": "text", "text": "The focus request may have reached the field inside its background window; text insertion refused before writing: BACKGROUND_TEXT_FOCUS_NOT_RETAINED."}],
  "isError": true,
  "structuredContent": {"code": "BACKGROUND_TEXT_FOCUS_NOT_RETAINED", "effect": "refused", "path": "ax", "verified": false, "focused_first": true, "range_restored": true}
}
```

Every envelope adds `"range_restored": true|false` when the driver had moved
the range before refusing (**new field**), and with `false` also
`"retryable": false` and `"escalation": {"recommended": "verify_state", …}`
(see above). A refusal with `range_restored: false` never names a place to
resume: it carries no `retry_from_character` and no `delivered_chars`
(**changed**).

Routes: **AX** is the background accessibility route, **Keys** the background
keystroke route's caret and focus checks, **Fg** the foreground route.

| Code | Routes | Meaning |
| --- | --- | --- |
| `BACKGROUND_TEXT_TARGET_UNSUPPORTED` | Keys | **Changed:** `insert_at` `end` or `selection` was passed for a terminal or for a target with no accessibility element. Unsupported roles no longer refuse; they take the keystroke route. |
| `BACKGROUND_TEXT_VALUE_UNREADABLE` | AX | Cannot read the whole old value, including a detected character-count mismatch. |
| `BACKGROUND_TEXT_FOCUS_UNSUPPORTED` | AX, Keys | An unfocused target that is web content, or not provably native, cannot use native-only AX focus (changed: now includes an incomplete ancestor walk). **New route:** Keys: an element addressed by `element_token` that does not read `AXFocused` true; the keystroke step never sets focus. |
| `BACKGROUND_TEXT_FOCUS_NOT_RETAINED` | AX | The focus request was clearly rejected (plain envelope; nothing moved), or it was accepted but did not read back within 100 ms, or (**changed**) it returned an error that is not a clear rejection, such as a busy app's timeout. The last two carry `focused_first: true`, because focus may have moved or still move. In both, the driver first waits 150 ms more for a select-all that follows a late focus to land. After an unclear error it then always sends the range restore and reports `range_restored`; after an accepted request (**changed**) it restores, and reports `range_restored`, when the range has moved by then. A call whose focus reads back in time waits no longer than before. |
| `BACKGROUND_TEXT_FOCUS_SELECTED_VALUE` | AX, Fg | AX: focus created a nonempty selection and the caller did not request end insertion; the range is restored (`range_restored`). **Changed / new route:** Fg: focusing the addressed field selected its whole value and the caret could not be put back; nothing was typed (`path` `key_events_fg` or `key_events`, `focused_first: true`, `range_restored: false`, `retryable: false`, `posted_keystrokes: 0`). |
| `BACKGROUND_TEXT_CARET_UNAVAILABLE` | AX, Keys | AX: default insertion has no reported caret on a nonempty value. **New route:** Keys: `end` was requested and the caret could not be moved to the end and read back; the old caret is put back (`range_restored`). |
| `BACKGROUND_TEXT_SELECTION_REQUIRES_EXPLICIT_REPLACEMENT` | AX, Keys | AX: default (`caret`) insertion encountered an existing nonempty readable selection; `end` there collapses the selection at the value end and appends instead. **New route:** Keys: default or `end` insertion encountered an existing nonempty readable selection, which keystrokes would replace. |
| `BACKGROUND_TEXT_SELECTION_UNAVAILABLE` | AX, Keys | Explicit selection replacement has no readable selected range (**new route:** Keys). |
| `BACKGROUND_TEXT_RANGE_INVALID` | AX | Range exceeds the value, overflows, or splits a UTF-16 surrogate pair. |
| `BACKGROUND_TEXT_RANGE_NOT_RETAINED` | AX | The control rejected or ignored an exact range write, or (**changed**) the range differed when re-read immediately before the write. |
| `BACKGROUND_TEXT_RANGE_UNSTABLE` | AX | **New.** Two consecutive range reads never agreed within 150 ms. |
| `BACKGROUND_TEXT_VALUE_CHANGED` | AX | Value changed while establishing focus/range, or when re-read immediately before the text write. The range is restored when the driver had moved it. |

These codes mean no **text write** ran and no keystroke was sent. A refusal
with `focused_first:true` means in-window focus may have changed. Generic tool
errors, partial delivery, unverifiable results, and pre-existing foreground
errors do not establish that no input was sent.

## Background keystroke route

Fields that take no `AXSelectedText` write get CGEvent keystrokes posted to the
target process, as before this change, after these checks:

- this step never sets `AXFocused`. An element addressed by `element_token`
  must already read focused, either as found or after the guarded focus step
  above (provably native fields only); otherwise
  `BACKGROUND_TEXT_FOCUS_UNSUPPORTED` before anything is sent. Without an
  `element_token` the keystrokes go to the app's focused element;

- on the field's readable range, a readable nonempty selection is replaced only with `insert_at: "selection"`;
  otherwise `BACKGROUND_TEXT_SELECTION_REQUIRES_EXPLICIT_REPLACEMENT`;
- `end` sets the range to the end and reads it back, or refuses with
  `BACKGROUND_TEXT_CARET_UNAVAILABLE`; it never types at an unknown position;
- an unreadable range with the default `caret` types as before.

Whether the call may send keystrokes at all is decided before any of these
checks, so a direct call refuses before the caret moves. Three codes block
keystrokes while still allowing the accessibility write:
`type_text_synthesis_budget_exceeded` (the text is over the keystroke budget),
and the exact-target gate's `same_pid_keyboard_ambiguity` (the app has another
window that could receive the keys) and `minimized_or_hidden_window` (the
window is minimized or the app hidden, or that could not be proven). When a provably native field reaches this route only after the guarded
focus step, those refusals keep their envelopes and add `focused_first: true`
and, when the range had moved, `range_restored: true|false`. With
`range_restored: false` (**changed**) they also drop `retry_from_character`
and `delivered_chars`, and their message no longer suggests retrying in chunks
or by the keyboard: it reads "type_text refused before sending keystrokes
(CODE). The field may still hold a selection the driver made: click the field
before typing with the keyboard, or the typing would replace it."

The result is the keystroke route's existing result (`path: "key_events"`,
`type_text_not_delivered`, `type_text_incomplete`, the synthesis budget
refusal, and so on), with `ax_attempt: "rejected"` where it reports one. Web
content never reports `confirmed`. This is the route the pixel form
(`x`,`y`) normally reaches in Electron and other web views.

## Foreground route

The foreground step fronts the window and focuses the addressed field. When
focusing turned the range into one covering the whole nonempty value and it
did not cover it before, the driver restores the range before the call (see
"The range before the call" above: a caret at the end for a field that was not
focused and had only a collapsed range) before any keystroke or accessibility write, and refuses with
`BACKGROUND_TEXT_FOCUS_SELECTED_VALUE` when the restore does not hold. The
long-text accessibility fallback no longer sets the whole `AXValue` for a
selection the driver's focus step created. The whole-value write and
keystrokes follow the long-text `AXSelectedText` write only when it was
clearly rejected. When that write, or the whole-value write, returns an error
that is not a clear rejection (a busy app's timeout), or (**changed**) is
accepted but the value still reads unchanged after 2 s, the call ends as
`type_text_indeterminate` (`effect: "indeterminate"`, `retryable: false`) with
no further write and no keystrokes, on both `ax_fg` routes (an addressed
field and desktop scope). An accepted write that shows no change used to fall
through to a second write, to keystrokes, or over the keystroke budget to a
retryable `type_text_synthesis_budget_exceeded`.

Guarantee on this route: the driver's own focus step never causes the field's
existing text to be replaced. A selection the field already had before the
call, including a whole-value one, is replaced as before, because foreground
typing inserts at the current cursor. Without an `element_token` the driver
does not focus anything and this check does not apply.

## Known limits

- Background typing addresses the app's key window only. A field in another
  window of the app does not read focused within 100 ms after the focus step,
  so it is refused with `BACKGROUND_TEXT_FOCUS_NOT_RETAINED` (`path: "ax"`,
  `focused_first: true`); nothing is typed.

## Native control test

The canonical Mac runner selects the entire opt-in binary:

```sh
cargo test -p cua-driver-e2e --test background_text_insert -- --run-gui --installed-driver
```

The canonical command uses the already installed, TCC-authorized driver through
the unrecorded testkit MCP proxy. Omit `--installed-driver` only for an explicitly
TCC-authorized in-process adapter test. It first calls `check_permissions` with `prompt:false` and refuses to launch
fixtures unless Accessibility and Screen Recording are already granted. Direct
capture is intentionally not probed. The test compiles an AppKit/SwiftUI app,
launches it with `open -n -a`, and scores the app's own values, model binding,
formatting, activation notifications, key-window notifications, and pointer
position through a separate file channel. Plain cargo test skips all app work.

The 45 cases cover five controls, fresh/default and fresh/append behavior,
empty values both fresh and previously focused, remembered carets, explicit
existing selection replacement, and refusal to replace implicit or newly
focus-created selections; each such refusal must report `range_restored: true`.
NSTextField, NSSearchField, NSComboBox and SwiftUI TextField select all on
first focus; NSTextView keeps a collapsed caret. Five more rows type into each
fresh control with `delivery_mode: "foreground"` and require `Original!`: the
value kept, with the text after it.

## Other platforms inspected

Windows UIA reads the current string and sets `old + text`, so that route
appends instead of replacing the whole value; it does not honor a caret.
Its unreadable-current-value fallback remains outside this macOS change.
Linux AT-SPI tries `EditableText.insert_text` at a caret (defaulting to zero if
unavailable), then falls back to `set_text_contents(text)`, which can replace
the old value. Neither implementation is changed here.

## Existing refusals outside the new insertion transaction

Exact target gating retains the codes `window_not_found`,
`owner_pid_mismatch`, `off_space_or_ax_unresolved`,
`minimized_or_hidden_window`, `same_pid_keyboard_ambiguity`,
`element_outside_target_window`, and `background_unavailable` (unsafe Electron
AX ancestry). Their full envelope uses the following shape; `escalation` is
omitted when there is no advice:

```json
{
  "content": [{"type": "text", "text": "Background input refused (CODE): REASON"}],
  "isError": true,
  "structuredContent": {
    "code": "CODE", "effect": "refused", "pid": 123, "window_id": 456,
    "reason": "REASON",
    "escalation": {"recommended": "ADVICE", "reason": "REASON"}
  }
}
```

Token validation retains `invalid_element_token`, `stale_element_token`, and
`conflicting_element_target` before dispatch:

```json
{
  "content": [{"type": "text", "text": "MESSAGE"}],
  "isError": true,
  "structuredContent": {"status": "refused", "refusal": {"code": "CODE", "message": "MESSAGE"}}
}
```

A stale-token response additionally carries `current_snapshots: [{snapshot_id,
window_id}]`; consumers should refresh the window snapshot.
`stale_element_token` also covers a control that no longer exists at foreground
dispatch: the token's snapshot is still current, but a live read made before a
foreground step addressed by the token finds the element gone. The envelope is
the same, with the message `element_token is stale; call get_window_state again
to refresh; the element could not be found live in the app, so nothing was
activated or sent; current snapshots for pid …`. This refusal is only returned
by checks made before anything was activated, focused, clicked or sent.

Which foreground routes check, before they act:

- macOS: `click` (the pointer, HID and assist dispatches and the middle-button
  arm), `press_key` (before any focus write; the only focus write on a
  foreground request is the one inside the activated key transaction),
  `hotkey` (before the web-content hit-test, the focus click and the chord),
  `type_text` (both foreground routes), `scroll` (before the text-area scroll,
  the wheel and the key fallback) and `double_click` by element (before
  `AXOpen` or its click). The live read is the element's role. Only `-25202`
  (invalid element) counts as gone; any other error lets the step run as
  before.
- Windows: typing, `press_key` and `hotkey` read the element's current
  rectangle through UIA before the window is activated. That read is UIA
  only: an MSAA element reads as unknown, so it is never refused as gone
  there, typing skips its on-screen check, and keyboard focus on it stays
  unsupported: it gets no fallback click, its focus can't be confirmed, and
  no keys or text are sent.
- Windows clicks, double-clicks and right-clicks by element, and the wheel
  fallback of `scroll` by element, send their input inside the element's live
  rectangle (UIA, or MSAA `accLocation`), never at the point cached at read
  time; the cached point is no longer checked on foreground requests, only on
  background ones. UIA clicks, double-clicks, right-clicks and the wheel's
  live-centre point use the live centre after the on-screen check. MSAA
  clicks are the exceptions: a normal MSAA click uses the live centre with no
  on-screen check, as before, and an MSAA split-button dropdown click uses the
  dropdown half (4 px inside the right edge). `UIA_E_ELEMENTNOTAVAILABLE` counts as gone. A click or
  wheel is also refused this way when the live rectangle can't be read or is
  empty. A refused click doesn't start the restore of the earlier foreground
  window.
- **Changed:** a foreground Windows `scroll` by element whose own UIA scroll
  sent nothing now sends the wheel at the element's live centre, not at the
  window centre. A caller's x,y picks where the wheel goes instead, exactly as
  given; the point itself is not checked for visibility, and it does not
  exempt the element from being located live first. The wheel doesn't bring
  the window forward, so a window on top of the element can still take it.
- The element's own UIA scroll is one `ScrollPattern::Scroll` call per step.
  It does not bring the window forward: the window's root is marked
  no-activate during the calls and for 50 ms after them, and if the control
  takes the foreground anyway, the window that was in front before is put
  back. Its success reply says `delivery_mode: background`.
- If the first call fails, the pattern's horizontal and vertical scroll
  position, read before the call and after the error, decide what happened.
  A position that moved means that call scrolled: no wheel is sent and the
  reply is `scroll_incomplete` (below) with one delivered step. A position
  that did not move means nothing scrolled, and the wheel fallback above
  applies. When the position can't be read, a first-call error is still
  taken as nothing scrolled, and the wheel may follow. If a later call fails,
  some steps already moved the content, so no wheel is sent and the reply is
  `scroll_incomplete`, never the stale refusal.

The check is a probe, not atomic with the action. A control that the app
removes after the check but before the input lands is not caught. Ordinary
errors from the later steps are reported as before.

On Windows, a control that disappears after the keyboard transaction has
already activated the window gets an honest failure, never the no-effects
stale refusal (activation may also have used the driver's no-op
activation-assist key):

```json
{
  "content": [{"type": "text", "text": "foreground_element_lost: HWND 0x… was brought to the front, then UIA element [N] was no longer available; no keys or text were sent. Read the window again before acting."}],
  "isError": true,
  "structuredContent": {
    "code": "foreground_element_lost", "effect": "failed", "activated": true,
    "keystrokes_sent": false, "focus_click": "not_sent",
    "window_id": 0, "element_index": 0, "retryable": false,
    "escalation": {"recommended": "verify_state", "reason": "The window was brought to the front and no keys or text were sent; read the window again before acting."}
  }
}
```

Every other failure while establishing focus on the element after activation
has the same shape with `code: "foreground_unavailable"`: focus not confirmed,
the fallback focus click refused as off-screen, or the fallback focus click
returning an error. A later failure in the same transaction, such as the
window losing the foreground after focus was established or an error from
the key or text sender, is reported as an ordinary error, as before.
`focus_click` says what became of that click: `"not_sent"`, `"sent"`, or
`"uncertain"` when the click returned an error, because part or all of its
mouse input may have gone out (and a completed click may have moved the
foreground). None is `status: "refused"`.

MSAA elements get no focus-fallback click on Windows keys and text: keyboard
focus can't be confirmed on them, so a click could only disturb the window.

A foreground Windows `scroll` by element whose own UIA scroll stopped part-way
returns:

```json
{
  "content": [{"type": "text", "text": "scroll_incomplete: UIA element [N] in HWND 0x… scrolled 1 of 3 step(s), then its UIA scroll failed (UIA scroll failed: …); no wheel was sent. Read the window again before acting."}],
  "isError": true,
  "structuredContent": {
    "code": "scroll_incomplete", "effect": "partial", "path": "uia",
    "delivery": {"delivered_count": 1},
    "completed_steps": 1, "amount": 3, "wheel_sent": false,
    "window_id": 0, "element_index": 0, "retryable": false,
    "escalation": {"recommended": "verify_state", "reason": "The element scrolled part of the way before its UIA scroll failed, and no wheel was sent; read the window again before scrolling more."}
  }
}
```

It appears only after at least one step moved the content (a call that
succeeded, or a first call whose error came after the position moved), so
its effect is `partial` with the completed steps as
`delivery.delivered_count`, as the
[action result contract](action-result-contract.md) requires; it is never
`status: "refused"`. The post-activation keyboard failures above stay
`effect: "failed"`, because none of the requested keys or text was sent.

Exceptions that never take the foreground and so have no foreground check:
`right_click` by element on macOS (`AXShowMenu`, or a click posted to the
process) and `click` with `action: "expand"` on Windows (UIA
`ExpandCollapsePattern`). Background routes are unchanged; some of them still
send input at the point cached at read time.
Malformed arguments may return only `content` and `isError:true`, with no code.

`SCREEN_SHARING_REQUIRES_FOREGROUND_HID` is another pre-dispatch refusal; its
structured content is exactly:

```json
{
  "code": "SCREEN_SHARING_REQUIRES_FOREGROUND_HID", "effect": "refused",
  "escalation": {
    "recommended": "foreground",
    "reason": "Screen Sharing forwards physical keycodes; background PID-routed Unicode events can corrupt guest text.",
    "requires": ["window_id"]
  }
}
```

The foreground/terminal/desktop routes otherwise retain their existing codes and are
outside the background AX insertion contract. In particular, do **not** map
all of these to "nothing sent": `foreground_field_not_accepting`,
`foreground_focus_lost`, `foreground_input_rate_limited`, `cancelled`,
`type_text_incomplete`, `type_text_indeterminate`, `type_text_not_delivered`,
`foreground_focus_changed`, and `type_text_synthesis_budget_exceeded`. Their
delivery counters or an earlier AX attempt can indicate an attempted write or
posted events. `foreground_target_not_focused` and
`foreground_target_not_editable` are pre-keystroke refusals. **Changed:** on
every keystroke route, `effect: "refused"` now means no keystroke was posted.
Keystrokes that were posted but do not show up in the field
(`type_text_not_delivered`, and `foreground_field_not_accepting`,
`foreground_focus_lost`, `cancelled` or `type_text_incomplete` with
`verified_chars: 0` and `posted_keystrokes` above 0) report
`effect: "unverifiable"`, because they may still land. **Changed:**
`type_text_incomplete` (`effect: "partial"`), where every keystroke was posted
but only some show, is `retryable: false` with no `retry_from_character`: the
rest may still arrive, so read the field before typing more. **Changed:**
`foreground_focus_lost` with a verified prefix (`effect: "partial"`) no
longer names a suffix to retry either: it is `retryable: false` with no
`retry_from_character` and `escalation.recommended: "verify_state"` (it was
`bring_to_front`), because keystrokes already sent after the verified part may
still arrive if the field regains focus. No result after foreground
keystrokes were posted is `retryable: true`.
Exact shapes remain
owned by `foreground_typing.rs`, `type_text.rs`, and their existing tests.
