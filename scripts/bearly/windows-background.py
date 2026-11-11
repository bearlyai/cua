"""Exercise the release driver against owned WinForms windows at medium integrity."""
import argparse
import ctypes
from ctypes import wintypes
import json
from pathlib import Path
import subprocess
import sys
import threading
import time
import uuid

parser = argparse.ArgumentParser()
parser.add_argument('--driver', required=True)
parser.add_argument('--output', required=True)
args = parser.parse_args()
out = Path(args.output).resolve()
out.mkdir(parents=True, exist_ok=True)
sys.stdout = (out / 'proof.log').open('w', encoding='utf-8', buffering=1)
sys.stderr = (out / 'proof-error.log').open('w', encoding='utf-8', buffering=1)
user = ctypes.WinDLL('user32', use_last_error=True)
kernel = ctypes.WinDLL('kernel32', use_last_error=True)
advapi = ctypes.WinDLL('advapi32', use_last_error=True)
user.GetForegroundWindow.restype = wintypes.HWND
kernel.GetCurrentProcess.restype = wintypes.HANDLE
kernel.CloseHandle.argtypes = [wintypes.HANDLE]
advapi.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
advapi.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
advapi.GetSidSubAuthorityCount.argtypes = [ctypes.c_void_p]
advapi.GetSidSubAuthorityCount.restype = ctypes.POINTER(ctypes.c_ubyte)
advapi.GetSidSubAuthority.argtypes = [ctypes.c_void_p, wintypes.DWORD]
advapi.GetSidSubAuthority.restype = ctypes.POINTER(wintypes.DWORD)
token = wintypes.HANDLE()
assert advapi.OpenProcessToken(kernel.GetCurrentProcess(), 8, ctypes.byref(token))
size = wintypes.DWORD()
advapi.GetTokenInformation(token, 25, None, 0, ctypes.byref(size))
data = ctypes.create_string_buffer(size.value)
assert advapi.GetTokenInformation(token, 25, data, size, ctypes.byref(size))
sid = ctypes.cast(data, ctypes.POINTER(ctypes.c_void_p))[0]
count = advapi.GetSidSubAuthorityCount(sid)[0]
rid = advapi.GetSidSubAuthority(sid, count - 1)[0]
kernel.CloseHandle(token)
assert 0x2000 <= rid < 0x3000, f'Requires medium integrity, got {rid:#x}'

fixture = out / 'fixture.ps1'
fixture.write_text(r'''
param([string]$Folder, [string]$Name)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$form = New-Object Windows.Forms.Form
$form.Text = $Name
$form.Width = 500
$form.Height = 280
$form.StartPosition = 'Manual'
$form.Left = 150
$form.Top = 150
$edit = New-Object Windows.Forms.TextBox
$edit.AccessibleName = 'Fixture edit'
$edit.SetBounds(30, 30, 350, 35)
$button = New-Object Windows.Forms.Button
$button.Text = 'Increment'
$button.AccessibleName = 'Fixture increment'
$button.SetBounds(30, 90, 160, 40)
$script:clicks = 0
$button.Add_Click({ $script:clicks++ })
$form.Controls.Add($edit)
$form.Controls.Add($button)
$timer = New-Object Windows.Forms.Timer
$timer.Interval = 50
$timer.Add_Tick({
  @{ pid=$PID; hwnd=$form.Handle.ToInt64(); clicks=$script:clicks; text=$edit.Text } |
    ConvertTo-Json -Compress | Set-Content -Encoding UTF8 (Join-Path $Folder "$Name.json")
})
$form.Add_Shown({ $edit.Focus(); $timer.Start() })
try { [Windows.Forms.Application]::Run($form) }
finally { $timer.Stop(); $timer.Dispose(); $form.Dispose() }
''', encoding='utf-8')
children = []
logs = []
records = []
pipe = r'\\.\pipe\bearly-background-' + uuid.uuid4().hex
session = 'bearly-background-proof'


def state(name):
    return json.loads((out / (name + '.json')).read_text(encoding='utf-8-sig'))


