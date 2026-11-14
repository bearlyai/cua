"""Replay the Bearly desktop app's driver calls against a built driver.

The released desktop app starts the driver with
`serve --embedded --host-bundle-id <id> --socket <pipe>`, waits for
`status --socket <pipe>`, lists tools with `list-tools`, and sends every tool
call as `call <tool> <json> --socket <pipe>` with `session` set. This script
runs that exact sequence with the same environment and argument encoding, then
checks every row of `bearly-host-calls.json` listed for this platform against
the running daemon's advertised input schemas.

A row fails when the process exits non-zero, when a reply's code is
`invalid_arguments`, or when the desktop app's own parsing would treat the
reply as a failure. Checks the dashboard makes on top of that are reported as
WARN. Rows are written to results.json and summary.md, and printed to
proof.log.
"""
import argparse
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import threading
import time
import traceback
import uuid

parser = argparse.ArgumentParser()
parser.add_argument('--driver', required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--fixture', default=None)
parser.add_argument('--bundle-id', default='com.bearly.app')
args = parser.parse_args()
out = Path(args.output).resolve()
out.mkdir(parents=True, exist_ok=True)
sys.stdout = (out / 'proof.log').open('w', encoding='utf-8', buffering=1)
sys.stderr = (out / 'proof-error.log').open('w', encoding='utf-8', buffering=1)
here = Path(__file__).resolve().parent
fixture_path = Path(args.fixture) if args.fixture else here.parent.parent / 'libs' / 'cua-driver' / 'compat-fixtures' / 'bearly-host-calls.json'
WINDOWS = os.name == 'nt'
PLATFORM = 'windows' if WINDOWS else ('macos' if sys.platform == 'darwin' else 'linux')
NO_WINDOW = 0x08000000 if WINDOWS else 0

# The desktop app's process environment for every driver command.
HOST_ENV = {
    'CUA_DRIVER_RS_TELEMETRY_ENABLED': 'false',
    'CUA_DRIVER_TELEMETRY_ENABLED': 'false',
    'CUA_TELEMETRY': 'off',
    'CUA_TELEMETRY_ENABLED': '0',
    'LUME_TELEMETRY_ENABLED': 'false',
    'CUA_DRIVER_EMBEDDED': '1',
    'CUA_DRIVER_HOST_BUNDLE_ID': args.bundle_id,
}
ENV = dict(os.environ, **HOST_ENV)
INSTANCE = uuid.uuid4().hex[:8]
if WINDOWS:
    SOCKET = rf'\\.\pipe\bearly-cua-driver-{os.getpid()}-{INSTANCE}'
else:
    SOCKET = str(out / f'cua-driver-{os.getpid()}-{INSTANCE}.sock')
PUBLIC_SESSION = 'bearly'
RUN_SESSION = f'bearly-hostcalls-{uuid.uuid4().hex[:12]}'
MAX_SCREENSHOT_DIMENSION = 1280
DAEMON_START_TIMEOUT = 10.0
rows = []
facts = {'platform': PLATFORM, 'driver': args.driver, 'socket': SOCKET, 'session': RUN_SESSION,
         'fixture': str(fixture_path), 'env': HOST_ENV}


def js(value):
    """JSON.stringify's encoding: no spaces, insertion order."""
    return json.dumps(value, separators=(',', ':'), ensure_ascii=False)


def run(argv, timeout):
    started = time.monotonic()
    try:
        done = subprocess.run([args.driver, *argv], env=ENV, capture_output=True, text=True, encoding='utf-8',
                              errors='replace', timeout=timeout, creationflags=NO_WINDOW, stdin=subprocess.DEVNULL)
        code, stdout, stderr = done.returncode, done.stdout, done.stderr
    except subprocess.TimeoutExpired as expired:
        code, stdout, stderr = 'timeout', expired.stdout or '', expired.stderr or ''
        if isinstance(stdout, bytes):
            stdout = stdout.decode('utf-8', 'replace')
        if isinstance(stderr, bytes):
            stderr = stderr.decode('utf-8', 'replace')
    return {'argv': argv, 'exit': code, 'stdout': stdout, 'stderr': stderr,
            'ms': int((time.monotonic() - started) * 1000)}


def parse_json(text):
    """The desktop app's parseJsonOutput: the whole stdout as JSON, or None."""
    if not text.strip():
        return None
    try:
        return json.loads(text)
    except ValueError:
        return None


def invalid_arguments(value):
    if not isinstance(value, dict):
        return False
    refusal = value.get('refusal') if isinstance(value.get('refusal'), dict) else {}
    return value.get('code') == 'invalid_arguments' or refusal.get('code') == 'invalid_arguments'


def short(text, limit=400):
    text = (text or '').strip().replace('\r', '')
    return text if len(text) <= limit else text[:limit] + '…'


def record(row_id, expected, result, verdict, notes):
    stdout = result.get('stdout', '') if result else ''
    parsed = parse_json(stdout)
    if isinstance(parsed, dict):
        for key in ('screenshot_png_b64',):
            if key in parsed:
                parsed[key] = f'<{len(parsed[key])} chars>'
    rec = {'id': row_id, 'desc': expected, 'verdict': verdict, 'notes': notes,
           'argv': result.get('argv') if result else None, 'exit': result.get('exit') if result else None,
           'ms': result.get('ms') if result else None,
           'reply': parsed if parsed is not None else short(stdout, 4000), 'stderr': short(result.get('stderr', '') if result else '', 4000)}
    rec['observed'] = f"exit {rec['exit']}" if result else 'schema check'
    rows.append(rec)
    print(f"ROW {row_id} {verdict}: exit={rec['exit']} {'; '.join(notes)}", flush=True)
    return rec


def call_row(row_id, expected, tool, values, timeout=60, check=None):
    """One `call` exactly as daemonCallArgs builds it: session first, then the call's own keys."""
    payload = dict({'session': PUBLIC_SESSION}, **values)
    result = run(['call', tool, js(payload), '--socket', SOCKET], timeout)
    parsed = parse_json(result['stdout'])
    notes = [f'args {js(payload)}']
    verdict = 'PASS'
    if result['exit'] != 0:
        verdict = 'FAIL'
        notes.append(f"desktop app: non-zero exit is a failure; stderr {short(result['stderr'], 300)!r}; stdout {short(result['stdout'], 300)!r}")
    if invalid_arguments(parsed):
        verdict = 'FAIL'
        notes.append('reply code invalid_arguments')
    if check and verdict == 'PASS':
        verdict, more = check(parsed, result)
        notes.extend(more)
    return record(row_id, expected, result, verdict, notes), parsed


def permissions_check(parsed, result):
    # handlePermissions: a reply that is not JSON is invalid_permissions_response.
    if parsed is None:
        return 'FAIL', ['desktop app: stdout is not JSON (invalid_permissions_response)']
    return 'PASS', [f'reply keys {sorted(parsed)[:12] if isinstance(parsed, dict) else type(parsed).__name__}']


def set_config_check(parsed, result):
    # configureCaptureScope: a JSON record with a string `code` or status `refused` is a failure.
    if isinstance(parsed, dict) and (isinstance(parsed.get('code'), str) or parsed.get('status') == 'refused'):
        return 'FAIL', [f"desktop app: configuration refused: {short(result['stdout'], 300)!r}"]
    return 'PASS', []


def windows_check(parsed, result):
    # The dashboard reads `windows` as an array of records with bounds, app_name, title, pid, window_id,
    # is_on_screen and z_index.
    windows = parsed.get('windows') if isinstance(parsed, dict) else None
    if not isinstance(windows, list):
        return 'WARN', ['dashboard: list_windows reply has no windows array']
    usable = [w for w in windows if isinstance(w, dict) and all(k in w for k in ('app_name', 'title', 'pid', 'window_id', 'is_on_screen', 'z_index', 'bounds'))]
    return 'PASS', [f'{len(windows)} windows, {len(usable)} with every field the dashboard reads']


def desktop_check(parsed, result):
    # The use_computer agent's parseObservation plus screen size.
    if not isinstance(parsed, dict):
        return 'WARN', ['dashboard: get_desktop_state reply is not a JSON object']
    missing = [k for k in ('screenshot_width', 'screenshot_height', 'screenshot_mime_type', 'screenshot_png_b64', 'screen_width', 'screen_height') if k not in parsed]
    if missing:
        return 'WARN', [f'dashboard: get_desktop_state reply lacks {missing}']
    if parsed.get('screenshot_mime_type') != 'image/png':
        return 'WARN', [f"dashboard: screenshot_mime_type {parsed.get('screenshot_mime_type')!r}, the agent reads only image/png"]
    return 'PASS', [f"screenshot {parsed['screenshot_width']}x{parsed['screenshot_height']}, screen {parsed['screen_width']}x{parsed['screen_height']}"]


def health_check(parsed, result):
    # probeComputerWindowTools reads `driver_version` as a string.
    if not isinstance(parsed, dict) or not isinstance(parsed.get('driver_version'), str):
        return 'WARN', ['dashboard: health_report reply has no driver_version string']
    return 'PASS', [f"driver_version {parsed['driver_version']}"]


def daemon_list():
    """The running daemon's own tool list, with each tool's advertised input schema."""
    request = (js({'method': 'list'}) + '\n').encode('utf-8')
    data = b''
    if WINDOWS:
        with open(SOCKET, 'r+b', buffering=0) as handle:
            handle.write(request)
            while b'\n' not in data:
                chunk = handle.read(1 << 16)
                if not chunk:
                    break
                data += chunk
    else:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(30)
            connection.connect(SOCKET)
            connection.sendall(request)
            while b'\n' not in data:
                chunk = connection.recv(1 << 16)
                if not chunk:
                    break
                data += chunk
    response = json.loads(data.split(b'\n', 1)[0].decode('utf-8'))
    if not response.get('ok'):
        raise RuntimeError(f"daemon list failed: {response.get('error')}")
    return {tool['name']: tool for tool in response['result']['tools']}


daemon = None
daemon_logs = []
try:
    fixture = json.loads(fixture_path.read_text(encoding='utf-8'))

    # binaries.ts verifies the downloaded driver with --version.
    result = run(['--version'], 30)
    record('version', '`--version` exits 0 (the app verifies the download with it)', result,
           'PASS' if result['exit'] == 0 else 'FAIL', [short(result['stdout'], 120)])

    # startEmbeddedDaemon first asks whether a daemon is already listening; no daemon yet is expected.
    result = run(['status', '--socket', SOCKET], 3)
    record('status-before-serve', '`status --socket` before serve: not listening (INFO)', result, 'INFO',
           ['the app starts the daemon when this fails'])

    stdout_log = (out / 'daemon-stdout.log').open('w', encoding='utf-8')
    stderr_log = (out / 'daemon-stderr.log').open('w', encoding='utf-8')
    daemon_logs = [stdout_log, stderr_log]
    serve_argv = ['serve', '--embedded', '--host-bundle-id', args.bundle_id, '--socket', SOCKET]
    daemon = subprocess.Popen([args.driver, *serve_argv], env=ENV, stdout=stdout_log, stderr=stderr_log,
                              stdin=subprocess.DEVNULL, creationflags=NO_WINDOW)
    facts['daemon_pid'] = daemon.pid

    # waitForDaemon: `status --socket` every 100 ms for up to 10 s.
    started = time.monotonic()
    ready, attempts, result = False, 0, None
    while time.monotonic() - started < DAEMON_START_TIMEOUT:
        attempts += 1
        result = run(['status', '--socket', SOCKET], 3)
        if result['exit'] == 0:
            ready = True
            break
        if daemon.poll() is not None:
            break
        time.sleep(0.1)
    result = dict(result or {}, argv=serve_argv)
    record('serve-ready', '`serve --embedded --host-bundle-id … --socket …` answers `status --socket` within 10 s', result,
           'PASS' if ready else 'FAIL',
           [f'{attempts} status attempts, {int((time.monotonic() - started) * 1000)} ms',
            f'daemon exit {daemon.poll()}' if daemon.poll() is not None else 'daemon running'])
    if not ready:
        raise RuntimeError('daemon never became ready')

    # handlePermissions.
    call_row('check_permissions', '`call check_permissions {"session":"bearly","prompt":false}`: exit 0 and a JSON reply',
             'check_permissions', {'prompt': False}, timeout=30, check=permissions_check)

    # ensureCallableTool: `list-tools` (no socket), one `name:` line per tool.
    result = run(['list-tools'], 15)
    names = {m.group(1) for line in result['stdout'].splitlines() if (m := re.match(r'^([a-z0-9_]{1,96}):', line))}
    wanted = sorted({row['tool'] for row in fixture['calls'] if PLATFORM in row['platforms']})
    missing = [name for name in wanted if name not in names]
    verdict = 'PASS' if result['exit'] == 0 and names and not missing else 'FAIL'
    record('list-tools', '`list-tools`: exit 0 and every tool the hosts call is listed', result, verdict,
           [f'{len(names)} tools', f'missing {missing}' if missing else 'none missing'])

    # A call with a capture scope first configures that session (configureCaptureScope), then runs.
    call_row('set_config', '`call set_config` with the session and max_image_dimension: exit 0, not refused',
             'set_config', {'session': RUN_SESSION, 'max_image_dimension': MAX_SCREENSHOT_DIMENSION}, timeout=15, check=set_config_check)
    call_row('start_session', '`call start_session` for the run: exit 0', 'start_session', {'session': RUN_SESSION}, timeout=15)
    call_row('list_windows', '`call list_windows`: exit 0', 'list_windows', {'session': RUN_SESSION}, timeout=30, check=windows_check)
    call_row('get_desktop_state', '`call get_desktop_state` (the use_computer screenshot): exit 0', 'get_desktop_state',
             {'session': RUN_SESSION}, timeout=60, check=desktop_check)
    call_row('health_report', '`call health_report {"include":["binary_version"]}` (window tools probe): exit 0',
             'health_report', {'include': ['binary_version']}, timeout=5, check=health_check)
    call_row('end_session', '`call end_session` for the run: exit 0', 'end_session', {'session': RUN_SESSION}, timeout=15)

    # Every fixture row for this platform against the running daemon's advertised schemas: the
    # tool exists, and a closed schema lists every key (the dispatch argument check).
    tools = daemon_list()
    for index, row in enumerate(fixture['calls']):
        if PLATFORM not in row['platforms']:
            continue
        tool = tools.get(row['tool'])
        notes = [row['caller']]
        if tool is None:
            verdict = 'FAIL'
            notes.append('tool not registered')
        else:
            schema = tool.get('input_schema') or {}
            properties = schema.get('properties') or {}
            unknown = [key for key in row['args'] if schema.get('additionalProperties') is False and key not in properties]
            verdict = 'FAIL' if unknown else 'PASS'
            notes.append(f'unknown argument {unknown}' if unknown else
                         ('open schema' if schema.get('additionalProperties') is not False else f"keys {sorted(row['args'])}"))
        record(f"schema-{index:02d}-{row['tool']}", f"`{row['tool']}` accepts every key the host sends", None, verdict, notes)
except Exception:
    facts['harness_error'] = traceback.format_exc()
    print(facts['harness_error'], flush=True)
finally:
    if daemon is not None and daemon.poll() is None:
        # The app stops the daemon by aborting its process.
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
    for handle in daemon_logs:
        handle.close()
    (out / 'results.json').write_text(json.dumps({'facts': facts, 'rows': rows}, indent=2, default=str), encoding='utf-8')
    lines = ['| Row | Expected | Observed | Verdict | Notes |', '|---|---|---|---|---|']
    for rec in rows:
        notes = '; '.join(str(n).splitlines()[0][:220] for n in rec['notes'] if n).replace('|', '/')
        lines.append(f"| {rec['id']} | {rec['desc'].replace('|', '/')} | {rec['observed']} | {rec['verdict']} | {notes} |")
    (out / 'summary.md').write_text('\n'.join(lines) + '\n', encoding='utf-8')
    tally = {}
    for rec in rows:
        tally[rec['verdict']] = tally.get(rec['verdict'], 0) + 1
    print('TALLY', json.dumps(tally), flush=True)
if 'harness_error' in facts:
    sys.exit(4)
sys.exit(5 if any(rec['verdict'] == 'FAIL' for rec in rows) else 0)
