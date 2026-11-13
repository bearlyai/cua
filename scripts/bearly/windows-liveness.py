"""Foreground liveness rows for a Bearly driver build against owned WinForms windows.

Each row reads the target window, optionally changes the fixture (removes or
replaces the addressed control, moves the window, arms a failure), then sends
one foreground action addressed by the element token from that read. It records
the driver's raw reply (text and structuredContent), both fixtures' state before
and after, every input message each fixture's thread received, and the
foreground window sampled every 2 ms. Expectations are evaluated after the fact
and never change what is sent.
"""
import argparse
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time
import traceback
import uuid
import winreg

parser = argparse.ArgumentParser()
parser.add_argument('--driver', required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--allow-high', action='store_true')
args = parser.parse_args()
out = Path(args.output).resolve()
out.mkdir(parents=True, exist_ok=True)
sys.stdout = (out / 'proof.log').open('w', encoding='utf-8', buffering=1)
sys.stderr = (out / 'proof-error.log').open('w', encoding='utf-8', buffering=1)
here = Path(__file__).resolve().parent
NO_WINDOW = 0x08000000

user = ctypes.WinDLL('user32', use_last_error=True)
kernel = ctypes.WinDLL('kernel32', use_last_error=True)
advapi = ctypes.WinDLL('advapi32', use_last_error=True)
dwm = ctypes.WinDLL('dwmapi')
user.GetForegroundWindow.restype = wintypes.HWND
user.SetForegroundWindow.argtypes = [wintypes.HWND]
user.GetCursorPos.argtypes = [ctypes.POINTER(wintypes.POINT)]
user.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user.WindowFromPoint.argtypes = [wintypes.POINT]
user.WindowFromPoint.restype = wintypes.HWND
user.GetAncestor.argtypes = [wintypes.HWND, wintypes.UINT]
user.GetAncestor.restype = wintypes.HWND
dwm.DwmGetWindowAttribute.argtypes = [wintypes.HWND, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD]
kernel.GetCurrentProcess.restype = wintypes.HANDLE
kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel.OpenProcess.restype = wintypes.HANDLE
kernel.CloseHandle.argtypes = [wintypes.HANDLE]
advapi.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
advapi.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
advapi.GetSidSubAuthorityCount.argtypes = [ctypes.c_void_p]
advapi.GetSidSubAuthorityCount.restype = ctypes.POINTER(ctypes.c_ubyte)
advapi.GetSidSubAuthority.argtypes = [ctypes.c_void_p, wintypes.DWORD]
advapi.GetSidSubAuthority.restype = ctypes.POINTER(wintypes.DWORD)
LEVELS = {0x1000: 'Low', 0x2000: 'Medium', 0x2100: 'Medium Plus', 0x3000: 'High', 0x4000: 'System'}
# Keys the driver sends itself around activation: VK_NONAME (activation assist)
# and 0xE8 (its input-drain marker). Counted apart from requested keys.
ASSIST_KEYS = {0xFC, 0xE8}
# Fixture notes that are not input: activation, focus and provider calls.
NOTE_KINDS = ('activated', 'deactivated', 'gotfocus', 'scrollcall', 'getpattern', 'percent')


class _Notes:
    def __contains__(self, kind):
        return kind.startswith(NOTE_KINDS)


NOTES = _Notes()
B9 = 0xB9
TARGET_HOME = (160, 140, 520, 420)
SENTINEL_HOME = (100, 100, 700, 520)
SENTINEL_ASIDE = (760, 560, 260, 160)
MOVE = (60, 30)


def integrity(pid=None):
    process = kernel.GetCurrentProcess() if pid is None else kernel.OpenProcess(0x1000, False, pid)
    if not process:
        return {'pid': pid, 'error': ctypes.get_last_error()}
    token = wintypes.HANDLE()
    try:
        if not advapi.OpenProcessToken(process, 8, ctypes.byref(token)):
            return {'pid': pid, 'error': ctypes.get_last_error()}
        size = wintypes.DWORD()
        advapi.GetTokenInformation(token, 25, None, 0, ctypes.byref(size))
        data = ctypes.create_string_buffer(size.value)
        if not advapi.GetTokenInformation(token, 25, data, size, ctypes.byref(size)):
            return {'pid': pid, 'error': ctypes.get_last_error()}
        sid = ctypes.cast(data, ctypes.POINTER(ctypes.c_void_p))[0]
        rid = advapi.GetSidSubAuthority(sid, advapi.GetSidSubAuthorityCount(sid)[0] - 1)[0]
        return {'pid': pid or os.getpid(), 'rid': hex(rid), 'level': LEVELS.get(rid, 'Other')}
    finally:
        if token:
            kernel.CloseHandle(token)
        if pid is not None:
            kernel.CloseHandle(process)


def foreground():
    return int(user.GetForegroundWindow() or 0)


_qpf = ctypes.c_int64()
kernel.QueryPerformanceFrequency(ctypes.byref(_qpf))


def now_ms():
    """The machine-wide performance counter in ms, the fixtures' event clock."""
    counter = ctypes.c_int64()
    kernel.QueryPerformanceCounter(ctypes.byref(counter))
    return counter.value // (_qpf.value // 1000)


def cursor():
    point = wintypes.POINT()
    user.GetCursorPos(ctypes.byref(point))
    return [point.x, point.y]


def frame(hwnd):
    rect = wintypes.RECT()
    dwm.DwmGetWindowAttribute(wintypes.HWND(hwnd), 9, ctypes.byref(rect), ctypes.sizeof(rect))
    return [rect.left, rect.top, rect.right, rect.bottom]


def wait_for(probe, timeout):
    deadline = time.monotonic() + timeout
    while True:
        try:
            value = probe()
            if value:
                return value
        except (OSError, ValueError, KeyError):
            pass
        if time.monotonic() > deadline:
            return None
        time.sleep(0.03)


def state(name):
    for attempt in range(60):
        try:
            return json.loads((out / (name + '.json')).read_text(encoding='utf-8-sig'))
        except (OSError, ValueError):
            if attempt == 59:
                raise
            time.sleep(0.03)


seqs = {}


def command(name, *parts, timeout=8):
    seq = seqs[name] = seqs.get(name, 0) + 1
    line = ' '.join([str(seq)] + [str(part) for part in parts])
    temp = out / f'{name}-cmd.tmp'
    temp.write_text(line, encoding='ascii')
    for _ in range(200):
        try:
            os.replace(temp, out / f'{name}-cmd.txt')
            break
        except PermissionError:
            time.sleep(0.01)
    reply = wait_for(lambda: (lambda current: current if current.get('ack') == seq else None)(state(name)), timeout)
    if not reply:
        raise RuntimeError(f'{name} did not acknowledge: {line}')
    if reply.get('cmd_error'):
        raise RuntimeError(f'{name} command failed: {line}: {reply["cmd_error"]}')
    return reply


me = integrity()
whoami = subprocess.run(['whoami', '/groups', '/fo', 'csv'], capture_output=True, text=True, creationflags=NO_WINDOW)
(out / 'whoami.txt').write_text(subprocess.run(['whoami'], capture_output=True, text=True, creationflags=NO_WINDOW).stdout + whoami.stdout, encoding='utf-8')
label_line = next((line for line in whoami.stdout.splitlines() if 'Mandatory Label' in line), '')
print('proof integrity', me, label_line, flush=True)
measured = me.get('level') == 'Medium'
if not measured and not args.allow_high:
    (out / 'results.json').write_text(json.dumps({'measured': False, 'proof_integrity': me, 'whoami_label': label_line}, indent=2))
    sys.exit(3)

children, logs, rows = [], [], []
pipe = r'\\.\pipe\bearly-liveness-' + uuid.uuid4().hex
SESSION = 'bearly-liveness-proof'
facts = {'measured_at_medium': measured, 'proof_integrity': me, 'whoami_label': label_line,
         'user': os.environ.get('USERNAME'), 'screen': [user.GetSystemMetrics(0), user.GetSystemMetrics(1)]}
sid = wintypes.DWORD()
kernel.ProcessIdToSessionId(os.getpid(), ctypes.byref(sid))
facts['session_id'] = sid.value
try:
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r'Control Panel\Desktop') as key:
        facts['mouse_wheel_routing'] = winreg.QueryValueEx(key, 'MouseWheelRouting')[0]
