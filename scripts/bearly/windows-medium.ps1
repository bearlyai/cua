param([Parameter(Mandatory)][string]$Driver, [Parameter(Mandatory)][string]$Python, [Parameter(Mandatory)][string]$Output)
$ErrorActionPreference = 'Stop'
$Output = [IO.Path]::GetFullPath($Output)
New-Item -ItemType Directory -Force $Output | Out-Null
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
    public static uint Run(string command,string directory) {
        IntPtr original=IntPtr.Zero,medium=IntPtr.Zero,admin=IntPtr.Zero,label=IntPtr.Zero;
        ProcessInfo child = new ProcessInfo();
        try {
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
$code = [BearlyMediumProof]::Run($command, (Get-Location).Path)
if (Test-Path $stdout) { Get-Content $stdout }
if (Test-Path $stderr) { Get-Content $stderr }
if ($code -ne 0) { throw "Medium integrity proof failed with exit $code; see evidence logs" }
