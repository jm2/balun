#requires -Version 7.2
<# Install the fixed upstream compiler into an explicit CI scratch directory. #>
param([Parameter(Mandatory = $true)][string]$Destination)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'The Inno Setup compiler requires Windows.' }
if (-not [System.IO.Path]::IsPathFullyQualified($Destination) -or
    $Destination.Contains('"') -or (Test-Path -LiteralPath $Destination)) {
    throw 'Compiler destination must be an absolute, new directory without quotes.'
}
$parent = Split-Path -Parent $Destination
if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
    throw 'Compiler destination parent must already exist.'
}
$download = Join-Path $parent ('balun-inno-compiler-' + [Guid]::NewGuid().ToString('N') + '.exe')
$process = $null
try {
    & curl.exe --fail --location --silent --show-error --max-time 60 --max-filesize 16777216 `
        --output $download 'https://github.com/jrsoftware/issrc/releases/download/is-6_7_3/innosetup-6.7.3.exe'
    if ($LASTEXITCODE -ne 0) { throw 'Pinned Inno Setup compiler download failed.' }
    if ((Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash -ine
        '9c73c3bae7ed48d44112a0f48e66742c00090bdb5bef71d9d3c056c66e97b732') {
        throw 'Inno Setup compiler checksum mismatch.'
    }
    $arguments = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-',
        '/CURRENTUSER', '/NOICONS', '/NOCLOSEAPPLICATIONS', ('/DIR="' + $Destination + '"'))
    $process = Start-Process -FilePath $download -ArgumentList $arguments -PassThru
    if (-not $process.WaitForExit(120000)) { throw 'Inno Setup compiler installation timed out.' }
    if ($process.ExitCode -ne 0) { throw 'Inno Setup compiler installation failed.' }
    # The verified installer digest establishes the selected compiler release.
    # ISCC's checked-in version resource has placeholder 0.0.0.0 fields; the
    # actual compiler engine lives in ISCmplr.dll and reports its version when
    # compiling. PE ProductVersion is not an admission oracle for this tool.
    foreach ($name in @('ISCC.exe', 'ISCmplr.dll')) {
        $member = Get-Item -LiteralPath (Join-Path $Destination $name) -Force -ErrorAction Stop
        if ($member.PSIsContainer -or $member.Length -le 0 -or
            (($member.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
            throw 'Pinned Inno Setup compiler installation is incomplete or aliased.'
        }
    }
    Write-Host "Pinned Inno Setup 6.7.3 compiler installed in $Destination"
}
finally {
    if ($process) {
        if (-not $process.HasExited) { $process.Kill($true); $null = $process.WaitForExit(5000) }
        $process.Dispose()
    }
    Remove-Item -LiteralPath $download -Force -ErrorAction SilentlyContinue
}