def wait_for(probe, label, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = probe()
            if value:
                return value
        except (OSError, ValueError):
            pass
        time.sleep(0.05)
    raise AssertionError('Timed out: ' + label)


def call(tool, values):
    values = dict(values, session=session)
    result = subprocess.run([args.driver, 'call', tool, '--socket', pipe, '--json-args', json.dumps(values)],
                            capture_output=True, text=True, encoding='utf-8', timeout=30)
    assert result.returncode == 0, (tool, result.stdout, result.stderr)
    try:
        return json.loads(result.stdout)
    except ValueError:
        return {'text': result.stdout}


def snapshot(target):
    return call('get_window_state', {'pid': target['pid'], 'window_id': target['hwnd']})


def element(view, label):
    return next(item for item in view['elements'] if item.get('label') == label)


def prove(route, operation, effect):
    before = int(user.GetForegroundWindow() or 0)
    assert before == sentinel['hwnd'], 'Sentinel must be foreground before each action'
    unexpected = []
    stopped = threading.Event()
    def observe():
        while not stopped.wait(0.002):
            current = int(user.GetForegroundWindow() or 0)
            if current != before:
                unexpected.append(current)
    observer = threading.Thread(target=observe)
    observer.start()
    try:
        response = operation()
        wait_for(effect, route + ' semantic effect')
        time.sleep(0.2)
    finally:
        stopped.set()
        observer.join()
    after = int(user.GetForegroundWindow() or 0)
    assert before == after and not unexpected, (route, 'foreground changed', unexpected, before, after)
    assert state('Sentinel')['clicks'] == 0 and state('Sentinel')['text'] == '', 'Input leaked into sentinel'
    records.append({'route': route, 'effect_verified': True, 'foreground_before': before,
                    'foreground_after': after, 'foreground_changes_sampled': unexpected, 'response': response})
    print('PASS', route, flush=True)


try:
    for name in ['Target', 'Sentinel']:
        child = subprocess.Popen(['powershell.exe', '-NoProfile', '-STA', '-ExecutionPolicy', 'Bypass',
                                  '-File', str(fixture), '-Folder', str(out), '-Name', name])
        children.append(child)
        wait_for(lambda: state(name), name + ' fixture')
        if name == 'Target':
            target = state(name)
    sentinel = state('Sentinel')
    wait_for(lambda: int(user.GetForegroundWindow() or 0) == sentinel['hwnd'], 'sentinel foreground')
    daemon_log = (out / 'daemon.log').open('w', encoding='utf-8')
    logs.append(daemon_log)
    daemon = subprocess.Popen([args.driver, 'serve', '--embedded', '--no-overlay', '--socket', pipe,
                               '--dangerously-bypass-approvals'], stdout=daemon_log, stderr=daemon_log)
    children.append(daemon)
    # Probe the private pipe, never a globally installed daemon.
    def pipe_ready():
        try:
            with open(pipe, 'r+b', buffering=0):
                return True
        except OSError:
            return False
    wait_for(pipe_ready, 'private regular daemon pipe', timeout=30)
    call('start_session', {})
    view = snapshot(target)
    (out / 'initial-state.json').write_text(json.dumps(view, indent=2), encoding='utf-8')
    increment = element(view, 'Fixture increment')['element_token']
    prove('UIA Invoke background click',
          lambda: call('click', {'pid': target['pid'], 'element_token': increment, 'delivery_mode': 'background'}),
          lambda: state('Target')['clicks'] == 1)
    view = snapshot(target)
    edit = element(view, 'Fixture edit')['element_token']
    prove('UIA ValuePattern background type',
          lambda: call('type_text', {'pid': target['pid'], 'element_token': edit, 'text': 'uia-text', 'delivery_mode': 'background'}),
          lambda: state('Target')['text'] == 'uia-text')
    view = snapshot(target)
    edit = element(view, 'Fixture edit')['element_token']
    call('set_value', {'pid': target['pid'], 'element_token': edit, 'value': '', 'delivery_mode': 'background'})
    wait_for(lambda: state('Target')['text'] == '', 'empty field before posted text')
    prove('PostMessage WM_CHAR background type',
          lambda: call('type_text', {'pid': target['pid'], 'window_id': target['hwnd'], 'text': 'posted-only', 'delivery_mode': 'background'}),
          lambda: state('Target')['text'] == 'posted-only')
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
    (out / 'results.json').write_text(json.dumps({'integrity_rid': rid, 'routes': records,
        'foreground_observation': 'before/after plus 2ms sampling during each action',
        'unverified': ['elevated targets', 'secure desktop', 'UIAccess', 'Chromium/WinUI/WPF', 'pixel clicks', 'scroll', 'drag', 'menus']}, indent=2), encoding='utf-8')