except OSError as error:
    facts['mouse_wheel_routing'] = f'unset ({error.__class__.__name__})'


def raw(tool, values, timeout=60):
    """One tool call straight over the daemon pipe, keeping the whole result."""
    request = {'method': 'call', 'name': tool, 'args': dict(values, session=SESSION),
               'session_id': 'cli-explicit', 'observation_origin': 'direct', 'client_kind': 'cli'}
    box = {}

    def work():
        try:
            deadline = time.monotonic() + 5
            while True:
                try:
                    handle = open(pipe, 'r+b', buffering=0)
                    break
                except OSError:
                    if time.monotonic() > deadline:
                        raise
                    time.sleep(0.05)
            with handle:
                handle.write((json.dumps(request) + '\n').encode('utf-8'))
                data = b''
                while b'\n' not in data:
                    chunk = handle.read(1 << 16)
                    if not chunk:
                        break
                    data += chunk
            box['line'] = data.split(b'\n', 1)[0].decode('utf-8')
        except Exception:
            box['error'] = traceback.format_exc()
    worker = threading.Thread(target=work, daemon=True)
    worker.start()
    worker.join(timeout)
    if worker.is_alive():
        return {'transport_error': f'no reply within {timeout}s'}
    if 'error' in box:
        return {'transport_error': box['error']}
    try:
        response = json.loads(box['line'])
    except ValueError:
        return {'transport_error': 'unparseable reply', 'line': box['line'][:4000]}
    result = response.get('result') or {}
    content = result.get('content') or []
    structured = result.get('structuredContent')
    if isinstance(structured, dict):
        structured.pop('screenshot_png_b64', None)
    return {'ok': response.get('ok'), 'error': response.get('error'), 'isError': result.get('isError'),
            'text': '\n'.join(item.get('text', '') for item in content if item.get('type') == 'text'),
            'content_types': [item.get('type') for item in content], 'structuredContent': structured}


def read_tokens():
    view = raw('get_window_state', {'pid': target['pid'], 'window_id': target['hwnd'], 'include_screenshot': False})
    body = view.get('structuredContent') or {}
    return {item.get('label'): item for item in body.get('elements', []) if item.get('label')}, view


def front(hwnd):
    if foreground() == hwnd:
        return True
    user.SetForegroundWindow(hwnd)
    if wait_for(lambda: foreground() == hwnd, 1):
        return True
    reply = out / 'foreground-reply.txt'
    reply.unlink(missing_ok=True)
    temp = out / 'foreground-request.tmp'
    temp.write_text(str(hwnd))
    temp.replace(out / 'foreground-request.txt')
    wait_for(lambda: reply.exists(), 10)
    return bool(wait_for(lambda: foreground() == hwnd, 2))


unknown_windows = {}


def describe(hwnd):
    pid = wintypes.DWORD()
    user.GetWindowThreadProcessId(wintypes.HWND(hwnd), ctypes.byref(pid))
    cls, title = ctypes.create_unicode_buffer(256), ctypes.create_unicode_buffer(256)
    user.GetClassNameW(wintypes.HWND(hwnd), cls, 256)
    user.GetWindowTextW(wintypes.HWND(hwnd), title, 256)
    owner = {target['pid']: 'target fixture', sentinel['pid']: 'sentinel fixture', daemon.pid: 'driver daemon', os.getpid(): 'proof'}.get(pid.value, 'other process')
    return {'hwnd': hex(hwnd), 'pid': pid.value, 'owner': owner, 'class': cls.value, 'title': title.value}


def who(hwnd):
    known = {target['hwnd']: 'target', sentinel['hwnd']: 'sentinel', 0: 'none'}.get(hwnd)
    if known:
        return known
    if hwnd not in unknown_windows:
        unknown_windows[hwnd] = describe(hwnd)
    return hex(hwnd)


def under(point):
    if point is None:
        return None
    child = int(user.WindowFromPoint(wintypes.POINT(*point)) or 0)
    top = int(user.GetAncestor(wintypes.HWND(child), 2) or 0) if child else 0
    return {'child': describe(child) if child else None, 'top_level': who(top)}


def one_insertion(before, after, text):
    """Position at which `text` was inserted once into `before`, else None."""
    if before is None or after is None or len(after) != len(before) + len(text):
        return None
    for index in range(len(before) + 1):
        if after == before[:index] + text + before[index:]:
            return index
    return None


