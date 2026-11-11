param([Parameter(Mandatory)][string]$Driver, [Parameter(Mandatory)][string]$Python, [Parameter(Mandatory)][string]$Output)
$ErrorActionPreference = 'Stop'
$Output = [IO.Path]::GetFullPath($Output)
New-Item -ItemType Directory -Force $Output | Out-Null
$proof = Join-Path $PSScriptRoot 'windows-background.py'
$job = Join-Path $Output 'run-proof.ps1'
$exitFile = Join-Path $Output 'exit.txt'
$stdout = Join-Path $Output 'proof.log'
$stderr = Join-Path $Output 'proof-error.log'
function Quote-Literal([string]$Value) { return "'" + $Value.Replace("'", "''") + "'" }
$body = '& ' + (Quote-Literal $Python) + ' ' + (Quote-Literal $proof) + ' --driver ' + (Quote-Literal $Driver) + ' --output ' + (Quote-Literal $Output) + ' > ' + (Quote-Literal $stdout) + ' 2> ' + (Quote-Literal $stderr) + "`n"
$body += '$LASTEXITCODE | Set-Content ' + (Quote-Literal $exitFile)
Set-Content -Encoding UTF8 $job $body
$name = 'BearlyDriverProof-' + [guid]::NewGuid().ToString('N')
$identity = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -ExecutionPolicy Bypass -File "' + $job + '"')
$principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Limited
try {
  Register-ScheduledTask -TaskName $name -Action $action -Principal $principal -Force | Out-Null
  Start-ScheduledTask -TaskName $name
  $deadline = [datetime]::UtcNow.AddMinutes(5)
  while (!(Test-Path $exitFile) -and [datetime]::UtcNow -lt $deadline) { Start-Sleep -Seconds 2 }
  if (Test-Path $stdout) { Get-Content $stdout }
  if (Test-Path $stderr) { Get-Content $stderr }
  if (!(Test-Path $exitFile)) { throw 'Medium integrity proof did not finish' }
  if ([int](Get-Content $exitFile -Raw) -ne 0) { throw 'Medium integrity proof failed; see evidence logs' }
} finally {
  Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
  Unregister-ScheduledTask -TaskName $name -Confirm:$false -ErrorAction SilentlyContinue
}
