# Static Inno payload inspection. This script is dot-sourced by the build helper.
# The inspector is a separately installed build tool; the installer is never run.

function Get-InnoInspectorPins {
    return @{
        Uri = 'https://github.com/UserUnknownFactor/innoextract_win/releases/download/670/innoextract670.zip'
        Archive = '79b69b9b1fcd98f42ccd4b245efdf6a03bcfb674ba6af482f5a46891c9ed4d14'
        Files = @{
            'innoextract.exe' = '0dab51583e403970065db49f87a6a2bf683ab6ebfdf27bede2bf633dd232a1a2'
            'libbz2-1.dll' = 'b79cd3a7102e359ef6324e98966a67939e58ec2b651ae2847cae84a4dfa453f7'
        }
    }
}

function Get-ValidatedInnoInspector {
    param([string]$Directory = $env:BALUN_INNOEXTRACT_DIR)
    if ([string]::IsNullOrWhiteSpace($Directory) -or
        -not [System.IO.Path]::IsPathFullyQualified($Directory)) {
        throw 'Set BALUN_INNOEXTRACT_DIR to the pinned inspector directory; see build-aux/inno/install-inspector.ps1.'
    }
    Assert-WindowsBundleRootIsNotReparsePoint $Directory
    $pins = Get-InnoInspectorPins
    $members = @(Get-ChildItem -LiteralPath $Directory -Force)
    if ($members.Count -ne $pins.Files.Count) {
        throw 'The Inno inspector directory must contain exactly the two pinned tool files.'
    }
    foreach ($member in $members) {
        if (-not $pins.Files.ContainsKey($member.Name) -or
            (Get-WindowsProbeSha256 $member.FullName) -cne $pins.Files[$member.Name]) {
            throw 'The Inno inspector or its native dependency differs from the reviewed pin.'
        }
    }
    return (Join-Path ([System.IO.Path]::GetFullPath($Directory)) 'innoextract.exe')
}