def summarise(events):
    counts = {}
    for _, control, kind, value in events:
        if kind in ('keydown', 'keyup', 'syskeydown', 'syskeyup') and value in ASSIST_KEYS:
            kind = 'assist_' + kind
        counts.setdefault(control, {}).setdefault(kind, 0)
        counts[control][kind] += 1
    totals = {'clicks': 0, 'double_clicks': 0, 'keys': 0, 'text': 0, 'wheel': 0, 'assist_keys': 0,
              'all': sum(1 for e in events if e[2] not in NOTES)}
    for per in counts.values():
        for kind, count in per.items():
            if kind in ('ldown', 'rdown', 'mdown'):
                totals['clicks'] += count
            elif kind in ('ldbl', 'rdbl'):
                totals['double_clicks'] += count
            elif kind in ('keydown', 'syskeydown'):
                totals['keys'] += count
            elif kind in ('char', 'syschar'):
                totals['text'] += count
            elif kind in ('wheel', 'hwheel'):
                totals['wheel'] += count
            elif kind.startswith('assist_') and kind.endswith('down'):
                totals['assist_keys'] += count
    return counts, totals


def timeline(rec, start_q):
    """Everything on one clock, ms from the call start: the reply, foreground
    changes, fixture notes (activation, focus, provider calls) and input."""
    items = [[0, 'proof', 'call start', None], [rec['reply_clock_ms'], 'proof', 'reply', None]]
    items += [[ms, 'foreground', name, None] for ms, name in rec['foreground_samples']]
    for name in ('Target', 'Sentinel'):
        for t, control, kind, value in rec['received'][name]['events']:
            items.append([t - start_q, name.lower(), f'{control} {kind}', value])
    items.sort(key=lambda item: item[0])
    stray = []
    for index, (t, source, what, value) in enumerate(items):
        if source in ('target', 'sentinel') and what.endswith(' keyup') and value == B9:
            earlier = [item for item in items[:index] if item[1] == 'target' and any(k in item[2] for k in ('scrollcall', 'getpattern', 'percent'))]
            stray.append({'at_ms': t, 'window': source, 'after_provider_call': earlier[-1] if earlier else None,
                          'after': items[index - 1] if index else None})
    return items, stray


def run_row(rid, group, desc, tool, label, extra=None, expect=None, mutate=(), arm=(), aside=False,
            tail=1.0, post=None, token=None, keep=False, mode='foreground'):
    rec = {'id': rid, 'group': group, 'desc': desc, 'tool': tool, 'label': label, 'aside': aside,
           'mutate': [list(m) for m in mutate], 'arm': [list(a) for a in arm]}
    try:
        if not keep:
            command('Target', 'reset')
            command('Sentinel', 'reset')
        if aside:
            command('Sentinel', 'bounds', *SENTINEL_ASIDE)
        rec['sentinel_in_front_before'] = front(sentinel['hwnd'])
        time.sleep(0.25)
        for step in arm:
            command('Target', *step)
        if token is None:
            found, view = read_tokens()
            if label not in found:
                rec['harness_error'] = f'{label!r} not in the snapshot'
                rec['snapshot_labels'] = sorted(found)
                rec['snapshot_reply'] = {k: v for k, v in view.items() if k != 'structuredContent'}
                raise LookupError(rec['harness_error'])
            token = found[label]['element_token']
            rec['element'] = {k: found[label].get(k) for k in ('element_index', 'role', 'label', 'value', 'actions', 'element_token')}
        rec['token'] = token
        before_read = state('Target')
        for step in mutate:
            command('Target', *step)
        if mutate:
            time.sleep(0.3)
        values = dict(extra(rec) if callable(extra) else (extra or {}))
        values.update(pid=target['pid'], element_token=token, delivery_mode=mode)
        rec['args'] = values
        fg_before, cursor_before = foreground(), cursor()
        target_before, sentinel_before = state('Target'), state('Sentinel')
        rec['target_before'], rec['sentinel_before'] = target_before, sentinel_before
        samples, stopped = [], threading.Event()
        start = time.monotonic()
        start_q = now_ms()
        rec['call_start_clock_ms'] = start_q

        def observe():
            last = fg_before
            while not stopped.wait(0.002):
                current = foreground()
                if current != last:
                    samples.append([round((time.monotonic() - start) * 1000), current])
                    if current and current not in (target['hwnd'], sentinel['hwnd']) and current not in unknown_windows:
                        unknown_windows[current] = describe(current)
                    last = current
        observer = threading.Thread(target=observe)
        observer.start()
        try:
            rec['reply'] = raw(tool, values)
            rec['reply_ms'] = round((time.monotonic() - start) * 1000)
            rec['reply_clock_ms'] = now_ms() - start_q
            rec['window_under_reported_point'] = under(reported_point(rec))
            if post:
                post(rec)
            time.sleep(tail)
        finally:
            stopped.set()
            observer.join()
        target_after, sentinel_after = state('Target'), state('Sentinel')
        rec['target_after'], rec['sentinel_after'] = target_after, sentinel_after
        rec['foreground_before'] = who(fg_before)
        rec['foreground_after'] = who(foreground())
        rec['foreground_samples'] = [[ms, who(hwnd)] for ms, hwnd in samples]
        rec['unknown_foreground_windows'] = [unknown_windows[hwnd] for _, hwnd in samples if hwnd in unknown_windows]
        rec['target_activated'] = any(name == 'target' for _, name in rec['foreground_samples'])
        rec['cursor_before'], rec['cursor_after'] = cursor_before, cursor()
        rec['daemon_alive'] = daemon.poll() is None
        received = {}
        for name, before, after in (('Target', target_before, target_after), ('Sentinel', sentinel_before, sentinel_after)):
            new = after['events'][len(before['events']):]
            counts, totals = summarise(new)
            received[name] = {'events': new, 'counts': counts, 'totals': totals}
        rec['received'] = received
        rec['before_read_state_events'] = len(before_read['events'])
        rec['timeline'], rec['stray_b9_keyups'] = timeline(rec, start_q)
    except Exception:
        rec.setdefault('harness_error', traceback.format_exc())
    if expect and 'harness_error' not in rec:
        notes = []
        try:
            verdict = expect(rec, notes)
        except Exception:
            verdict, notes = 'HARNESS', notes + [traceback.format_exc()]
        rec['verdict'], rec['notes'] = verdict, notes
    else:
        rec['verdict'], rec['notes'] = ('HARNESS', [rec.get('harness_error', '')[-600:]]) if 'harness_error' in rec else ('INFO', [])
    rec['observed'] = observed(rec)
    rows.append(rec)
    print('ROW', rid, rec['verdict'], '|', desc, '|', rec['observed'], '|', '; '.join(n.splitlines()[0] for n in rec['notes'] if n), flush=True)
    return rec


def sc(rec):
    return (rec.get('reply') or {}).get('structuredContent') or {}


