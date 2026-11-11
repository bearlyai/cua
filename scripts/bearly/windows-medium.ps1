param([Parameter(Mandatory)][string]$Driver, [Parameter(Mandatory)][string]$Python, [Parameter(Mandatory)][string]$Output, [switch]$InteractiveBootstrap)
$ErrorActionPreference = 'Stop'
$Output = [IO.Path]::GetFullPath($Output)
New-Item -ItemType Directory -Force $Output | Out-Null
$sessionId = (Get-Process -Id $PID).SessionId
Write-Host "Proof launcher session: $sessionId"
if (!$InteractiveBootstrap) {
    $bootstrap = Join-Path $Output 'interactive-bootstrap.ps1'
    $result = Join-Path $Output 'bootstrap-exit.txt'
    $log = Join-Path $Output 'bootstrap.log'
    function Q([string]$Value) { return "'" + $Value.Replace("'", "''") + "'" }
    $invoke = '& ' + (Q $PSCommandPath) + ' -Driver ' + (Q $Driver) + ' -Python ' + (Q $Python) + ' -Output ' + (Q $Output) + ' -InteractiveBootstrap *> ' + (Q $log)
    $body = 'try { ' + $invoke + '; $code=0 } catch { $_ | Out-File -Append ' + (Q $log) + '; $code=1 }' + "`n"
    $body += '$code | Set-Content ' + (Q $result)
    Set-Content -Encoding UTF8 $bootstrap $body
    $name = 'BearlyDriverProof-' + [guid]::NewGuid().ToString('N')
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -ExecutionPolicy Bypass -File "' + $bootstrap + '"') -WorkingDirectory (Get-Location).Path
    $principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Limited
    try {
        Register-ScheduledTask -TaskName $name -Action $action -Principal $principal -Force | Out-Null
        Start-ScheduledTask -TaskName $name
        $deadline = [datetime]::UtcNow.AddMinutes(6)
        while (!(Test-Path $result) -and [datetime]::UtcNow -lt $deadline) { Start-Sleep -Seconds 2 }
        if (Test-Path $log) { Get-Content $log }
        if (!(Test-Path $result) -or [int](Get-Content $result -Raw) -ne 0) { throw 'Interactive proof bootstrap failed' }
    } finally {
        Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $name -Confirm:$false -ErrorAction SilentlyContinue
    }
    return
}
if ($sessionId -eq 0) { throw 'Background input proof requires an interactive session' }
$proof = Join-Path $PSScriptRoot 'windows-background.py'
$stdout = Join-Path $Output 'proof.log'
$stderr = Join-Path $Output 'proof-error.log'
$userSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
icacls $Output /grant ('*' + $userSid + ':(OI)(CI)M') /T | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Grant proof directory access failed' }
icacls $Output /setintegritylevel '(OI)(CI)M' /T | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Set proof directory integrity failed' }
# Hosted Windows runners can have UAC disabled, making even a Limited task high
# integrity. Create an actual medium token with administrator membership denied.
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
public static class BearlyMediumProof {
    [StructLayout(LayoutKind.Sequential)] struct SidAttributes { public IntPtr Sid; public uint Attributes; }
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct Startup {
        public uint cb; public string reserved, desktop, title;
        public uint x,y,width,height,xchars,ychars,fill,flags;
        public ushort show,reservedSize; public IntPtr reservedBytes,input,output,error;
    }
    [StructLayout(LayoutKind.Sequential)] struct ProcessInfo { public IntPtr process,thread; public uint pid,tid; }
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr handle);
    [DllImport("kernel32.dll")] static extern uint WaitForSingleObject(IntPtr handle,uint milliseconds);
    [DllImport("kernel32.dll")] static extern bool GetExitCodeProcess(IntPtr process,out uint code);
    [DllImport("kernel32.dll")] static extern bool TerminateProcess(IntPtr process,uint code);
    [DllImport("advapi32.dll",SetLastError=true)] static extern bool OpenProcessToken(IntPtr process,uint access,out IntPtr token);
    [DllImport("advapi32.dll",SetLastError=true)] static extern bool CreateRestrictedToken(IntPtr token,uint flags,uint disableCount,ref SidAttributes disabled,uint deleteCount,IntPtr deleted,uint restrictCount,IntPtr restricted,out IntPtr result);
    [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool ConvertStringSidToSid(string value,out IntPtr sid);
    [DllImport("advapi32.dll")] static extern uint GetLengthSid(IntPtr sid);
    [DllImport("advapi32.dll",SetLastError=true)] static extern bool SetTokenInformation(IntPtr token,int kind,ref SidAttributes value,uint size);
    [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool CreateProcessAsUser(IntPtr token,string application,StringBuilder command,IntPtr processSecurity,IntPtr threadSecurity,bool inherit,uint flags,IntPtr environment,string directory,ref Startup startup,out ProcessInfo result);
    [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool CreateProcessWithTokenW(IntPtr token,uint logonFlags,string application,StringBuilder command,uint flags,IntPtr environment,string directory,ref Startup startup,out ProcessInfo result);
    [DllImport("user32.dll")] static extern IntPtr GetProcessWindowStation();
    [DllImport("user32.dll")] static extern IntPtr GetThreadDesktop(uint thread);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll",SetLastError=true)] static extern bool GetUserObjectSecurity(IntPtr handle,ref uint info,[Out] byte[] data,uint size,out uint needed);
    [DllImport("user32.dll",SetLastError=true)] static extern bool SetUserObjectSecurity(IntPtr handle,ref uint info,byte[] data);
    static byte[] GrantDesktopAccess(IntPtr handle,string userSid,int mask) {
        uint info=4,needed;
        GetUserObjectSecurity(handle,ref info,null,0,out needed);
        var original=new byte[needed];
        if (!GetUserObjectSecurity(handle,ref info,original,needed,out needed)) throw new Win32Exception();
        var descriptor=new System.Security.AccessControl.RawSecurityDescriptor(original,0);
        descriptor.DiscretionaryAcl.InsertAce(descriptor.DiscretionaryAcl.Count,
            new System.Security.AccessControl.CommonAce(System.Security.AccessControl.AceFlags.None,
                System.Security.AccessControl.AceQualifier.AccessAllowed,mask,
                new System.Security.Principal.SecurityIdentifier(userSid),false,null));
        var updated=new byte[descriptor.BinaryLength];
        descriptor.GetBinaryForm(updated,0);
        if (!SetUserObjectSecurity(handle,ref info,updated)) throw new Win32Exception();
        return original;
    }
    public static uint Run(string command,string directory,string userSid) {
        IntPtr original=IntPtr.Zero,medium=IntPtr.Zero,admin=IntPtr.Zero,label=IntPtr.Zero;
        ProcessInfo child = new ProcessInfo();
        IntPtr container=GetProcessWindowStation(),desktop=GetThreadDesktop(GetCurrentThreadId());
        byte[] originalContainer=null,originalDesktop=null;
        try {
            originalContainer=GrantDesktopAccess(container,userSid,0xF037F);
            originalDesktop=GrantDesktopAccess(desktop,userSid,0xF01FF);
            if (!OpenProcessToken(GetCurrentProcess(),0xF01FF,out original)) throw new Win32Exception();
            if (!ConvertStringSidToSid("S-1-5-32-544",out admin)) throw new Win32Exception();
            var disabled = new SidAttributes { Sid=admin,Attributes=0 };
            if (!CreateRestrictedToken(original,1,1,ref disabled,0,IntPtr.Zero,0,IntPtr.Zero,out medium)) throw new Win32Exception();
            if (!ConvertStringSidToSid("S-1-16-8192",out label)) throw new Win32Exception();
            var mandatory = new SidAttributes { Sid=label,Attributes=0x20 };
            if (!SetTokenInformation(medium,25,ref mandatory,(uint)Marshal.SizeOf(typeof(SidAttributes))+GetLengthSid(label))) throw new Win32Exception();
            var startup = new Startup { cb=(uint)Marshal.SizeOf(typeof(Startup)),desktop="winsta0\\default" };
            if (!CreateProcessAsUser(medium,null,new StringBuilder(command),IntPtr.Zero,IntPtr.Zero,false,0x08000000,IntPtr.Zero,directory,ref startup,out child)) {
                if (!CreateProcessWithTokenW(medium,0,null,new StringBuilder(command),0x08000000,IntPtr.Zero,directory,ref startup,out child)) throw new Win32Exception();
            }
            if (WaitForSingleObject(child.process,300000) != 0) {
                TerminateProcess(child.process,124);
                throw new Exception("Medium proof exceeded five minutes");
            }
            uint code;
            if (!GetExitCodeProcess(child.process,out code)) throw new Win32Exception();
            return code;
        } finally {
            uint info=4;
            if (originalDesktop!=null) SetUserObjectSecurity(desktop,ref info,originalDesktop);
            if (originalContainer!=null) SetUserObjectSecurity(container,ref info,originalContainer);
            if (child.thread!=IntPtr.Zero) CloseHandle(child.thread);
            if (child.process!=IntPtr.Zero) CloseHandle(child.process);
            if (original!=IntPtr.Zero) CloseHandle(original);
            if (medium!=IntPtr.Zero) CloseHandle(medium);
            if (admin!=IntPtr.Zero) LocalFree(admin);
            if (label!=IntPtr.Zero) LocalFree(label);
        }
    }
}
'@
$command = '"' + $Python + '" "' + $proof + '" --driver "' + $Driver + '" --output "' + $Output + '"'
$code = [BearlyMediumProof]::Run($command, (Get-Location).Path, $userSid)
if (Test-Path $stdout) { Get-Content $stdout }
if (Test-Path $stderr) { Get-Content $stderr }
if ($code -ne 0) { throw "Medium integrity proof failed with exit $code; see evidence logs" }