function ConvertTo-InnoPayloadPath {
    param([string]$RawPath)
    if (-not $RawPath.StartsWith('{app}\', [StringComparison]::Ordinal)) {
        throw 'Installer member is outside the application payload.'
    }
    $relative = $RawPath.Substring(6).Replace('\', '/')
    $parts = $relative.Split('/')
    if ($relative.Length -gt 1024 -or $parts.Count -gt 64 -or
        $relative -match '[^\x20-\x7e]|[:<>"|?*{}]') {
        throw 'Installer member has an unsupported, unsafe, or overlong path.'
    }
    foreach ($part in $parts) {
        if (-not $part -or $part -in @('.', '..') -or $part -match '[. ]$' -or
            $part -match '^(?i:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)') {
            throw 'Installer member has an escaping, empty, ambiguous, or reserved path component.'
        }
    }
    return $relative
}

function ConvertFrom-InnoPayloadListing {
    param([string[]]$Lines)
    if ($Lines.Count -eq 0 -or $Lines.Count -gt 65536) {
        throw 'Installer listing is empty or exceeds the member limit.'
    }
    $records = [System.Collections.Generic.Dictionary[string, string]]::new([StringComparer]::Ordinal)
    $declared = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $spellings = [System.Collections.Generic.Dictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
    $encoding = [System.Text.UTF8Encoding]::new($false, $true)
    $totalBytes = 0L
    $listingBytes = 0L
    $manifestBytes = 0L
    foreach ($line in $Lines) {
        $listingBytes += $encoding.GetByteCount($line) + 1
        if ($listingBytes -gt 16MB -or $line.Length -gt 1200) {
            throw 'Installer listing exceeds the bounded text budget.'
        }
        $isDirectory = $false
        if ($line -cmatch '^([0-9]{1,10}) SHA-256 ([0-9a-f]{64}) (.+)$') {
            $size = [long]$Matches[1]
            $hash = $Matches[2]
            $raw = $Matches[3]
            if ($size -gt 1GB) { throw 'Installer file exceeds 1 GiB.' }
            $totalBytes += $size
            if ($totalBytes -gt 4GB) { throw 'Installer payload exceeds 4 GiB.' }
        }
        elseif ($line.EndsWith('/') -or $line.EndsWith('\')) {
            $raw = $line.Substring(0, $line.Length - 1)
            $isDirectory = $true
        }
        else { throw 'Installer listing has an unknown record or lacks a SHA-256 checksum.' }
        $relative = ConvertTo-InnoPayloadPath $raw
        if (-not $declared.Add($relative)) { throw 'Installer has duplicate or colliding members.' }
        $parts = $relative.Split('/')
        for ($index = 1; $index -le $parts.Count; $index++) {
            $path = $parts[0..($index - 1)] -join '/'
            if ($spellings.ContainsKey($path) -and $spellings[$path] -cne $path) {
                throw 'Installer has case-colliding parent paths.'
            }
            $spellings[$path] = $path
            $pathKey = [Convert]::ToBase64String($encoding.GetBytes($path))
            $record = if ($index -lt $parts.Count -or $isDirectory) {
                "D`t$pathKey"
            } else { "F`t$pathKey`t$size`t$hash" }
            if ($records.ContainsKey($path) -and $records[$path] -cne $record) {
                throw 'Installer uses the same path as both a file and a directory.'
            }
            if (-not $records.ContainsKey($path)) {
                $manifestBytes += $encoding.GetByteCount($record) + 1
                if ($manifestBytes -gt 16MB) { throw 'Installer manifest exceeds 16 MiB.' }
            }
            $records[$path] = $record
            if ($records.Count -gt 65536) { throw 'Installer exceeds the total member limit.' }
        }
    }
    return ,$records
}

function Assert-InnoPayloadManifest {
    param($Expected, $Actual)
    if ($Expected.Count -ne $Actual.Count) { throw 'Installer payload membership differs from staging.' }
    foreach ($path in $Expected.Keys) {
        if (-not $Actual.ContainsKey($path) -or $Expected[$path] -cne $Actual[$path]) {
            throw 'Installer payload paths, types, sizes, or hashes differ from staging.'
        }
    }
}

function Invoke-InnoPayloadInspector {
    param([string]$Inspector, [string]$Installer, [string]$OutputDirectory, [switch]$Extract)
    foreach ($path in @($Inspector, $Installer, $OutputDirectory)) {
        if ([string]::IsNullOrWhiteSpace($path)) { continue }
        if (-not [System.IO.Path]::IsPathFullyQualified($path) -or $path -match '[\x00-\x1f\x7f"]') {
            throw 'Installer inspector paths must be absolute and contain no quotes or controls.'
        }
    }
    $arguments = '--silent --color=0 --progress=0 --collisions error --no-extract-unknown '
    if ($Extract) {
        if ([string]::IsNullOrWhiteSpace($OutputDirectory)) { throw 'Missing installer extraction directory.' }
        $arguments += '--extract --output-dir "' + $OutputDirectory + '" '
    }
    else { $arguments += '--list --list-sizes --list-checksums --dump ' }
    $arguments += '"' + $Installer + '"'
    return @(Invoke-BoundedInspector -Inspector $Inspector -Arguments $arguments `
        -Label 'Inno payload' -TokenPrefix 'inno' -ClosureClock $null `
        -ClosureDeadlineMs 300000 -ProcessDeadlineMs 300000 -OutputByteLimit 16777216 -RejectStandardError)
}

function Assert-WindowsInstallerPayload {
    param(
        [string]$Installer,
        [string]$Distribution,
        [string]$Inspector,
        [string]$PeInspector,
        [string]$ExpectedVersion,
        $ExpectedManifest
    )
    # Hold the Windows file against writes, deletion, and pathname replacement
    # while the inspector reopens it for listing and extraction.
    $installerHash = Get-WindowsProbeSha256 $Installer
    $held = [System.IO.File]::Open($Installer, 'Open', 'Read', 'Read')
    $temporaryBase = if ($env:TMPDIR) { $env:TMPDIR } else { [System.IO.Path]::GetTempPath() }
    $temporary = Join-Path $temporaryBase ('balun-inno-' + [Guid]::NewGuid().ToString('N'))
    $validationFailed = $false
    try {
        if ((Get-WindowsProbeSha256 $Installer) -cne $installerHash) {
            throw 'Installer changed before inspection.'
        }
        Assert-InnoPayloadManifest $ExpectedManifest.Records (Get-WindowsProbeTreeDigest $Distribution -IncludeRecords).Records
        $listing = @(Invoke-InnoPayloadInspector $Inspector $Installer)
        $listedManifest = ConvertFrom-InnoPayloadListing $listing
        Assert-InnoPayloadManifest $ExpectedManifest.Records $listedManifest
        # Only a completely validated listing can cause extraction. The tool's
        # normal filename expansion stays enabled; --dump is listing-only.
        $null = New-Item -ItemType Directory -Path $temporary -ErrorAction Stop
        $null = Invoke-InnoPayloadInspector $Inspector $Installer $temporary -Extract
        $top = @(Get-ChildItem -LiteralPath $temporary -Force)
        if ($top.Count -ne 1 -or $top[0].Name -cne 'app') {
            throw 'Installer extraction produced members outside the application tree.'
        }
        $payload = Join-Path $temporary 'app'
        $extracted = Get-WindowsProbeTreeDigest $payload -IncludeRecords
        Assert-InnoPayloadManifest $ExpectedManifest.Records $extracted.Records
        Assert-WindowsPackageFinalGates $payload $PeInspector $ExpectedVersion
        Invoke-PackagedRuntimeProbe $payload
        if ((Get-WindowsProbeTreeDigest $payload) -cne $ExpectedManifest.Digest -or
            (Get-WindowsProbeTreeDigest $Distribution) -cne $ExpectedManifest.Digest -or
            (Get-WindowsProbeSha256 $Installer) -cne $installerHash) {
            throw 'Installer or payload changed during final inspection.'
        }
        Assert-WindowsProbeReceipt $Distribution
    }
    catch {
        $validationFailed = $true
        throw
    }
    finally {
        $held.Dispose()
        try {
            if (Test-Path -LiteralPath $temporary) {
                Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction Stop
            }
        }
        catch {
            if (-not $validationFailed) { throw }
            Write-Warning "Installer validation failed and its scratch payload could not be removed: $temporary" `
                -WarningAction Continue
        }
    }
}
