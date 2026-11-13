param(
    [Parameter(Mandatory)][string]$Driver,
    [Parameter(Mandatory)][string]$Python,
    [Parameter(Mandatory)][string]$Output,
    [switch]$SkipHighControl,
    [switch]$SkipLiveness
)
# Runs the background-input proof as a freshly created standard (non-administrator)
# local user, so the regular driver and the fixtures run at medium integrity on the
# interactive desktop. Hosted runners have UAC disabled, so the runner account is
# always high integrity; a separate standard account is the only ordinary-user route.
$ErrorActionPreference = 'Stop'
$Output = [IO.Path]::GetFullPath($Output)
$Driver = [IO.Path]::GetFullPath($Driver)
New-Item -ItemType Directory -Force $Output | Out-Null
$sessionId = (Get-Process -Id $PID).SessionId
Write-Host "Launcher session: $sessionId"
if ($sessionId -eq 0) { throw 'Background input proof requires an interactive session; this job runs in session 0' }
whoami /all | Out-File -Encoding utf8 (Join-Path $Output 'launcher-whoami.txt')
$pythonw = Join-Path (Split-Path $Python) 'pythonw.exe'
if (!(Test-Path $pythonw)) { throw "pythonw.exe not found next to $Python" }
$proof = Join-Path $PSScriptRoot 'windows-background.py'
$liveness = Join-Path $PSScriptRoot 'windows-liveness.py'

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
public static class BearlyProofLauncher {
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct Startup {
        public uint cb; public string reserved, desktop, title;
        public uint x,y,width,height,xchars,ychars,fill,flags;
        public ushort show,reservedSize; public IntPtr reservedBytes,input,output,error;
    }
    [StructLayout(LayoutKind.Sequential)] struct ProcessInfo { public IntPtr process,thread; public uint pid,tid; }
    [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)]
    static extern bool CreateProcessWithLogonW(string user,string domain,string password,uint logonFlags,string application,StringBuilder command,uint flags,IntPtr environment,string directory,ref Startup startup,out ProcessInfo result);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] public static extern uint WaitForSingleObject(IntPtr handle,uint milliseconds);
    [DllImport("kernel32.dll")] static extern bool GetExitCodeProcess(IntPtr process,out uint code);
    [DllImport("kernel32.dll")] public static extern bool TerminateProcess(IntPtr process,uint code);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] static extern IntPtr GetProcessWindowStation();
    [DllImport("user32.dll")] static extern IntPtr GetThreadDesktop(uint thread);
    [DllImport("user32.dll",SetLastError=true)] static extern bool GetUserObjectSecurity(IntPtr handle,ref uint info,[Out] byte[] data,uint size,out uint needed);
    [DllImport("user32.dll",SetLastError=true)] static extern bool SetUserObjectSecurity(IntPtr handle,ref uint info,byte[] data);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool BringWindowToTop(IntPtr hwnd);
    [DllImport("user32.dll")] static extern void keybd_event(byte key,byte scan,uint flags,UIntPtr extra);

    static byte[] Security(IntPtr handle,uint info) {
        uint needed;
        GetUserObjectSecurity(handle,ref info,null,0,out needed);
        var data=new byte[needed];
        if (!GetUserObjectSecurity(handle,ref info,data,needed,out needed)) throw new Win32Exception();
        return data;
    }
    public static string DesktopSddl() {
        // DACL plus mandatory label of the current window station and desktop.
        var station=new RawSecurityDescriptor(Security(GetProcessWindowStation(),0x14),0);
        var desktop=new RawSecurityDescriptor(Security(GetThreadDesktop(GetCurrentThreadId()),0x14),0);
        return "winsta0: "+station.GetSddlForm(AccessControlSections.All)+"\ndefault: "+desktop.GetSddlForm(AccessControlSections.All);
    }
    static void Grant(IntPtr handle,string sid,int mask) {
        var descriptor=new RawSecurityDescriptor(Security(handle,4),0);
        descriptor.DiscretionaryAcl.InsertAce(descriptor.DiscretionaryAcl.Count,
            new CommonAce(AceFlags.None,AceQualifier.AccessAllowed,mask,new SecurityIdentifier(sid),false,null));
        var updated=new byte[descriptor.BinaryLength];
        descriptor.GetBinaryForm(updated,0);
        uint info=4;
        if (!SetUserObjectSecurity(handle,ref info,updated)) throw new Win32Exception();
    }
    public static void GrantDesktop(string sid) {
        Grant(GetProcessWindowStation(),sid,0xF037F);
        Grant(GetThreadDesktop(GetCurrentThreadId()),sid,0xF01FF);
    }
    // CreateProcessWithLogonW with lpDesktop NULL: the child inherits winsta0\default
    // and the function grants the new logon access to them.
    public static IntPtr Start(string user,string password,string command,string directory,uint flags,out uint pid) {
        var startup=new Startup { cb=(uint)Marshal.SizeOf(typeof(Startup)) };
        ProcessInfo child;
        if (!CreateProcessWithLogonW(user,".",password,1,null,new StringBuilder(command),flags|0x400,IntPtr.Zero,directory,ref startup,out child))
            throw new Win32Exception();
        CloseHandle(child.thread);
        pid=child.pid;
        return child.process;
    }
    public static uint ExitCode(IntPtr process) {
        uint code;
        if (!GetExitCodeProcess(process,out code)) throw new Win32Exception();
        return code;
    }
    public static void Close(IntPtr handle) { CloseHandle(handle); }
    public static long Foreground() { return GetForegroundWindow().ToInt64(); }
    // Used only to put the proof's own sentinel window in front before each
    // measurement; the launcher is high integrity and owns the CI desktop.
    public static bool Front(long hwnd) {
        var target=new IntPtr(hwnd);
        for (int i=0;i<5 && GetForegroundWindow()!=target;i++) {
            keybd_event(0x12,0,0,UIntPtr.Zero);
            keybd_event(0x12,0,2,UIntPtr.Zero);
            SetForegroundWindow(target);
            BringWindowToTop(target);
            System.Threading.Thread.Sleep(100);
        }
        return GetForegroundWindow()==target;
    }
}
'@

