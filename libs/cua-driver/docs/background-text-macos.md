# macOS background text insertion

`type_text` on a native accessibility element preserves its existing value and
formatting. The driver reads the old value, establishes and reads back an exact
range, writes `AXSelectedText` once, and verifies the exact expected result. It
never writes the whole `AXValue`, activates the application, moves the physical
pointer, or retries an unverified write through keystrokes on this route.

The macOS tool accepts `insert_at: "caret" | "end" | "selection"` (default
`"caret"`). This argument applies only to the background AX route; passing it
with desktop scope, pixel targeting, or foreground delivery is an argument
error. Other platforms retain their existing behavior and tool schemas.

A fresh native field receives `AXFocused=true` inside its own background window
when its readable `AXFocused` was false. The driver verifies focus, then reads
the range again. Single-line Cocoa fields commonly select their entire value:
that selection is not a caret or permission to replace the value. Default
`caret` refuses. Explicit `end` collapses the range at the UTF-16 value end.
`selection` may replace a range that existed before focus, but refuses a new
selection created by focusing. Empty fields may proceed. A missing caret on a
nonempty value refuses; it never means append implicitly.

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
focus. No previous focus is restored by activating another element or app.
Web-content AX echoes never establish confirmed delivery. Unreadable or known
truncated readback yields `unverifiable`; a readable unchanged value yields
`suspected_noop`; an unexpected changed value yields `unverifiable` and an
error. These outcomes are not refusals and must not trigger a blind retry.

## Refusals before a text write

Every code below returns this complete envelope, with `CODE` substituted in
both places:

```json
{
  "content": [{"type": "text", "text": "Background insertion refused before writing text: CODE."}],
  "isError": true,
  "structuredContent": {"code": "CODE", "effect": "refused", "path": "ax", "verified": false}
}
```

If focus was successfully moved first, the full envelope instead is:

```json
{
  "content": [{"type": "text", "text": "Field focused first inside its background window; text insertion refused before writing: CODE."}],
  "isError": true,
  "structuredContent": {"code": "CODE", "effect": "refused", "path": "ax", "verified": false, "focused_first": true}
}
```

| Code | Meaning |
| --- | --- |
| `BACKGROUND_TEXT_TARGET_UNSUPPORTED` | Not an unprotected supported text-entry element, or an insertion mode requires AX on a key-driven/missing-field route. |
| `BACKGROUND_TEXT_VALUE_UNREADABLE` | Cannot read the whole old value, including a detected character-count mismatch. |
| `BACKGROUND_TEXT_FOCUS_UNSUPPORTED` | An unfocused web-content target cannot safely use native-only AX focus. |
| `BACKGROUND_TEXT_FOCUS_NOT_RETAINED` | The focus setter failed or readable focus did not confirm it. |
| `BACKGROUND_TEXT_FOCUS_SELECTED_VALUE` | Focus created a nonempty selection, and the caller did not request end insertion. |
| `BACKGROUND_TEXT_CARET_UNAVAILABLE` | Default insertion has no reported caret on a nonempty value. |
| `BACKGROUND_TEXT_SELECTION_REQUIRES_EXPLICIT_REPLACEMENT` | Default caret insertion encountered an existing nonempty selection. |
| `BACKGROUND_TEXT_SELECTION_UNAVAILABLE` | Explicit selection replacement has no readable selected range. |
| `BACKGROUND_TEXT_RANGE_INVALID` | Range exceeds the value, overflows, or splits a UTF-16 surrogate pair. |
| `BACKGROUND_TEXT_RANGE_NOT_RETAINED` | The control rejected or ignored an exact range write. |
| `BACKGROUND_TEXT_VALUE_CHANGED` | Value changed while establishing focus/range, before the text write. |

These codes mean no **text write** ran. A refusal with `focused_first:true`
means in-window focus did change. Generic tool errors, partial delivery,
unverifiable results, and pre-existing foreground errors do not establish that
no input was sent.

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

The 40 cases cover five controls, fresh/default and fresh/append behavior,
empty values both fresh and previously focused, remembered carets, explicit
existing selection replacement, and refusal to replace implicit or newly
focus-created selections. NSTextField, NSSearchField, NSComboBox and SwiftUI
TextField select all on first focus; NSTextView keeps a collapsed caret.

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

The foreground/terminal/desktop routes retain their existing codes and are
outside the background AX insertion contract. In particular, do **not** map
all of these to "nothing sent": `foreground_field_not_accepting`,
`foreground_focus_lost`, `foreground_input_rate_limited`, `cancelled`,
`type_text_incomplete`, `type_text_indeterminate`, `type_text_not_delivered`,
`foreground_focus_changed`, and `type_text_synthesis_budget_exceeded`. Their
delivery counters or an earlier AX attempt can indicate an attempted write or
posted events. `foreground_target_not_focused` and
`foreground_target_not_editable` are pre-keystroke refusals. Exact shapes remain
owned by `foreground_typing.rs`, `type_text.rs`, and their existing tests.
