#requires -Version 7.2
<# Install the fixed build-only inspection tool into a new, explicitly supplied directory. #>
param([Parameter(Mandatory = $true)][string]$Destination)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../../scripts/windows-installer-policy.ps1')
$pins = Get-InnoInspectorPins
if (-not [System.IO.Path]::IsPathRooted($Destination) -or (Test-Path -LiteralPath $Destination)) {
    throw 'Inspector destination must be an absolute, new directory.'
}
$client = [System.Net.Http.HttpClient]::new()
$deadline = [System.Threading.CancellationTokenSource]::new(60000)
$buffered = [System.IO.MemoryStream]::new()
$response = $null
$stream = $null
$archive = $null
try {
    $response = $client.GetAsync($pins.Uri, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead,
        $deadline.Token).GetAwaiter().GetResult()
    $null = $response.EnsureSuccessStatusCode()
    $stream = $response.Content.ReadAsStreamAsync($deadline.Token).GetAwaiter().GetResult()
    $buffer = [byte[]]::new(65536)
    while (($read = $stream.ReadAsync($buffer, 0, $buffer.Length, $deadline.Token).GetAwaiter().GetResult()) -gt 0) {
        if ($buffered.Length + $read -gt 2MB) { throw 'Inspector download exceeds 2 MiB.' }
        $buffered.Write($buffer, 0, $read)
    }
    $bytes = $buffered.ToArray()
    $hash = [Convert]::ToHexString([System.Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
    if ($hash -cne $pins.Archive) { throw 'Inspector archive checksum mismatch.' }
    $buffered.Position = 0
    $archive = [System.IO.Compression.ZipArchive]::new($buffered, 'Read', $true)
    if ($archive.Entries.Count -ne $pins.Files.Count) { throw 'Unexpected inspector archive members.' }
    $contents = @{}
    foreach ($entry in $archive.Entries) {
        if (-not $pins.Files.ContainsKey($entry.FullName) -or $contents.ContainsKey($entry.FullName) -or
            $entry.Length -gt 8MB -or (($entry.ExternalAttributes -shr 16) -band 0xF000) -notin @(0, 0x8000)) {
            throw 'Unexpected inspector archive member name, type, or size.'
        }
        $member = $entry.Open()
        $output = [System.IO.MemoryStream]::new()
        try {
            while (($read = $member.Read($buffer, 0, $buffer.Length)) -gt 0) {
                if ($output.Length + $read -gt $entry.Length) { throw 'Inspector member exceeds its declared size.' }
                $output.Write($buffer, 0, $read)
            }
            $data = $output.ToArray()
            $hash = [Convert]::ToHexString([System.Security.Cryptography.SHA256]::HashData($data)).ToLowerInvariant()
            if ($data.Length -ne $entry.Length -or $hash -cne $pins.Files[$entry.FullName]) {
                throw 'Inspector member checksum mismatch.'
            }
            $contents[$entry.FullName] = $data
        }
        finally { $member.Dispose(); $output.Dispose() }
    }
    $null = New-Item -ItemType Directory -Path $Destination -ErrorAction Stop
    foreach ($name in $contents.Keys) {
        [System.IO.File]::WriteAllBytes((Join-Path $Destination $name), $contents[$name])
    }
    Write-Host "Pinned build-only inspector installed in $Destination"
}
finally {
    if ($archive) { $archive.Dispose() }
    if ($stream) { $stream.Dispose() }
    if ($response) { $response.Dispose() }
    $buffered.Dispose(); $deadline.Dispose(); $client.Dispose()
}
