BeforeAll {
    $repository = (Resolve-Path (Join-Path $PSScriptRoot '../..')).ProviderPath
}

Describe 'Production Windows package gates' {
    It 'rejects altered, aliased, malformed, and oversized policy snapshots' {
        & (Join-Path $repository 'scripts/test-windows-component-policy.ps1')
    }

    It 'binds every staged payload member and build input for both profiles' {
        & (Join-Path $repository 'scripts/test-windows-probe-receipt.ps1') -Profile x86_64
        & (Join-Path $repository 'scripts/test-windows-probe-receipt.ps1') -Profile aarch64
    }

    It 'validates installer manifests before extraction and final gates' {
        & (Join-Path $repository 'scripts/test-windows-installer-policy.ps1')
    }

    It 'preserves manifest identity under generated positive and negative cases' {
        & (Join-Path $repository 'scripts/test-adversarial-installer.ps1')
    }
}