def observed(rec):
    if 'reply' not in rec:
        return 'no reply'
    reply, structured = rec['reply'], sc(rec)
    parts = []
    if reply.get('transport_error'):
        parts.append('transport error')
    if structured.get('status'):
        parts.append(f"status={structured['status']}")
    if structured.get('refusal'):
        parts.append(f"refusal={structured['refusal'].get('code')}")
    for key in ('code', 'effect', 'activated', 'keystrokes_sent', 'focus_click', 'path', 'wheel_sent', 'completed_steps'):
        if key in structured:
            parts.append(f'{key}={structured[key]}')
    if 'delivery' in structured and isinstance(structured['delivery'], dict) and 'delivered_count' in structured['delivery']:
        parts.append(f"delivered_count={structured['delivery']['delivered_count']}")
    parts.append(f"isError={reply.get('isError')}")
    if not structured:
        parts.append('text=' + (reply.get('text') or '')[:120].replace('\n', ' '))
    received = rec.get('received') or {}
    for name in ('Target', 'Sentinel'):
        totals = (received.get(name) or {}).get('totals') or {}
        nonzero = {k: v for k, v in totals.items() if v and k != 'all'}
        parts.append(f"{name.lower()} input={nonzero or 0}")
    parts.append(f"activated={rec.get('target_activated')}")
    parts.append(f"fg_after={rec.get('foreground_after')}")
    return ', '.join(parts)


# ---------- expectation helpers ----------

def check(condition, note, notes):
    if not condition:
        notes.append(note)
    return bool(condition)


def totals(rec, name):
    return rec['received'][name]['totals']


def counts(rec, name, control):
    return rec['received'][name]['counts'].get(control, {})


def control_state(rec, when, control):
    return rec['target_' + when]['controls'].get(control)


def is_stale(rec, notes):
    structured = sc(rec)
    ok = check(structured.get('status') == 'refused' and (structured.get('refusal') or {}).get('code') == 'stale_element_token',
               f'expected the stale_element_token refusal, got {observed(rec)}', notes)
    check('could not be found live' in ((structured.get('refusal') or {}).get('message') or ''),
          'refusal message lacks the live-read detail', notes)
    return ok


def nothing_happened(rec, notes, ignore=()):
    ok = True
    for name in ('Target', 'Sentinel'):
        events = [e for e in rec['received'][name]['events'] if (e[2], e[3]) not in ignore and e[2] not in NOTES]
        ok &= check(not events, f'{name} received input: {rec["received"][name]["counts"]}', notes)
    ok &= check(not rec['target_activated'], f'target was activated: {rec["foreground_samples"]}', notes)
    ok &= check(rec['foreground_after'] == 'sentinel', f'sentinel not in front after: {rec["foreground_after"]}', notes)
    return ok


def no_requested_keys(rec, notes):
    ok = True
    for name in ('Target', 'Sentinel'):
        t = totals(rec, name)
        ok &= check(t['keys'] == 0 and t['text'] == 0, f'{name} received keys/text: {rec["received"][name]["counts"]}', notes)
    return ok


def verdict(ok):
    return 'PASS' if ok else 'FAIL'


def expect_stale(replaced=False):
    def evaluate(rec, notes):
        ok = is_stale(rec, notes)
        ok &= nothing_happened(rec, notes)
        if replaced:
            before, after = control_state(rec, 'before', 'Replacement'), control_state(rec, 'after', 'Replacement')
            ok &= check(before is not None and before == after, f'replacement changed: {before} -> {after}', notes)
        return verdict(ok)
    return evaluate


def succeeded(rec, notes):
    reply = rec['reply']
    return check(not reply.get('isError') and not reply.get('transport_error') and sc(rec).get('status') != 'refused',
                 f'action did not succeed: {observed(rec)} {reply.get("text", "")[:300]}', notes)


def only_on(rec, notes, control, kinds):
    """Mouse/wheel input of `kinds` went to `control` on the target and nowhere else."""
    ok = True
    for name in ('Target', 'Sentinel'):
        for other, per in rec['received'][name]['counts'].items():
            hits = {k: v for k, v in per.items() if k in kinds}
            if hits and not (name == 'Target' and other == control):
                ok &= check(False, f'{name}/{other} received {hits}', notes)
    return ok


def expect_tap(control, wanted):
    def evaluate(rec, notes):
        ok = succeeded(rec, notes)
        got = counts(rec, 'Target', control)
        for kind, number in wanted.items():
            # 'ldown+ldbl': a second press arrives as a double-click message when the class asks for one.
            have = sum(got.get(part, 0) for part in kind.split('+'))
            ok &= check(have == number, f'{control} {kind}={have}, expected {number}: {got}', notes)
        ok &= only_on(rec, notes, control, {'ldown', 'lup', 'ldbl', 'rdown', 'rup', 'rdbl', 'mdown', 'mup'})
        ok &= check(rec['target_activated'], 'target never activated', notes)
        notes.append(f"restore: fg_after={rec['foreground_after']} samples={rec['foreground_samples']}")
        return verdict(ok)
    return evaluate


def expect_insertion(control, text, caret=None, focus_click=False):
    def evaluate(rec, notes):
        ok = succeeded(rec, notes)
        before, after = control_state(rec, 'before', control), control_state(rec, 'after', control)
        index = one_insertion(before['text'], after['text'], text)
        ok &= check(index is not None, f'{control}: {before["text"]!r} -> {after["text"]!r} is not one insertion of {text!r}', notes)
        if caret is not None:
            ok &= check(index == caret, f'inserted at {index}, caret was {caret}', notes)
        ok &= check(before['selection'] == 0, f'field had a selection of {before["selection"]}', notes)
        chars = counts(rec, 'Target', control).get('char', 0)
        ok &= check(chars == len(text), f'{control} received {chars} chars', notes)
        for name in ('Target', 'Sentinel'):
            for other, per in rec['received'][name]['counts'].items():
                if per.get('char') and not (name == 'Target' and other == control):
                    ok &= check(False, f'{name}/{other} received text {per}', notes)
        for other, after_state in rec['target_after']['controls'].items():
            if other != control and 'text' in after_state:
                ok &= check(after_state['text'] == rec['target_before']['controls'][other]['text'], f'{other} text changed', notes)
        if focus_click:
            ok &= check(counts(rec, 'Target', control).get('ldown', 0) == 1, f'focus click count {counts(rec, "Target", control)}', notes)
        else:
            ok &= check(totals(rec, 'Target')['clicks'] == 0, 'direct focus sent a click', notes)
        notes.append(f'inserted at {index}; text {after["text"]!r}')
        return verdict(ok)
    return evaluate


