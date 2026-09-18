<# Generated installer manifest properties. No installer or extracted payload executes. #>
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-installer-policy.ps1')

function Get-TestNumber {
    param([string]$Name, [long]$Default, [long]$Maximum)
    $text = [Environment]::GetEnvironmentVariable($Name)
    $value = if ($null -eq $text) { $Default } else { [long]::Parse($text) }
    if ($value -lt 0 -or $value -gt $Maximum) { throw "$Name exceeds the test budget." }
    return $value
}
function Assert-Rejected {
    param([scriptblock]$Action)
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw 'Invalid synthetic manifest was accepted.' }
}
$seed = Get-TestNumber 'BALUN_ADVERSARIAL_SEED' 20260917 ([long]::MaxValue)
$cases = Get-TestNumber 'BALUN_ADVERSARIAL_CASES' 128 100000
$first = Get-TestNumber 'BALUN_ADVERSARIAL_START' 0 100000
if (-not $cases -or $first + $cases -gt 100000) { throw 'Invalid adversarial case range.' }
$encoding = [System.Text.UTF8Encoding]::new($false, $true)
for ($case = $first; $case -lt $first + $cases; $case++) {
    try {
        # Each case is independently replayable; constrain the .NET Random seed.
        $random = [Random]::new([int](($seed -bxor ($case * 104729)) % [int]::MaxValue))
        $expected = [System.Collections.Generic.Dictionary[string, string]]::new([StringComparer]::Ordinal)
        $lines = [System.Collections.Generic.List[string]]::new()
        $count = $random.Next(1, 17)
        foreach ($directory in @('bin', 'empty')) {
            $key = [Convert]::ToBase64String($encoding.GetBytes($directory))
            $expected[$directory] = "D`t$key"
            $lines.Add("{app}\$directory/")
        }
        for ($index = 0; $index -lt $count; $index++) {
            $name = "bin/fixture-$index-$($random.Next()).dll"
            $key = [Convert]::ToBase64String($encoding.GetBytes($name))
            $bytes = [byte[]]::new(32)
            $random.NextBytes($bytes)
            $hash = [Convert]::ToHexString($bytes).ToLowerInvariant()
            $size = $random.Next(0, 1048576)
            $expected[$name] = "F`t$key`t$size`t$hash"
            $lines.Add("$size SHA-256 $hash {app}\$($name.Replace('/', '\'))")
        }
        for ($index = $lines.Count - 1; $index -gt 0; $index--) {
            $other = $random.Next($index + 1)
            $saved = $lines[$index]; $lines[$index] = $lines[$other]; $lines[$other] = $saved
        }
        $parsed = ConvertFrom-InnoPayloadListing $lines.ToArray()
        Assert-InnoPayloadManifest $expected $parsed
        # Order is irrelevant; membership, casing, type, size and bytes are not.
        $reversed = $lines.ToArray(); [Array]::Reverse($reversed)
        Assert-InnoPayloadManifest $parsed (ConvertFrom-InnoPayloadListing $reversed)
        Assert-Rejected { ConvertFrom-InnoPayloadListing ($lines.ToArray() + $lines[0]) }
        $fileIndex = 0
        while ($lines[$fileIndex].StartsWith('{app}')) { $fileIndex++ }
        $missing = $lines.ToArray() | Where-Object { $_ -cne $lines[$fileIndex] }
        Assert-Rejected {
            Assert-InnoPayloadManifest $expected (ConvertFrom-InnoPayloadListing $missing)
        }
        $badParts = @('..', '.', 'NUL.txt', 'COM1', 'trailing.', 'trailing ', 'bad:stream', '')
        $part = $badParts[$random.Next($badParts.Count)]
        Assert-Rejected {
            ConvertFrom-InnoPayloadListing @("1 SHA-256 $hash {app}\bin\$part\fixture")
        }
        Assert-Rejected {
            ConvertFrom-InnoPayloadListing ($lines.ToArray() + "1 SHA-256 $hash {app}\BIN\extra")
        }
        foreach ($mutation in @('size', 'hash', 'type')) {
            $modified = $lines.ToArray()
            $fields = $modified[$fileIndex].Split(' ', 4)
            $changedHash = $(if ($fields[2][0] -eq '0') { '1' } else { '0' }) + $fields[2].Substring(1)
            $modified[$fileIndex] = switch ($mutation) {
                'size' { "$([long]$fields[0] + 1) SHA-256 $($fields[2]) $($fields[3])" }
                'hash' { "$($fields[0]) SHA-256 $changedHash $($fields[3])" }
                'type' { "$($fields[3])/" }
            }
            Assert-Rejected { Assert-InnoPayloadManifest $expected (ConvertFrom-InnoPayloadListing $modified) }
        }
    }
    catch {
        $replay = "suite=installer-manifests`nBALUN_ADVERSARIAL_SEED=$seed`nBALUN_ADVERSARIAL_START=$case`nBALUN_ADVERSARIAL_CASES=1`n"
        [Console]::Error.WriteLine($replay)
        if ($env:BALUN_ADVERSARIAL_FAILURE_DIR) {
            $null = New-Item -ItemType Directory -Path $env:BALUN_ADVERSARIAL_FAILURE_DIR -Force
            [System.IO.File]::WriteAllText(
                (Join-Path $env:BALUN_ADVERSARIAL_FAILURE_DIR "installer-manifests-$seed-$case.txt"), $replay)
        }
        throw
    }
}
Write-Host "adversarial installer-manifests: seed=$seed, start=$first, cases=$cases"
