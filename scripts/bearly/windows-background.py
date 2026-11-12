"""Measure the release driver's background routes against owned WinForms windows.

Every route is recorded, never asserted: the driver's raw response, the fixture state
before and after, the foreground window and cursor before, during and after, and the
integrity level of this process, the driver daemon and the fixture. A route whose
background attempt has no effect is retried once with delivery_mode=foreground.
"""
import argparse
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import threading
import time
import traceback
import uuid
import zlib

parser = argparse.ArgumentParser()
parser.add_argument('--driver', required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--allow-high', action='store_true')
args = parser.parse_args()
out = Path(args.output).resolve()
out.mkdir(parents=True, exist_ok=True)
sys.stdout = (out / 'proof.log').open('w', encoding='utf-8', buffering=1)
sys.stderr = (out / 'proof-error.log').open('w', encoding='utf-8', buffering=1)
NO_WINDOW = 0x08000000

user = ctypes.WinDLL('user32', use_last_error=True)
kernel = ctypes.WinDLL('kernel32', use_last_error=True)
advapi = ctypes.WinDLL('advapi32', use_last_error=True)
user.GetForegroundWindow.restype = wintypes.HWND
user.IsWindowEnabled.argtypes = [wintypes.HWND]
user.SetForegroundWindow.argtypes = [wintypes.HWND]
user.WindowFromPoint.argtypes = [wintypes.POINT]
user.WindowFromPoint.restype = wintypes.HWND
user.GetAncestor.argtypes = [wintypes.HWND, wintypes.UINT]
user.GetAncestor.restype = wintypes.HWND
user.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user.GetCursorPos.argtypes = [ctypes.POINTER(wintypes.POINT)]
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


def cursor():
    point = wintypes.POINT()
    user.GetCursorPos(ctypes.byref(point))
    return [point.x, point.y]


me = integrity()
whoami = subprocess.run(['whoami', '/groups', '/fo', 'csv'], capture_output=True, text=True, creationflags=NO_WINDOW)
(out / 'whoami.txt').write_text(subprocess.run(['whoami'], capture_output=True, text=True, creationflags=NO_WINDOW).stdout + whoami.stdout, encoding='utf-8')
label_line = next((line for line in whoami.stdout.splitlines() if 'Mandatory Label' in line), '')
print('proof integrity', me, label_line, flush=True)
measured = me.get('level') == 'Medium'
if not measured and not args.allow_high:
    (out / 'results.json').write_text(json.dumps({'measured': False, 'proof_integrity': me, 'whoami_label': label_line}, indent=2))
    sys.exit(3)

fixture = out / 'fixture.ps1'
fixture.write_text(r'''
param([string]$Folder, [string]$Name, [int]$Left, [int]$Top, [int]$Width, [int]$Height, [int]$Red, [int]$Green, [int]$Blue)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
# Log fixture exceptions instead of opening WinForms' modal error dialog.
[Windows.Forms.Application]::SetUnhandledExceptionMode([Windows.Forms.UnhandledExceptionMode]::CatchException)
[Windows.Forms.Application]::add_ThreadException({ param($sender, $e) Add-Content (Join-Path $Folder "$Name-errors.log") $e.Exception.ToString() })
$form = New-Object Windows.Forms.Form
$form.Text = $Name
$form.StartPosition = 'Manual'
$form.SetBounds($Left, $Top, $Width, $Height)
$form.BackColor = [Drawing.Color]::FromArgb($Red, $Green, $Blue)
$form.KeyPreview = $true
$empty = New-Object Windows.Forms.TextBox
$empty.AccessibleName = 'Empty edit'
$empty.SetBounds(20, 20, 300, 30)
$filled = New-Object Windows.Forms.TextBox
$filled.AccessibleName = 'Filled edit'
$filled.Text = 'hello world'
$filled.SetBounds(20, 60, 300, 30)
$button = New-Object Windows.Forms.Button
$button.Text = 'Increment'
$button.AccessibleName = 'Fixture increment'
$button.SetBounds(20, 100, 140, 34)
$check = New-Object Windows.Forms.CheckBox
$check.Text = 'Fixture check'
$check.AccessibleName = 'Fixture check'
$check.SetBounds(180, 104, 160, 28)
$slider = New-Object Windows.Forms.TrackBar
$slider.AccessibleName = 'Fixture slider'
$slider.Minimum = 0
$slider.Maximum = 100
$slider.Value = 10
$slider.SetBounds(20, 145, 300, 45)
$list = New-Object Windows.Forms.ListBox
$list.AccessibleName = 'Fixture list'
$list.IntegralHeight = $false
$list.SetBounds(20, 200, 300, 120)
foreach ($i in 1..200) { [void]$list.Items.Add("Row $i") }
$script:clicks = 0
$script:keys = New-Object Collections.ArrayList
$button.Add_Click({ $script:clicks++ })
$form.Add_KeyDown({ [void]$script:keys.Add($_.KeyData.ToString()) })
$form.Controls.AddRange(@($empty, $filled, $button, $check, $slider, $list))
$timer = New-Object Windows.Forms.Timer
$timer.Interval = 50
$script:writeErrors = 0
$timer.Add_Tick({
  $active = if ($form.ActiveControl) { $form.ActiveControl.AccessibleName } else { '' }
  $json = @{ pid=$PID; hwnd=$form.Handle.ToInt64(); clicks=$script:clicks; empty=$empty.Text; filled=$filled.Text
     filled_caret=$filled.SelectionStart; filled_selection=$filled.SelectionLength; checked=$check.Checked
     slider=$slider.Value; list_top=$list.TopIndex; keys=@($script:keys); active=$active; focused=$form.ContainsFocus
     write_errors=$script:writeErrors } | ConvertTo-Json -Compress
  # Never let a state write raise: an unhandled WinForms exception opens a modal
  # error dialog that disables the fixture for the rest of the run.
  try {
    $temp = Join-Path $Folder "$Name.json.tmp"
    [IO.File]::WriteAllText($temp, $json)
    Move-Item -Force $temp (Join-Path $Folder "$Name.json")
  } catch { $script:writeErrors++ }
})
# Caret inside the non-empty field: "hello| world".
$form.Add_Shown({ $form.ActiveControl = $filled; $filled.Select(5, 0); $timer.Start() })
try { [Windows.Forms.Application]::Run($form) }
finally { $timer.Stop(); $timer.Dispose(); $form.Dispose() }
''', encoding='utf-8')

children, logs, rows = [], [], []
pipe = r'\\.\pipe\bearly-background-' + uuid.uuid4().hex
session = 'bearly-background-proof'
facts = {'measured_at_medium': measured, 'proof_integrity': me, 'whoami_label': label_line,
         'user': os.environ.get('USERNAME'), 'session_id': None}
sid = wintypes.DWORD()
kernel.ProcessIdToSessionId(os.getpid(), ctypes.byref(sid))
facts['session_id'] = sid.value


def state(name):
    for attempt in range(40):
        try:
            return json.loads((out / (name + '.json')).read_text(encoding='utf-8-sig'))
        except (OSError, ValueError):
            if attempt == 39:
                raise
            time.sleep(0.05)


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
        time.sleep(0.05)


def call(tool, values, screenshot=None):
    command = [args.driver, 'call', tool, json.dumps(dict(values, session=session)), '--socket', pipe]
    if screenshot:
        command += ['--screenshot-out-file', str(screenshot)]
    result = subprocess.run(command, capture_output=True, text=True, encoding='utf-8', timeout=60, creationflags=NO_WINDOW)
    try:
        body = json.loads(result.stdout)
        if isinstance(body, dict):
            body.pop('screenshot_png_b64', None)
    except ValueError:
        body = result.stdout.strip()
    return {'exit': result.returncode, 'body': body, 'stderr': result.stderr.strip()[-2000:]}


def tokens(target):
    view = call('get_window_state', {'pid': target['pid'], 'window_id': target['hwnd'], 'include_screenshot': False})
    body = view['body'] if isinstance(view['body'], dict) else {}
    return {item.get('label'): item for item in body.get('elements', []) if item.get('label')}, view


def front_sentinel():
    hwnd = sentinel['hwnd']
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


def classify(before, after, text, caret):
    if after == before:
        return 'unchanged'
    if after == before + text:
        return 'appended at end'
    if after == before[:caret] + text + before[caret:]:
        return f'inserted at caret {caret}'
    if after == text:
        return 'whole value replaced'
    return 'other'


def measure(route, operation, effect, timeout=4, mode='background'):
    """Run one driver action and record everything observable around it."""
    sentinel_front = front_sentinel() if mode == 'background' else None
    fg_before, cursor_before = foreground(), cursor()
    target_before, sentinel_before = state('Target'), state('Sentinel')
    changes, stopped = [], threading.Event()

    def observe():
        last = fg_before
        while not stopped.wait(0.002):
            current = foreground()
            if current != last:
                changes.append(current)
                last = current
    observer = threading.Thread(target=observe)
    observer.start()
    try:
        response = operation(mode)
        worked = bool(wait_for(lambda: effect(target_before, state('Target')), timeout))
        time.sleep(0.3)
    except Exception:
        response, worked = {'exception': traceback.format_exc()}, False
    finally:
        stopped.set()
        observer.join()
    target_after, sentinel_after = state('Target'), state('Sentinel')
    record = {
        'route': route, 'delivery_mode': mode, 'effect_observed': worked,
        'target_window_enabled': bool(user.IsWindowEnabled(target['hwnd'])),
        'sentinel_in_front_before': sentinel_front, 'target_hwnd': target['hwnd'], 'sentinel_hwnd': sentinel['hwnd'],
        'foreground_before': fg_before, 'foreground_after': foreground(), 'foreground_changes_sampled': changes,
        'target_became_foreground': target['hwnd'] in changes,
        'cursor_before': cursor_before, 'cursor_after': cursor(),
        'target_before': target_before, 'target_after': target_after,
        'sentinel_unchanged': sentinel_before == sentinel_after,
        'sentinel_changes': {k: [sentinel_before.get(k), v] for k, v in sentinel_after.items() if sentinel_before.get(k) != v},
        'response': response,
    }
    rows.append(record)
    print('ROW', route, mode, 'effect' if worked else 'NO EFFECT', 'fg', fg_before, '->', record['foreground_after'], changes, flush=True)
    return record


def read_png(path):
    data = Path(path).read_bytes()
    assert data[:8] == b'\x89PNG\r\n\x1a\n'
    pos, idat, width = 8, b'', 0
    while pos < len(data):
        length, kind = struct.unpack('>I4s', data[pos:pos + 8])
        chunk = data[pos + 8:pos + 8 + length]
        if kind == b'IHDR':
            width, height, depth, color = struct.unpack('>IIBB', chunk[:10])
        elif kind == b'IDAT':
            idat += chunk
        pos += 12 + length
    assert depth == 8 and color in (2, 6), (depth, color)
    bpp = 3 if color == 2 else 4
    raw, stride, pixels, prev = zlib.decompress(idat), width * bpp, [], bytearray(width * bpp)
    for y in range(height):
        kind = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b, c = prev[i], prev[i - bpp] if i >= bpp else 0
            if kind == 1:
                line[i] = (line[i] + a) & 255
            elif kind == 2:
                line[i] = (line[i] + b) & 255
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 255
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        pixels.append(line)
        prev = line
    return width, height, bpp, pixels


def colour_share(path):
    width, height, bpp, pixels = read_png(path)
    green = magenta = total = 0
    for line in pixels[::4]:
        for x in range(0, width * bpp, bpp * 4):
            r, g, b = line[x], line[x + 1], line[x + 2]
            total += 1
            green += r < 60 and g > 130 and b < 60
            magenta += r > 180 and g < 80 and b > 180
    return {'width': width, 'height': height, 'target_green_share': round(green / total, 3),
            'sentinel_magenta_share': round(magenta / total, 3)}


try:
    # Target: green, 520x380 at (160,160). Sentinel: magenta, covers the target fully.
    for name, bounds, colour in [('Target', (160, 160, 520, 380), (0, 170, 0)), ('Sentinel', (100, 100, 700, 520), (220, 0, 220))]:
        child = subprocess.Popen(['powershell.exe', '-NoProfile', '-STA', '-ExecutionPolicy', 'Bypass', '-File', str(fixture),
                                  '-Folder', str(out), '-Name', name] + [part for key, value in zip(('Left', 'Top', 'Width', 'Height', 'Red', 'Green', 'Blue'), bounds + colour)
                                  for part in ('-' + key, str(value))],
                                 creationflags=NO_WINDOW)
        children.append(child)
        assert wait_for(lambda: state(name), 30), name + ' fixture did not start'
    target, sentinel = state('Target'), state('Sentinel')
    facts['fixture_integrity'] = integrity(target['pid'])
    facts['sentinel_integrity'] = integrity(sentinel['pid'])
    daemon_log = (out / 'daemon.log').open('w', encoding='utf-8')
    logs.append(daemon_log)
    daemon = subprocess.Popen([args.driver, 'serve', '--embedded', '--no-overlay', '--socket', pipe, '--dangerously-bypass-approvals'],
                              stdout=daemon_log, stderr=daemon_log, creationflags=NO_WINDOW)
    children.append(daemon)

    def pipe_ready():
        try:
            with open(pipe, 'r+b', buffering=0):
                return True
        except OSError:
            return False
    assert wait_for(pipe_ready, 30), 'private driver pipe did not open'
    facts['driver_integrity'] = integrity(daemon.pid)
    facts['driver_version'] = subprocess.run([args.driver, '--version'], capture_output=True, text=True, creationflags=NO_WINDOW).stdout.strip()
    facts['start_session'] = call('start_session', {})
    facts['sentinel_in_front_initially'] = front_sentinel()
    centre = wintypes.POINT(160 + 260, 160 + 190)
    facts['window_at_target_centre'] = int(user.GetAncestor(user.WindowFromPoint(centre), 2) or 0)
    print('facts', json.dumps(facts), flush=True)

    # 1. Read window state.
    def read(mode):
        view = call('get_window_state', {'pid': target['pid'], 'window_id': target['hwnd'], 'include_screenshot': False})
        (out / 'window-state.json').write_text(json.dumps(view, indent=2), encoding='utf-8')
        body = view['body'] if isinstance(view['body'], dict) else {}
        found = {item.get('label'): {k: item.get(k) for k in ('role', 'value', 'actions', 'enabled', 'selected')}
                 for item in body.get('elements', []) if item.get('label') in
                 ('Empty edit', 'Filled edit', 'Fixture increment', 'Fixture check', 'Fixture slider', 'Fixture list')}
        view['body'] = {'total_element_count': body.get('total_element_count'), 'our_controls': found}
        return view
    rec = measure('read window state', read, lambda b, a: True, timeout=0)
    rec['read_ok'] = len(rec['response']['body'].get('our_controls', {})) == 6 if isinstance(rec['response'].get('body'), dict) else False

    # 2. Posted typing to the window (no element): goes to the window's focused field.
    posted_text = 'XY'
    actions = []

    def window_action(route, tool, extra, effect, after=None):
        operation = lambda mode: call(tool, dict(extra, pid=target['pid'], window_id=target['hwnd'], delivery_mode=mode))
        record = measure(route, operation, effect)
        if after:
            after(record)
        actions.append((route, operation, effect, after))

    def classify_posted(r):
        r['filled_classification'] = classify(r['target_before']['filled'], r['target_after']['filled'], posted_text, r['target_before']['filled_caret'])
        r['empty_classification'] = classify(r['target_before']['empty'], r['target_after']['empty'], posted_text, 0)
    window_action('type text to window (WM_CHAR, no element)', 'type_text', {'text': posted_text},
                  lambda b, a: a['filled'] != b['filled'] or a['empty'] != b['empty'], classify_posted)

    def element_action(route, tool, label, extra, effect, after=None):
        def operation(mode):
            found, view = tokens(target)
            if label not in found:
                return {'error': f'{label} not in snapshot', 'snapshot': view}
            return call(tool, dict(extra, pid=target['pid'], element_token=found[label]['element_token'], delivery_mode=mode))
        record = measure(route, operation, effect)
        if after:
            after(record)
        actions.append((route, operation, effect, after))
        return record

    element_action('type text into empty field (element)', 'type_text', 'Empty edit', {'text': 'typed-empty'},
                   lambda b, a: a['empty'] != b['empty'],
                   lambda r: r.update(classification=classify(r['target_before']['empty'], r['target_after']['empty'], 'typed-empty', 0)))
    element_action('type text into non-empty field (element)', 'type_text', 'Filled edit', {'text': '+tail'},
                   lambda b, a: a['filled'] != b['filled'],
                   lambda r: r.update(classification=classify(r['target_before']['filled'], r['target_after']['filled'], '+tail', r['target_before']['filled_caret'])))
    element_action('click button (element)', 'click', 'Fixture increment', {}, lambda b, a: a['clicks'] > b['clicks'])
    element_action('click checkbox (element)', 'click', 'Fixture check', {}, lambda b, a: a['checked'] != b['checked'])
    element_action('set value: text field', 'set_value', 'Empty edit', {'value': 'direct-value'}, lambda b, a: a['empty'] == 'direct-value')
    element_action('set value: slider', 'set_value', 'Fixture slider', {'value': '55'}, lambda b, a: a['slider'] == 55)
    element_action('set value: checkbox', 'set_value', 'Fixture check', {'value': 'true' }, lambda b, a: a['checked'] != b['checked'])
    element_action('press key q (element)', 'press_key', 'Empty edit', {'key': 'q'},
                   lambda b, a: len(a['keys']) > len(b['keys']) or a['empty'] != b['empty'])
    window_action('press key q (window, no element)', 'press_key', {'key': 'q'},
                  lambda b, a: len(a['keys']) > len(b['keys']) or a['empty'] != b['empty'] or a['filled'] != b['filled'])
    # Effective only if the control saw Ctrl held with A (select all), not two separate keys.
    element_action('hotkey ctrl+a (element)', 'hotkey', 'Filled edit', {'keys': ['ctrl', 'a']},
                   lambda b, a: 'A, Control' in a['keys'][len(b['keys']):] or (a['filled_selection'] == len(a['filled']) > 0))
    element_action('scroll list down 5 (element)', 'scroll', 'Fixture list', {'direction': 'down', 'amount': 5},
                   lambda b, a: a['list_top'] != b['list_top'])
    window_action('scroll window down 5 (window, no element)', 'scroll', {'direction': 'down', 'amount': 5},
                  lambda b, a: a['list_top'] != b['list_top'])

    # Screenshot of the covered target window only.
    shot = out / 'covered-target.png'

    def screenshot(mode):
        response = call('get_window_state', {'pid': target['pid'], 'window_id': target['hwnd'], 'include_accessibility_tree': False}, screenshot=shot)
        response['covered_by_sentinel'] = int(user.GetAncestor(user.WindowFromPoint(centre), 2) or 0) == sentinel['hwnd']
        response['png'] = colour_share(shot) if shot.exists() else None
        return response
    rec = measure('screenshot of covered window', screenshot, lambda b, a: True, timeout=0)
    png = rec['response'].get('png') or {}
    rec['effect_observed'] = png.get('target_green_share', 0) > 0.2 and png.get('sentinel_magenta_share', 1) < 0.02

    # Foreground escalation for each background route that had no effect.
    for route, operation, effect, after in actions:
        if not any(r['route'] == route and r['effect_observed'] for r in rows):
            record = measure(route, operation, effect, mode='foreground')
            if after:
                after(record)
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
    (out / 'results.json').write_text(json.dumps({'facts': facts, 'rows': rows}, indent=2), encoding='utf-8')
sys.exit(4 if 'harness_error' in facts else 0)