def expect_after_activation(code, focus_click, clicked=None):
    def evaluate(rec, notes):
        structured = sc(rec)
        ok = check(structured.get('code') == code, f'code {structured.get("code")!r}, expected {code}: {rec["reply"].get("text", "")[:300]}', notes)
        ok &= check(structured.get('effect') == 'failed', f'effect {structured.get("effect")!r}', notes)
        ok &= check(structured.get('activated') is True, 'activated is not true', notes)
        ok &= check(structured.get('keystrokes_sent') is False, 'keystrokes_sent is not false', notes)
        ok &= check('status' not in structured and 'refusal' not in structured, 'reply carries a status/refusal', notes)
        ok &= check(structured.get('focus_click') == focus_click, f'focus_click {structured.get("focus_click")!r}, expected {focus_click}', notes)
        ok &= check(rec['reply'].get('isError') is True, 'isError is not true', notes)
        ok &= no_requested_keys(rec, notes)
        ok &= check(rec['target_activated'], 'target was not observed in front', notes)
        if clicked:
            ok &= check(counts(rec, 'Target', clicked).get('ldown', 0) == 1, f'{clicked} focus clicks {counts(rec, "Target", clicked)}', notes)
        else:
            ok &= check(totals(rec, 'Target')['clicks'] == 0, 'a click was sent', notes)
        assist = totals(rec, 'Target')['assist_keys'] + totals(rec, 'Sentinel')['assist_keys']
        if assist:
            notes.append(f'driver assist keys seen: {assist}')
        return verdict(ok)
    return evaluate


def stays_behind(rec, notes, settle_ms=500):
    """Foreground UIA scroll: the sentinel is in front at the end and has been
    since shortly after the reply. Brief changes during the call are recorded."""
    samples = rec['foreground_samples']
    ok = check(rec['foreground_after'] == 'sentinel', f'foreground ended on {rec["foreground_after"]}: {samples}', notes)
    late = [s for s in samples if s[0] > rec['reply_ms'] + settle_ms]
    ok &= check(not late, f'foreground changed after the reply: {late}', notes)
    notes.append(f'foreground during call: {samples or "unchanged"}; target activations '
                 f'{rec["target_after"]["activations"] - rec["target_before"]["activations"] if rec["target_after"]["activations"] >= rec["target_before"]["activations"] else "reset"}')
    if rec['stray_b9_keyups']:
        notes.append(f'stray 0xB9 key-up: {rec["stray_b9_keyups"]}')
    else:
        notes.append('no stray 0xB9 key-up')
    return ok


def reported_point(rec):
    match = re.search(r'at screen \((-?\d+),(-?\d+)\)', rec['reply'].get('text') or '')
    return (int(match.group(1)), int(match.group(2))) if match else None


def inside(point, rect):
    return point is not None and rect[0] <= point[0] < rect[2] and rect[1] <= point[1] < rect[3]


def expect_wheel_at(control, point_control=None):
    def evaluate(rec, notes):
        ok = succeeded(rec, notes)
        ok &= check('SendInput wheel' in (rec['reply'].get('text') or ''), 'reply is not the wheel route', notes)
        got = counts(rec, 'Target', control)
        ok &= check(got.get('wheel', 0) + got.get('hwheel', 0) >= 1, f'{control} received no wheel: {rec["received"]}', notes)
        ok &= only_on(rec, notes, control, {'wheel', 'hwheel'})
        rect = control_state(rec, 'after', point_control or control)['screen']
        point = reported_point(rec)
        ok &= check(inside(point, rect), f'wheel point {point} not inside {rect}', notes)
        notes.append(f'wheel point {point}, {point_control or control} live rect {rect}, events {got}')
        return verdict(ok)
    return evaluate


def scroll_consistency(control, value_key):
    """For a real or fixture provider: the reply must match what moved."""
    def evaluate(rec, notes):
        structured = sc(rec)
        before = control_state(rec, 'before', control) or {}
        after = control_state(rec, 'after', control) or before
        moved = (after.get(value_key, 0) or 0) - (before.get(value_key, 0) or 0)
        wheel_target = totals(rec, 'Target')['wheel']
        wheel_sentinel = totals(rec, 'Sentinel')['wheel']
        text = rec['reply'].get('text') or ''
        notes.append(f'{control} {value_key} {before.get(value_key)} -> {after.get(value_key)}; calls {after.get("calls")}; '
                      f'wheel target={wheel_target} sentinel={wheel_sentinel}; reply: {text[:200]}')
        behind = stays_behind(rec, [])
        notes.append(f'foreground: ended on {rec["foreground_after"]}, samples {rec["foreground_samples"]}, stays behind={behind}, '
                     f'stray 0xB9={rec["stray_b9_keyups"] or "none"}')
        if structured.get('code') == 'scroll_incomplete':
            ok = check(structured.get('effect') == 'partial', 'partial result without effect partial', notes)
            ok &= check(structured.get('wheel_sent') is False and wheel_target + wheel_sentinel == 0, 'wheel sent on a partial scroll', notes)
            ok &= check((structured.get('delivery') or {}).get('delivered_count') == structured.get('completed_steps'), 'delivered_count differs from completed_steps', notes)
            ok &= check(moved != 0, 'partial reported but nothing moved', notes)
            ok &= check('status' not in structured, 'partial carries a status', notes)
            return verdict(ok)
        if 'SendInput wheel' in text:
            # Covered target: any wheel lands on the sentinel, so a move here came from UIA.
            ok = check(moved == 0, f'COUNTEREXAMPLE: the provider moved {moved} and the driver also sent the wheel', notes)
            return verdict(ok)
        return 'INFO'
    return evaluate


