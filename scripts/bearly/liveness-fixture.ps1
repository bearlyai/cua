param([string]$Source, [string]$Folder, [string]$Name, [int]$Left, [int]$Top, [int]$Width, [int]$Height, [int]$Red, [int]$Green, [int]$Blue)
# Compiles liveness-fixture.cs and runs one fixture window. Compile or start-up
# errors go to <Name>-errors.log, because this process has no console.
$ErrorActionPreference = 'Stop'
try {
    $refs = @('System.Windows.Forms', 'System.Drawing', 'UIAutomationProvider', 'UIAutomationTypes', 'WindowsBase') |
        ForEach-Object { [Reflection.Assembly]::LoadWithPartialName($_).Location }
    Add-Type -TypeDefinition (Get-Content -Raw $Source) -ReferencedAssemblies $refs -Language CSharp -IgnoreWarnings
    [BearlyLiveness.Fixture]::Run($Folder, $Name, $Left, $Top, $Width, $Height, $Red, $Green, $Blue)
} catch {
    Add-Content (Join-Path $Folder "$Name-errors.log") ($_ | Out-String)
    exit 2
}