function Wait-Proof([scriptblock]$HasExited, [string]$Folder, [int]$Seconds) {
    # Services the proof's requests to put its sentinel window in front.
    $request = Join-Path $Folder 'foreground-request.txt'
    $deadline = [datetime]::UtcNow.AddSeconds($Seconds)
    while (!(& $HasExited)) {
        if ([datetime]::UtcNow -gt $deadline) { return $false }
        if (Test-Path $request) {
            try {
                $hwnd = [long](Get-Content $request -Raw).Trim()
                Remove-Item $request -Force
                $ok = [BearlyProofLauncher]::Front($hwnd)
                Set-Content (Join-Path $Folder 'foreground-reply.txt') "$hwnd $ok $([BearlyProofLauncher]::Foreground())"
            } catch { }
        }
        Start-Sleep -Milliseconds 100
    }
    return $true
}

$user = 'bearlyproof'
$password = 'Bq7-' + [guid]::NewGuid().ToString('N').Substring(0, 24)
$medium = Join-Path $Output 'medium'
New-Item -ItemType Directory -Force $medium | Out-Null
$failure = $null
try {
    net user $user $password /add /y | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Create standard user failed' }
    $sid = (New-Object Security.Principal.NTAccount($user)).Translate([Security.Principal.SecurityIdentifier]).Value
    $membership = @{
        user = $user; sid = $sid
        administrators = @(net localgroup Administrators | Where-Object { $_ -and $_ -notmatch '^(Alias name|Comment|Members|-+|The command completed)' } | ForEach-Object { $_.Trim() })
        groups = @(net user $user | Select-String 'Group Memberships|^\s+\*' | ForEach-Object { $_.Line.Trim() })
    }
    $membership | ConvertTo-Json | Out-File -Encoding utf8 (Join-Path $Output 'standard-user.json')
    if ($membership.administrators -contains $user) { throw 'Proof user must not be an administrator' }
    foreach ($folder in @((Split-Path $Driver), $PSScriptRoot, (Split-Path $Python))) {
        icacls $folder /grant "*${sid}:(OI)(CI)RX" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Grant read access to $folder failed" }
    }
    icacls $medium /grant "*${sid}:(OI)(CI)M" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Grant proof folder access failed' }
    Set-Service seclogon -StartupType Manual -ErrorAction SilentlyContinue
    Start-Service seclogon
    # CreateProcessWithLogonW also grants the new logon access to the inherited
    # desktop; the explicit user grant is a fallback, and the SDDL is evidence.
    try {
        [BearlyProofLauncher]::DesktopSddl() | Out-File -Encoding utf8 (Join-Path $Output 'desktop-acl-before.txt')
        [BearlyProofLauncher]::GrantDesktop($sid)
        [BearlyProofLauncher]::DesktopSddl() | Out-File -Encoding utf8 (Join-Path $Output 'desktop-acl-after.txt')
    } catch { "Desktop grant failed: $_" | Out-File -Encoding utf8 (Join-Path $Output 'desktop-acl-error.txt') }

    # Probe: an ordinary console process as the standard user, reporting its groups.
    $probeOut = Join-Path $medium 'probe-whoami.txt'
    $probePid = [uint32]0
    $probe = [BearlyProofLauncher]::Start($user, $password, "cmd.exe /c whoami /groups > `"$probeOut`" 2>&1", $medium, 0x08000000, [ref]$probePid)
    [void][BearlyProofLauncher]::WaitForSingleObject($probe, 60000)
    $probeCode = [BearlyProofLauncher]::ExitCode($probe)
    [BearlyProofLauncher]::Close($probe)
    Write-Host "Standard user probe exit: $probeCode"
    if (Test-Path $probeOut) { Get-Content $probeOut | Select-String 'Mandatory' | ForEach-Object { Write-Host $_.Line } }

    $command = "`"$pythonw`" -I `"$proof`" --driver `"$Driver`" --output `"$medium`""
    $proofPid = [uint32]0
    $handle = [BearlyProofLauncher]::Start($user, $password, $command, $medium, 0, [ref]$proofPid)
    Write-Host "Medium proof pid: $proofPid"
    $finished = Wait-Proof { [BearlyProofLauncher]::WaitForSingleObject($handle, 0) -eq 0 } $medium 420
    if (!$finished) { [void][BearlyProofLauncher]::TerminateProcess($handle, 124) }
    $code = [BearlyProofLauncher]::ExitCode($handle)
    [BearlyProofLauncher]::Close($handle)
    Write-Host "Medium proof exit: $code"
    if (Test-Path (Join-Path $medium 'proof.log')) { Get-Content (Join-Path $medium 'proof.log') }
    if (Test-Path (Join-Path $medium 'proof-error.log')) { Get-Content (Join-Path $medium 'proof-error.log') }
    if ($code -ne 0) { $failure = "Medium proof exited $code" }

    if (!$SkipLiveness) {
        # Foreground liveness rows, same standard user, own folder, own daemon.
        $live = Join-Path $Output 'liveness'
        New-Item -ItemType Directory -Force $live | Out-Null
        icacls $live /grant "*${sid}:(OI)(CI)M" | Out-Null
        $command = "`"$pythonw`" -I `"$liveness`" --driver `"$Driver`" --output `"$live`""
        $livePid = [uint32]0
        $handle = [BearlyProofLauncher]::Start($user, $password, $command, $live, 0, [ref]$livePid)
        Write-Host "Liveness proof pid: $livePid"
        $finished = Wait-Proof { [BearlyProofLauncher]::WaitForSingleObject($handle, 0) -eq 0 } $live 1200
        if (!$finished) { [void][BearlyProofLauncher]::TerminateProcess($handle, 124) }
        $code = [BearlyProofLauncher]::ExitCode($handle)
        [BearlyProofLauncher]::Close($handle)
        Write-Host "Liveness proof exit: $code"
        if (Test-Path (Join-Path $live 'proof.log')) { Get-Content (Join-Path $live 'proof.log') }
        if (Test-Path (Join-Path $live 'proof-error.log')) { Get-Content (Join-Path $live 'proof-error.log') }
        Get-ChildItem $live -Filter '*-errors.log' -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "== $($_.Name)"; Get-Content $_.FullName }
        if ($code -ne 0 -and !$failure) { $failure = "Liveness proof exited $code" }
    }
} catch {
    $failure = "Medium launcher failed: $_"
} finally {
    Get-Process -IncludeUserName -ErrorAction SilentlyContinue | Where-Object { $_.UserName -like "*\$user" } |
        Stop-Process -Force -ErrorAction SilentlyContinue
    net user $user /delete 2>&1 | Out-Null
}

if (!$SkipHighControl) {
    # Comparison only: the same proof as the high integrity runner account. These
    # rows never count as ordinary-user evidence.
    $high = Join-Path $Output 'high-control'
    New-Item -ItemType Directory -Force $high | Out-Null
    $process = Start-Process -FilePath $pythonw -ArgumentList @('-I', "`"$proof`"", '--driver', "`"$Driver`"", '--output', "`"$high`"", '--allow-high') -WorkingDirectory $high -PassThru
    $null = $process.Handle
    $finished = Wait-Proof { $process.HasExited } $high 420
    if (!$finished) { $process.Kill() }
    Write-Host "High control exit: $($process.ExitCode)"
    if (Test-Path (Join-Path $high 'proof.log')) { Get-Content (Join-Path $high 'proof.log') }
}
if ($failure) { throw $failure }
