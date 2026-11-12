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
| `suspected_noop` | `BACKGROUND_TEXT_UNCHANGED` | yes | **Changed:** only when the write was clearly rejected (unsupported attribute, illegal argument, invalid element, not implemented, API disabled) and the value reads back unchanged. |

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

Either envelope adds `"range_restored": true|false` when the driver had moved
the range before refusing (**new field**), and with `false` also
`"retryable": false` and `"escalation": {"recommended": "verify_state", …}`
(see above).

Routes: **AX** is the background accessibility route, **Keys** the background
keystroke route's caret and focus checks, **Fg** the foreground route.

| Code | Routes | Meaning |
| --- | --- | --- |
| `BACKGROUND_TEXT_TARGET_UNSUPPORTED` | Keys | **Changed:** `insert_at` `end` or `selection` was passed for a terminal or for a target with no accessibility element. Unsupported roles no longer refuse; they take the keystroke route. |
| `BACKGROUND_TEXT_VALUE_UNREADABLE` | AX | Cannot read the whole old value, including a detected character-count mismatch. |
| `BACKGROUND_TEXT_FOCUS_UNSUPPORTED` | AX, Keys | An unfocused target that is web content, or not provably native, cannot use native-only AX focus (changed: now includes an incomplete ancestor walk). **New route:** Keys: an element addressed by `element_token` that does not read `AXFocused` true; the keystroke step never sets focus. |
| `BACKGROUND_TEXT_FOCUS_NOT_RETAINED` | AX | The focus request was clearly rejected (plain envelope; nothing moved), or it was accepted but did not read back within 100 ms, or (**changed**) it returned an error that is not a clear rejection, such as a busy app's timeout. The last two carry `focused_first: true`, because focus may have moved or still move; after an unclear error the driver sends the range restore behind the request and reports `range_restored`. |
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
and, when the range had moved, `range_restored: true|false`.

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
rest may still arrive, so read the field before typing more. Only
`foreground_focus_lost` with a verified prefix still names a suffix to retry.
Exact shapes remain
owned by `foreground_typing.rs`, `type_text.rs`, and their existing tests.
