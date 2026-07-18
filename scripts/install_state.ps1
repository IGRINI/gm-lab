function Get-GmlFileSha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Test-GmlSha256([string]$Value) {
    return -not [string]::IsNullOrWhiteSpace($Value) -and $Value -match '^[0-9a-fA-F]{64}$'
}

function Get-GmlReleaseExecutablePath([string]$RepoRoot) {
    $executableName = if ($env:OS -eq "Windows_NT") { "gml-app.exe" } else { "gml-app" }
    return Join-Path $RepoRoot "target\release\$executableName"
}

function Get-GmlRelativeRepoPath([string]$RepoRoot, [string]$Path) {
    $root = ([System.IO.Path]::GetFullPath($RepoRoot) -replace '[\\/]+$', '') + [System.IO.Path]::DirectorySeparatorChar
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    if (-not $fullPath.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Build fingerprint path is outside the repository: $fullPath"
    }
    return $fullPath.Substring($root.Length).Replace('\', '/')
}

function Get-GmlFileSetFingerprint([string]$RepoRoot, [object[]]$Files) {
    $records = New-Object System.Collections.Generic.List[string]
    foreach ($file in @($Files | Sort-Object -Property FullName -Unique)) {
        if ($null -eq $file -or -not $file.Exists) { continue }
        $relative = Get-GmlRelativeRepoPath $RepoRoot $file.FullName
        $records.Add("$relative`n$($file.Length)`n$(Get-GmlFileSha256 $file.FullName)`n")
    }
    if ($records.Count -eq 0) {
        throw "No files were found for the build fingerprint."
    }
    $payload = [System.Text.Encoding]::UTF8.GetBytes(($records -join ''))
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha256.ComputeHash($payload))).Replace('-', '').ToLowerInvariant()
    } finally {
        $sha256.Dispose()
    }
}

function Get-GmlSourceFingerprint([string]$RepoRoot) {
    $files = New-Object System.Collections.Generic.List[object]
    foreach ($relative in @(
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "setup.ps1",
            "setup.cmd",
            "run.ps1",
            "run.cmd",
            "scripts\install_state.ps1",
            "web\index.html",
            "web\package.json",
            "web\package-lock.json",
            "web\vite.config.js",
            "sidecar\install_models.py",
            "sidecar\models.json",
            "sidecar\requirements-runtime.lock",
            "sidecar\requirements-image.lock",
            "sidecar\serve.py"
        )) {
        $candidate = Join-Path $RepoRoot $relative
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            $files.Add((Get-Item -LiteralPath $candidate))
        }
    }
    foreach ($relative in @("crates", "web\src", "web\public")) {
        $directory = Join-Path $RepoRoot $relative
        if (Test-Path -LiteralPath $directory -PathType Container) {
            foreach ($file in Get-ChildItem -LiteralPath $directory -File -Recurse) {
                $files.Add($file)
            }
        }
    }
    return Get-GmlFileSetFingerprint $RepoRoot $files.ToArray()
}

function Get-GmlTreeFingerprint([string]$RepoRoot, [string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "Build output directory was not found: $Path"
    }
    return Get-GmlFileSetFingerprint $RepoRoot @(Get-ChildItem -LiteralPath $Path -File -Recurse)
}

function Assert-GmlReleaseState(
    [string]$RepoRoot,
    [string]$InferenceRoot,
    [string]$ExpectedProfile = ""
) {
    $statePath = Join-Path $InferenceRoot "install.json"
    if (-not (Test-Path -LiteralPath $statePath -PathType Leaf)) {
        throw "No completed TaleShift installation was found. Run setup.cmd."
    }
    try {
        $state = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
    } catch {
        throw "The installation state is invalid: $statePath. Rerun setup.cmd."
    }
    if ($state.schema_version -ne 2 -or
        (-not [string]::IsNullOrWhiteSpace($ExpectedProfile) -and [string]$state.profile -ne $ExpectedProfile)) {
        throw "The installation state is outdated or belongs to another profile. Rerun setup.cmd."
    }
    if ($state.build_complete -ne $true) {
        throw "The TaleShift application build is incomplete. Rerun setup.cmd without -SkipBuild."
    }

    $executable = Get-GmlReleaseExecutablePath $RepoRoot
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "The TaleShift executable was not found: $executable. Rerun setup.cmd."
    }
    $webDist = Join-Path $RepoRoot "web\dist"
    $expectedSource = [string]$state.source_fingerprint
    $expectedExecutable = [string]$state.executable_sha256
    $expectedWebDist = [string]$state.web_dist_fingerprint
    if (-not (Test-GmlSha256 $expectedSource) -or
        -not (Test-GmlSha256 $expectedExecutable) -or
        -not (Test-GmlSha256 $expectedWebDist) -or
        $expectedSource -ne (Get-GmlSourceFingerprint $RepoRoot) -or
        $expectedExecutable -ne (Get-GmlFileSha256 $executable) -or
        $expectedWebDist -ne (Get-GmlTreeFingerprint $RepoRoot $webDist)) {
        throw "The release build does not match the current source tree. Rerun setup.cmd."
    }
    return $state
}
