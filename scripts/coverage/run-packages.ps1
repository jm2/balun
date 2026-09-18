param([Parameter(Mandatory)][string]$PesterModule, [Parameter(Mandatory)][string]$OutputPath)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module $PesterModule -RequiredVersion 6.2.0 -ErrorAction Stop
$config = New-PesterConfiguration
$config.Run.Path = Join-Path $PSScriptRoot 'PackageGates.Tests.ps1'
$config.Run.PassThru = $true
$config.CodeCoverage.Enabled = $true
$config.CodeCoverage.Path = @(
    (Join-Path $PSScriptRoot '../build-windows.ps1'),
    (Join-Path $PSScriptRoot '../windows-installer-policy.ps1'))
# The shared summarizer enforces per-gate floors; the whole build helper also
# includes native build orchestration intentionally outside this portable run.
$config.CodeCoverage.CoveragePercentTarget = 0
$config.CodeCoverage.OutputFormat = 'Cobertura'
$config.CodeCoverage.OutputPath = $OutputPath
$config.Output.Verbosity = 'Normal'
$result = Invoke-Pester -Configuration $config
if ($result.Result -ne 'Passed' -or $result.PassedCount -lt 4) {
    throw 'Package coverage requires all four gate suites to pass.'
}