try:
    # Target: green, at TARGET_HOME. Sentinel: magenta, covers the target fully.
    for name, bounds, colour in [('Target', TARGET_HOME, (0, 170, 0)), ('Sentinel', SENTINEL_HOME, (220, 0, 220))]:
        child = subprocess.Popen(['powershell.exe', '-NoProfile', '-STA', '-ExecutionPolicy', 'Bypass', '-File', str(here / 'liveness-fixture.ps1'),
                                  '-Source', str(here / 'liveness-fixture.cs'), '-Folder', str(out), '-Name', name] +
                                 [part for key, value in zip(('Left', 'Top', 'Width', 'Height', 'Red', 'Green', 'Blue'), bounds + colour)
                                  for part in ('-' + key, str(value))], creationflags=NO_WINDOW)
        children.append(child)
        if not wait_for(lambda: state(name).get('controls'), 90):
            errors = out / f'{name}-errors.log'
            raise RuntimeError(name + ' fixture did not start: ' + (errors.read_text(errors='replace') if errors.exists() else 'no error log'))
    target, sentinel = state('Target'), state('Sentinel')
    facts['fixture_integrity'] = integrity(target['pid'])
    facts['sentinel_integrity'] = integrity(sentinel['pid'])
    command('Target', 'away', sentinel['hwnd'])
    daemon_log = (out / 'daemon.log').open('w', encoding='utf-8')
    logs.append(daemon_log)
    daemon = subprocess.Popen([args.driver, 'serve', '--embedded', '--no-overlay', '--socket', pipe, '--dangerously-bypass-approvals'],
                              stdout=daemon_log, stderr=daemon_log, creationflags=NO_WINDOW)
    children.append(daemon)
    kernel.WaitNamedPipeW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD]
    if not wait_for(lambda: kernel.WaitNamedPipeW(pipe, 1), 30):
        raise RuntimeError('private driver pipe did not open')
    facts['driver_integrity'] = integrity(daemon.pid)
    facts['driver_version'] = subprocess.run([args.driver, '--version'], capture_output=True, text=True, creationflags=NO_WINDOW).stdout.strip()
    facts['start_session'] = raw('start_session', {})
    facts['sentinel_in_front_initially'] = front(sentinel['hwnd'])
    found, view = read_tokens()
    (out / 'window-state.json').write_text(json.dumps(view, indent=2), encoding='utf-8')
    facts['snapshot_labels'] = sorted(found)
    print('facts', json.dumps(facts), flush=True)

    ARGS = {'click': {}, 'double_click': {}, 'right_click': {}, 'press_key': {'key': 'q'},
            'hotkey': {'keys': ['ctrl', 'a']}, 'type_text': {'text': 'XY'}, 'scroll': {'direction': 'down', 'amount': 3}}
    VICTIM = {'click': 'Victim button', 'double_click': 'Victim button', 'right_click': 'Victim button',
              'press_key': 'Victim edit', 'hotkey': 'Victim edit', 'type_text': 'Victim edit', 'scroll': 'Victim list'}

    # 1. Removed control: refused as stale before anything happens.
    for tool in ARGS:
        run_row(f'removed-{tool}', 'removed control', f'{tool} on a control removed after the read', tool, VICTIM[tool],
                ARGS[tool], expect_stale(), mutate=[('remove', VICTIM[tool])])
    # 2. Replaced control: a different control at the same place gets nothing.
    for tool in ARGS:
        run_row(f'replaced-{tool}', 'replaced control', f'{tool} on a control replaced after the read', tool, VICTIM[tool],
                ARGS[tool], expect_stale(replaced=True), mutate=[('replace', VICTIM[tool])])
    # 3. Moved window: the action lands on the control at its new position, once.
    run_row('moved-click', 'moved window', 'click after the window moved', 'click', 'Victim button', {},
            expect_tap('Victim button', {'ldown': 1, 'lup': 1}), mutate=[('move',) + MOVE], tail=3.5)
    run_row('moved-double_click', 'moved window', 'double_click after the window moved', 'double_click', 'Victim button', {},
            expect_tap('Victim button', {'ldown+ldbl': 2, 'lup': 2}), mutate=[('move',) + MOVE], tail=3.5)
    run_row('moved-right_click', 'moved window', 'right_click after the window moved', 'right_click', 'Victim button', {},
            expect_tap('Victim button', {'rdown': 1, 'rup': 1}), mutate=[('move',) + MOVE], tail=3.5)
    run_row('moved-type_text', 'moved window', 'type_text after the window moved', 'type_text', 'Victim edit', {'text': 'XY'},
            expect_insertion('Victim edit', 'XY', caret=5), mutate=[('move',) + MOVE])
    run_row('moved-scroll', 'moved window', 'scroll (no ScrollPattern) after the window moved, target uncovered', 'scroll', 'Wheel pad',
            ARGS['scroll'], expect_wheel_at('Wheel pad'), mutate=[('move',) + MOVE], aside=True)
    # 4. Typing: one insertion into unselected existing text.
    run_row('type-direct-focus', 'typing', 'type_text into "hello| world" by UIA SetFocus', 'type_text', 'Filled edit', {'text': 'XY'},
            expect_insertion('Filled edit', 'XY', caret=5))
    run_row('type-fallback-click', 'typing', 'type_text into a field that rejects SetFocus but takes a click', 'type_text', 'Click focus edit',
            {'text': 'XY'}, expect_insertion('Click focus edit', 'XY', focus_click=True))
    # 5. Lost on activation.
    for tool in ('type_text', 'press_key', 'hotkey'):
        run_row(f'lost-on-activation-{tool}', 'lost on activation', f'{tool} on a control removed when its window activates', tool, 'Lose edit',
                ARGS[tool], expect_after_activation('foreground_element_lost', 'not_sent'), arm=[('arm-lose',)])
    # 6. Focus never confirmed; the focus click was sent.
    for tool in ('type_text', 'press_key', 'hotkey'):
        run_row(f'focus-never-{tool}', 'focus never confirmed', f'{tool} on a control that never keeps focus', tool, 'Bounce edit',
                ARGS[tool], expect_after_activation('foreground_unavailable', 'sent', clicked='Bounce edit'))
    # 7. Restore after a dispatched click, none after a refused one.

    def restore_dispatched(rec, notes):
        ok = succeeded(rec, notes)
        ok &= check(rec['target_activated'], 'target never activated', notes)
        samples = rec['foreground_samples']
        back = [ms for ms, name in samples if name == 'sentinel']
        ok &= check(rec['foreground_after'] == 'sentinel' and back, f'earlier foreground not restored: {samples}', notes)
        notes.append(f'samples {samples}')
        return verdict(ok)
    run_row('restore-after-click', 'restore', 'earlier foreground comes back after a dispatched click', 'click', 'Victim button', {},
            restore_dispatched, tail=4.0)

    def bring_target(rec):
        rec['harness_fronted_target'] = front(target['hwnd'])
        rec['harness_fronted_ms'] = round(time.monotonic() * 1000)

    def restore_refused(rec, notes):
        ok = is_stale(rec, notes)
        # The launcher's Alt tap that fronts the target is the harness's own input.
        ignore = {('syskeydown', 0x12), ('syskeyup', 0x12), ('keydown', 0x12), ('keyup', 0x12)}
        for name in ('Target', 'Sentinel'):
            events = [e for e in rec['received'][name]['events'] if (e[2], e[3]) not in ignore and e[2] not in NOTES]
            ok &= check(not events, f'{name} received input: {rec["received"][name]["counts"]}', notes)
        samples = rec['foreground_samples']
        reached = next((i for i, (_, name) in enumerate(samples) if name == 'target'), None)
        ok &= check(rec.get('harness_fronted_target') and reached is not None, f'harness could not front the target: {samples}', notes)
        later = [s for s in samples[(reached or 0) + 1:] if s[1] == 'sentinel']
        ok &= check(not later, f'LATE RESTORE after a refused click: {samples}', notes)
        ok &= check(rec['foreground_after'] == 'target', f'foreground ended on {rec["foreground_after"]}', notes)
        notes.append(f'samples {samples}')
        return verdict(ok)
    run_row('no-restore-after-refusal', 'restore', 'refused click, then the target is brought forward; nothing switches back',
            'click', 'Victim button', {}, restore_refused, mutate=[('remove', 'Victim button')], post=bring_target, tail=4.0)
    # 8. Scroll.

    def uia_scroll(rec, notes):
        ok = succeeded(rec, notes)
        ok &= check(sc(rec).get('route') == 'accessibility' and 'via UIA' in (rec['reply'].get('text') or ''),
                    f'not the UIA route: {sc(rec)}', notes)
        before, after = control_state(rec, 'before', 'Fixture list'), control_state(rec, 'after', 'Fixture list')
        ok &= check(after['top'] > before['top'], f'list top {before["top"]} -> {after["top"]}', notes)
        ok &= check(totals(rec, 'Target')['wheel'] + totals(rec, 'Sentinel')['wheel'] == 0, 'a wheel event was received', notes)
        ok &= stays_behind(rec, notes)
        notes.append(f'list top {before["top"]} -> {after["top"]}; reply delivery={sc(rec).get("delivery")}')
        return verdict(ok)
    run_row('scroll-uia', 'scroll', 'scroll a list with ScrollPattern: UIA, no wheel, target stays behind', 'scroll', 'Fixture list',
            ARGS['scroll'], uia_scroll, aside=True, tail=3.0)
    run_row('scroll-uia-covered', 'scroll', 'same, target covered', 'scroll', 'Fixture list', ARGS['scroll'], uia_scroll, tail=3.0)
    run_row('scroll-uia-background', 'scroll', 'contrast: background UIA scroll of the same list', 'scroll', 'Fixture list', ARGS['scroll'],
            uia_scroll, mode='background', tail=3.0)
    pad = run_row('scroll-wheel-live-centre', 'scroll', 'scroll a control with no ScrollPattern: wheel at its live centre', 'scroll', 'Wheel pad',
                  ARGS['scroll'], expect_wheel_at('Wheel pad'), aside=True)

    def click_after_scroll(rec, notes):
        ok = succeeded(rec, notes)
        before, after = control_state(rec, 'before', 'Wheel pad'), control_state(rec, 'after', 'Wheel pad')
        ok &= check(after['clicks'] == before['clicks'] + 1, f'Wheel pad clicks {before["clicks"]} -> {after["clicks"]}', notes)
        ok &= check(rec['daemon_alive'], 'driver daemon exited', notes)
        follow = raw('get_window_state', {'pid': target['pid'], 'window_id': target['hwnd'], 'include_screenshot': False})
        ok &= check(not follow.get('isError') and not follow.get('transport_error'), f'follow-up read failed: {follow}', notes)
        return verdict(ok)
    if pad.get('token'):
        run_row('click-after-wheel-scroll', 'scroll', 'click the same token after a wheel scroll (COM reference check)', 'click', 'Wheel pad', {},
                click_after_scroll, token=pad['token'], keep=True, aside=True, tail=1.5)

    def pad_two_point(rec):
        st = state('Target')
        rect = st['controls']['Wheel pad two']['screen']
        left, top, _, _ = frame(target['hwnd'])
        sx, sy = (rect[0] + rect[2]) // 2, (rect[1] + rect[3]) // 2
        rec['requested_screen_point'] = [sx, sy]
        rec['frame'] = frame(target['hwnd'])
        return dict(ARGS['scroll'], x=sx - (left + 1), y=sy - (top + 1))

    def wheel_at_xy(rec, notes):
        ok = expect_wheel_at('Wheel pad two', 'Wheel pad two')(rec, notes) == 'PASS'
        ok &= check(list(reported_point(rec) or []) == rec['requested_screen_point'],
                    f'wheel point {reported_point(rec)} != requested {rec["requested_screen_point"]}', notes)
        return verdict(ok)
    run_row('scroll-xy-live', 'scroll', 'scroll a live control with explicit x,y: wheel at that point', 'scroll', 'Wheel pad',
            pad_two_point, wheel_at_xy, aside=True)
    run_row('scroll-xy-removed', 'scroll', 'scroll a removed control with explicit x,y: nothing sent', 'scroll', 'Wheel pad',
            pad_two_point, lambda rec, notes: verdict(is_stale(rec, notes) & nothing_happened(rec, notes)),
            mutate=[('remove', 'Wheel pad')], aside=True)

    def occluded(rec, notes):
        notes.append(f'documented: the wheel does not raise the target. target={rec["received"]["Target"]["counts"]} '
                     f'sentinel={rec["received"]["Sentinel"]["counts"]} point={reported_point(rec)}')
        return 'INFO'
    run_row('scroll-wheel-covered', 'scroll', 'wheel fallback while another window covers the control (documented limit)', 'scroll',
            'Wheel pad', ARGS['scroll'], occluded)
    # 9. A real provider at its limits (target covered, so a wheel cannot move the target list).
    run_row('real-list-top-up', 'real provider', 'list at top, scroll up 1', 'scroll', 'Fixture list', {'direction': 'up', 'amount': 1},
            scroll_consistency('Fixture list', 'top'), tail=3.0)
    run_row('real-list-bottom-down', 'real provider', 'list at bottom, scroll down 1', 'scroll', 'Fixture list', {'direction': 'down', 'amount': 1},
            scroll_consistency('Fixture list', 'top'), tail=3.0, arm=[('listtop', 199, 'Fixture list')])
    run_row('real-list-near-bottom-down3', 'real provider', 'list a few rows from the bottom, scroll down 3', 'scroll', 'Fixture list',
            ARGS['scroll'], scroll_consistency('Fixture list', 'top'), tail=3.0, arm=[('listtop', 186, 'Fixture list')])
    # 10. Stretch: a custom UIA provider whose Scroll fails on purpose. Its scroll
    # percents move with its content (50 at the start) unless a mode makes the
    # percent getters fail.

    def custom(kind, completed=0, axis='vpos'):
        """kind: ok | partial | wheel (uncovered, pass/fail) | wheel-covered (INFO) | info."""
        def evaluate(rec, notes):
            structured = sc(rec)
            before = control_state(rec, 'before', 'Custom scroller')
            after = control_state(rec, 'after', 'Custom scroller') or {}
            moved = (after.get(axis, before[axis]) or 0) - (before[axis] or 0)
            text = rec['reply'].get('text') or ''
            calls = [[e[0] - rec['call_start_clock_ms'], e[2], e[3]] for e in rec['received']['Target']['events']
                     if e[1] == 'Custom scroller' and e[2] in NOTES and e[2] != 'gotfocus']
            wheels = {name: {c: v for c, v in rec['received'][name]['counts'].items() if v.get('wheel') or v.get('hwheel')}
                      for name in ('Target', 'Sentinel')}
            notes.append(f'{axis} {before[axis]} -> {after.get(axis)} (moved {moved}); calls {after.get("calls")} exists={bool(after)}; '
                         f'provider calls (ms, kind, value) {calls}; wheel received {wheels}; '
                         f'under wheel point {rec.get("window_under_reported_point")}; reply: {text[:220]}')
            ok = check('status' not in structured and 'refusal' not in structured, 'reply is a refusal', notes)
            wheel_total = totals(rec, 'Sentinel')['wheel'] + totals(rec, 'Target')['wheel']
            if kind == 'ok':
                ok &= succeeded(rec, notes)
                ok &= check(structured.get('route') == 'accessibility' and moved == 3, 'uia scroll did not complete', notes)
                ok &= check(wheel_total == 0, 'wheel sent', notes)
                ok &= stays_behind(rec, notes)
            elif kind == 'partial':
                ok &= check(structured.get('code') == 'scroll_incomplete', f'code {structured.get("code")!r}', notes)
                ok &= check(structured.get('effect') == 'partial', f'effect {structured.get("effect")!r}', notes)
                ok &= check((structured.get('delivery') or {}).get('delivered_count') == completed,
                            f'delivered_count {(structured.get("delivery") or {}).get("delivered_count")}', notes)
                ok &= check(structured.get('completed_steps') == completed, f'completed_steps {structured.get("completed_steps")}', notes)
                ok &= check(structured.get('wheel_sent') is False, 'wheel_sent is not false', notes)
                ok &= check(wheel_total == 0, 'wheel sent', notes)
                if after:
                    ok &= check(moved == completed, f'moved {moved}, reported {completed}', notes)
                ok &= stays_behind(rec, notes)
            elif kind == 'wheel':
                ok &= check('SendInput wheel' in text, 'no wheel fallback', notes)
                ok &= check(moved == 0, f'provider moved {moved} and the wheel was also sent', notes)
                got = counts(rec, 'Target', 'Custom scroller')
                ok &= check(got.get('wheel', 0) >= 1, 'Custom scroller received no wheel', notes)
                ok &= only_on(rec, notes, 'Custom scroller', {'wheel', 'hwheel'})
                ok &= check(inside(reported_point(rec), before['screen']), f'wheel point {reported_point(rec)} not inside {before["screen"]}', notes)
                behind = stays_behind(rec, [])
                notes.append(f'foreground ended on {rec["foreground_after"]}, samples {rec["foreground_samples"]}, stays behind={behind}')
            else:
                behind = stays_behind(rec, [])
                notes.append(f'foreground ended on {rec["foreground_after"]}, samples {rec["foreground_samples"]}, stays behind={behind}, '
                             f'stray 0xB9={rec["stray_b9_keyups"] or "none"}')
                if kind == 'wheel-covered':
                    notes.append('covered: the target stays behind, so the wheel goes to whatever is on top at that point (documented limit)')
                return 'INFO'
            return verdict(ok)
        return evaluate
    stretch = [
        # id, mode, direction, kind, completed, axis, aside
        ('custom-ok-down', 'ok', 'down', 'ok', 3, 'vpos', False),
        ('custom-fail-first-down', 'fail-first', 'down', 'wheel-covered', 0, 'vpos', False),
        ('custom-move-then-fail-down', 'move-then-fail', 'down', 'partial', 1, 'vpos', False),
        ('custom-fail-after-1-down', 'fail-after-1', 'down', 'partial', 1, 'vpos', False),
        ('custom-fail-after-2-down', 'fail-after-2', 'down', 'partial', 2, 'vpos', False),
        ('custom-remove-after-1-down', 'remove-after-1', 'down', 'partial', 1, 'vpos', False),
        ('custom-fail-after-1-right', 'fail-after-1', 'right', 'partial', 1, 'hpos', False),
        ('custom-fail-first-down-uncovered', 'fail-first', 'down', 'wheel', 0, 'vpos', True),
        ('custom-move-then-fail-down-uncovered', 'move-then-fail', 'down', 'partial', 1, 'vpos', True),
        ('custom-move-then-fail-nopercent-down-uncovered', 'move-then-fail-nopercent', 'down', 'info', 1, 'vpos', True),
    ]
    for rid, mode, direction, kind, completed, axis, aside in stretch:
        run_row(rid, 'custom provider', f'custom ScrollPattern {mode}, scroll {direction} 3' + (', target uncovered' if aside else ''),
                'scroll', 'Custom scroller', {'direction': direction, 'amount': 3}, custom(kind, completed, axis),
                arm=[('scroller', mode)], aside=aside, tail=3.0)
    # 11. Stretch: the fallback focus click returns an error (it completed with another window in front).
    for tool in ('type_text', 'press_key', 'hotkey'):
        run_row(f'focus-click-uncertain-{tool}', 'focus click error', f'{tool}: focus click completes with another window in front',
                tool, 'Click away edit', ARGS[tool], expect_after_activation('foreground_unavailable', 'uncertain', clicked='Click away edit'))
except Exception:
    facts['harness_error'] = traceback.format_exc()
    print(facts['harness_error'], flush=True)
finally:
    for child in reversed(children):
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
    for handle in logs:
        handle.close()
    facts['unknown_windows'] = {hex(k): v for k, v in unknown_windows.items()}
    (out / 'results.json').write_text(json.dumps({'facts': facts, 'rows': rows}, indent=2), encoding='utf-8')
    lines = ['| Row | Expected | Observed | Verdict | Notes |', '|---|---|---|---|---|']
    for rec in rows:
        notes = '; '.join(n.splitlines()[0][:220] for n in rec['notes'] if n).replace('|', '/')
        lines.append(f"| {rec['id']} | {rec['desc']} | {rec['observed'].replace('|', '/')} | {rec['verdict']} | {notes} |")
    (out / 'summary.md').write_text('\n'.join(lines) + '\n', encoding='utf-8')
    tally = {}
    for rec in rows:
        tally[rec['verdict']] = tally.get(rec['verdict'], 0) + 1
    print('TALLY', json.dumps(tally), flush=True)
sys.exit(4 if 'harness_error' in facts else 0)
