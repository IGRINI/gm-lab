[CmdletBinding()]
param(
    [ValidateSet("Minimal", "Rag", "Voice", "Images", "Full")]
    [string]$Profile,
    [string]$InferenceHome,
    [switch]$SkipBuild,
    [switch]$VerifyOnly,
    [switch]$NonInteractive,
    [switch]$AcceptRestrictedModelLicenses
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$RepoRoot = $PSScriptRoot
. (Join-Path $RepoRoot "scripts\install_state.ps1")
$RuntimePythonVersion = "3.12.11"
$ImagePythonVersion = "3.14.4"
$RustVersion = "1.92.0"
$UvVersion = "0.9.13"
$UvInstallerUrl = "https://github.com/astral-sh/uv/releases/download/$UvVersion/uv-installer.ps1"
$UvInstallerSha256 = "799852b8bf24a1911c3160fe8bfa1adbacabf3c995cc7ff3c67b76cf6af49435"
$ComfyRepository = "https://github.com/comfyanonymous/ComfyUI.git"
$ComfyRevision = "1a510f04234e5a213d3985a1a54f65652623f4bc"
$ManagedEnvStart = "# BEGIN GM-LAB SETUP"
$ManagedEnvEnd = "# END GM-LAB SETUP"

function Write-Step([string]$Message) {
    Write-Host "`n==> $Message" -ForegroundColor Cyan
}

function Get-RequiredCommand([string]$Name, [string]$Help) {
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($null -eq $command) {
        throw "Command '$Name' was not found. $Help"
    }
    return $command.Source
}

function Invoke-Checked([string]$FilePath, [string[]]$Arguments) {
    & $FilePath @Arguments | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code $LASTEXITCODE`: $FilePath $($Arguments -join ' ')"
    }
}

function Write-Utf8FileAtomically([string]$Path, [string]$Contents) {
    $parent = Split-Path $Path -Parent
    if (-not [string]::IsNullOrWhiteSpace($parent)) {
        New-Item -ItemType Directory -Force -Path $parent | Out-Null
    }
    $temporary = "$Path.tmp-$([Guid]::NewGuid().ToString('N'))"
    $backup = "$Path.bak-$([Guid]::NewGuid().ToString('N'))"
    try {
        [System.IO.File]::WriteAllText($temporary, $Contents, [System.Text.UTF8Encoding]::new($false))
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            [System.IO.File]::Replace($temporary, $Path, $backup)
        } else {
            [System.IO.File]::Move($temporary, $Path)
        }
    } finally {
        Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
    }
}

function Get-PythonVersion([string]$Python) {
    $version = (& $Python -c "import platform; print(platform.python_version())").Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($version)) {
        throw "Cannot determine the Python version in $Python"
    }
    return $version
}

function Get-PackageInventory([string]$Uv, [string]$Python) {
    $packages = @(& $Uv "pip" "freeze" "--python" $Python)
    if ($LASTEXITCODE -ne 0) {
        throw "Cannot inspect the Python environment: $Python"
    }
    return @($packages | ForEach-Object { $_.Trim() } | Where-Object { $_ } | Sort-Object)
}

function Write-EnvironmentMarker(
    [string]$Uv,
    [string]$Python,
    [string]$Venv,
    [string]$Requirements
) {
    $state = [ordered]@{
        schema_version = 1
        python_version = Get-PythonVersion $Python
        requirements_sha256 = Get-GmlFileSha256 $Requirements
        packages = @(Get-PackageInventory $Uv $Python)
    } | ConvertTo-Json -Depth 4
    Write-Utf8FileAtomically (Join-Path $Venv ".gml-environment.json") ($state + "`n")
}

function Assert-LockedEnvironment(
    [string]$Uv,
    [string]$Python,
    [string]$Venv,
    [string]$PythonVersion,
    [string]$Requirements
) {
    if (-not (Test-Path -LiteralPath $Python -PathType Leaf)) {
        throw "The Python environment was not found: $Python"
    }
    $actualPythonVersion = Get-PythonVersion $Python
    if ($actualPythonVersion -ne $PythonVersion) {
        throw "Python $PythonVersion is required in $Venv. Found $actualPythonVersion. Rerun setup without -VerifyOnly."
    }
    Invoke-Checked $Uv @("pip", "check", "--python", $Python)

    $markerPath = Join-Path $Venv ".gml-environment.json"
    if (-not (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
        throw "The environment lock marker is missing: $markerPath. Rerun setup without -VerifyOnly."
    }
    try {
        $marker = Get-Content -LiteralPath $markerPath -Raw | ConvertFrom-Json
    } catch {
        throw "The environment lock marker is invalid: $markerPath. Rerun setup without -VerifyOnly."
    }
    if ($marker.schema_version -ne 1 -or
        [string]$marker.python_version -ne $PythonVersion -or
        [string]$marker.requirements_sha256 -ne (Get-GmlFileSha256 $Requirements)) {
        throw "The Python environment does not match $Requirements. Rerun setup without -VerifyOnly."
    }
    $expectedPackages = @($marker.packages | ForEach-Object { [string]$_ } | Sort-Object)
    $actualPackages = @(Get-PackageInventory $Uv $Python)
    if (@(Compare-Object -ReferenceObject $expectedPackages -DifferenceObject $actualPackages).Count -ne 0) {
        throw "The Python environment package inventory has changed. Rerun setup without -VerifyOnly."
    }
}

function Assert-InstalledProfile([string]$InferenceRoot, [string]$SelectedProfile) {
    [void](Assert-GmlReleaseState $RepoRoot $InferenceRoot $SelectedProfile)
    Assert-ManagedEnv $InferenceRoot $SelectedProfile
}

function Resolve-Profile {
    if (-not [string]::IsNullOrWhiteSpace($Profile)) {
        $normalizedProfile = switch ($Profile.ToLowerInvariant()) {
            "minimal" { "Minimal" }
            "rag" { "Rag" }
            "voice" { "Voice" }
            "images" { "Images" }
            "full" { "Full" }
        }
        return $normalizedProfile
    }
    if ($NonInteractive) {
        throw "-Profile is required with -NonInteractive."
    }
    Write-Host "Select an installation profile:"
    Write-Host "  1. Minimal - application only (recommended for the first start)"
    Write-Host "  2. Rag     - local memory search; non-commercial reranker license"
    Write-Host "  3. Voice   - Rag plus local speech recognition and text-to-speech"
    Write-Host "  4. Images  - Rag plus experimental local images (RTX 50)"
    Write-Host "  5. Full    - all local components (RTX 50)"
    $choice = Read-Host "Profile [1]"
    if ([string]::IsNullOrWhiteSpace($choice)) { $choice = "1" }
    $profiles = @{ "1" = "Minimal"; "2" = "Rag"; "3" = "Voice"; "4" = "Images"; "5" = "Full" }
    if (-not $profiles.ContainsKey($choice)) {
        throw "Unknown profile: $choice"
    }
    return $profiles[$choice]
}

function Resolve-InferenceRoot {
    if (-not [string]::IsNullOrWhiteSpace($InferenceHome)) {
        return [System.IO.Path]::GetFullPath($InferenceHome)
    }
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw "LOCALAPPDATA is unavailable. Pass -InferenceHome."
    }
    return Join-Path $env:LOCALAPPDATA "gm-lab\inference"
}

function Assert-Node([string]$Node) {
    $raw = (& $Node --version).Trim().TrimStart("v")
    $version = [version]($raw.Split("-")[0])
    $supported = (($version.Major -eq 20 -and $version -ge [version]"20.19.0") -or
        ($version.Major -ge 22 -and $version -ge [version]"22.12.0"))
    if (-not $supported) {
        throw "Node.js 20.19+ or 22.12+ is required. Found $version."
    }
}

function Assert-Rust([string]$Cargo) {
    $raw = (& $Cargo --version)
    if ($raw -notmatch 'cargo\s+(\d+\.\d+\.\d+)') {
        throw "Cannot determine the Cargo version: $raw"
    }
    if ([version]$matches[1] -lt [version]"1.85.0") {
        throw "Rust/Cargo 1.85+ is required. Found $($matches[1])."
    }
}

function Assert-WindowsBuildPrerequisites {
    if ($env:OS -ne "Windows_NT") { return }

    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
        throw "Visual Studio Build Tools 2022 with Desktop development with C++ is required."
    }
    $installation = @(& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
    if ($LASTEXITCODE -ne 0 -or $installation.Count -eq 0 -or [string]::IsNullOrWhiteSpace($installation[0])) {
        throw "Visual Studio Build Tools 2022 is missing the MSVC C++ toolchain."
    }

    $kits = Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots" -ErrorAction SilentlyContinue
    $kitsRoot = if ($null -ne $kits) { [string]$kits.KitsRoot10 } else { "" }
    $sdkFound = $false
    if (-not [string]::IsNullOrWhiteSpace($kitsRoot)) {
        $includeRoot = Join-Path $kitsRoot "Include"
        if (Test-Path -LiteralPath $includeRoot -PathType Container) {
            foreach ($directory in @(Get-ChildItem -LiteralPath $includeRoot -Directory -ErrorAction SilentlyContinue)) {
                if (Test-Path -LiteralPath (Join-Path $directory.FullName "um\Windows.h") -PathType Leaf) {
                    $sdkFound = $true
                    break
                }
            }
        }
    }
    if (-not $sdkFound) {
        throw "Windows 10/11 SDK is required. Add it through Visual Studio Installer."
    }

    $webViewClient = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
    $webViewFound = $false
    foreach ($key in @(
            "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$webViewClient",
            "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$webViewClient",
            "HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$webViewClient"
        )) {
        $client = Get-ItemProperty $key -ErrorAction SilentlyContinue
        if ($null -ne $client -and -not [string]::IsNullOrWhiteSpace([string]$client.pv)) {
            $webViewFound = $true
            break
        }
    }
    foreach ($directory in @(
            (Join-Path ${env:ProgramFiles(x86)} "Microsoft\EdgeWebView\Application"),
            (Join-Path $env:ProgramFiles "Microsoft\EdgeWebView\Application")
        )) {
        if (Test-Path -LiteralPath $directory -PathType Container) { $webViewFound = $true }
    }
    if (-not $webViewFound) {
        throw "Microsoft Edge WebView2 Runtime is required for the desktop application."
    }
}

function Assert-Nvidia([bool]$NeedsImages) {
    $nvidiaSmi = Get-RequiredCommand "nvidia-smi" "Local models require an NVIDIA GPU and a current driver."
    Invoke-Checked $nvidiaSmi @("--query-gpu=name,driver_version", "--format=csv,noheader")
    $capabilities = & $nvidiaSmi "--query-gpu=compute_cap" "--format=csv,noheader,nounits" 2>$null
    if ($LASTEXITCODE -ne 0 -or $null -eq $capabilities) {
        throw "Cannot query the NVIDIA GPU compute capability. Update the NVIDIA driver."
    }
    $compatible = $false
    $minimumCapability = if ($NeedsImages) { 10.0 } else { 8.0 }
    foreach ($capability in @($capabilities)) {
        $parsed = 0.0
        if ([double]::TryParse(
                $capability.Trim(),
                [System.Globalization.NumberStyles]::Float,
                [System.Globalization.CultureInfo]::InvariantCulture,
                [ref]$parsed) -and $parsed -ge $minimumCapability) {
            $compatible = $true
        }
    }
    if (-not $compatible) {
        if ($NeedsImages) {
            throw "The local Images profile requires NVIDIA Blackwell (RTX 50, compute capability 10+)."
        }
        throw "Local RAG and voice require an NVIDIA Ampere GPU or newer (compute capability 8+)."
    }
}

function Assert-TorchCuda([string]$Python, [bool]$NeedsImages) {
    $minimumMajor = if ($NeedsImages) { 10 } else { 8 }
    $probe = @'
import sys
import torch

if not torch.cuda.is_available():
    raise SystemExit("PyTorch cannot use CUDA. Update the NVIDIA driver and rerun setup.")
major, minor = torch.cuda.get_device_capability()
minimum = int(sys.argv[1])
if major < minimum:
    raise SystemExit(f"GPU compute capability {major}.{minor} is below the required {minimum}.0")
if minimum == 8 and not torch.cuda.is_bf16_supported():
    raise SystemExit("This GPU/driver combination does not support the required BF16 runtime.")
print(f"CUDA ready: {torch.cuda.get_device_name(0)} (compute {major}.{minor}, torch {torch.__version__})")
'@
    Invoke-Checked $Python @("-c", $probe, $minimumMajor.ToString([Globalization.CultureInfo]::InvariantCulture))
}

function Assert-FreeSpace([string]$InferenceRoot, [string]$SelectedProfile) {
    $manifest = Get-Content (Join-Path $RepoRoot "sidecar\models.json") -Raw | ConvertFrom-Json
    [int64]$missingBytes = 0
    foreach ($component in $manifest.components) {
        if ($component.profiles -notcontains $SelectedProfile) { continue }
        $destination = Join-Path $InferenceRoot $component.destination
        if ($component.kind -eq "hf_snapshot") {
            $marker = Join-Path $destination ".gml-model.json"
        } else {
            $marker = "$destination.gml-model.json"
        }
        if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
            $missingBytes += [int64]$component.estimated_bytes
        }
    }
    if ($SelectedProfile -ne "Minimal" -and -not (Test-Path (Join-Path $InferenceRoot "runtime\.venv"))) {
        $missingBytes += 8GB
    }
    if ($SelectedProfile -in @("Images", "Full") -and -not (Test-Path (Join-Path $InferenceRoot "image\.venv"))) {
        $missingBytes += 8GB
    }
    $missingBytes += 2GB

    $fullPath = [System.IO.Path]::GetFullPath($InferenceRoot)
    $driveRoot = [System.IO.Path]::GetPathRoot($fullPath)
    if ([string]::IsNullOrWhiteSpace($driveRoot)) { return }
    $drive = [System.IO.DriveInfo]::new($driveRoot)
    if ($drive.AvailableFreeSpace -lt $missingBytes) {
        $need = [math]::Ceiling($missingBytes / 1GB)
        $free = [math]::Floor($drive.AvailableFreeSpace / 1GB)
        throw "Not enough free space on $driveRoot`: approximately $need GiB required, $free GiB available."
    }
}

function Get-Uv {
    $existing = Get-Command "uv" -ErrorAction SilentlyContinue
    if ($null -ne $existing) {
        $installedVersion = [string](& $existing.Source --version)
        $installedVersion = $installedVersion.Trim()
        if ($LASTEXITCODE -eq 0 -and $installedVersion -match "^uv\s+$([regex]::Escape($UvVersion))(?:\s|$)") {
            return $existing.Source
        }
        if ($VerifyOnly) {
            throw "uv $UvVersion is required for verification. Found: $installedVersion"
        }
    }
    if ($VerifyOnly) {
        throw "uv was not found. Run a normal setup first."
    }

    Write-Step "Installing uv $UvVersion"
    $temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("gm-lab-uv-" + [System.IO.Path]::GetRandomFileName() + ".ps1")
    try {
        Invoke-WebRequest $UvInstallerUrl -UseBasicParsing -OutFile $temporary | Out-Null
        $actualInstallerSha256 = Get-GmlFileSha256 $temporary
        if ($actualInstallerSha256 -ne $UvInstallerSha256) {
            throw "The uv installer checksum is invalid. Expected $UvInstallerSha256, found $actualInstallerSha256."
        }
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $temporary | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "The uv installer failed with exit code $LASTEXITCODE." }
    } finally {
        Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
    }
    $candidates = @(
        (Join-Path $env:USERPROFILE ".local\bin\uv.exe"),
        (Join-Path $env:USERPROFILE ".cargo\bin\uv.exe"),
        (Join-Path $env:LOCALAPPDATA "Programs\uv\uv.exe")
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
    }
    $installed = Get-Command "uv" -ErrorAction SilentlyContinue
    if ($null -eq $installed) { throw "uv was installed but uv.exe was not found. Restart PowerShell." }
    return $installed.Source
}

function Ensure-Venv(
    [string]$Uv,
    [string]$PythonVersion,
    [string]$Venv,
    [string]$Requirements
) {
    $python = Join-Path $Venv "Scripts\python.exe"
    $create = -not (Test-Path -LiteralPath $python -PathType Leaf)
    $recreate = $false
    if (-not $create) {
        try {
            $recreate = (Get-PythonVersion $python) -ne $PythonVersion
        } catch {
            $recreate = $true
        }
    }
    if ($recreate) {
        Write-Host "Recreating $Venv with Python $PythonVersion"
        Invoke-Checked $Uv @("python", "install", $PythonVersion)
        Invoke-Checked $Uv @("venv", "--clear", "--python", $PythonVersion, $Venv)
    } elseif ($create) {
        Invoke-Checked $Uv @("python", "install", $PythonVersion)
        Invoke-Checked $Uv @("venv", "--python", $PythonVersion, $Venv)
    }
    Invoke-Checked $Uv @("pip", "sync", "--python", $python, $Requirements)
    Invoke-Checked $Uv @("pip", "check", "--python", $python)
    Write-EnvironmentMarker $Uv $python $Venv $Requirements
    return $python
}

function Ensure-ComfyUi([string]$Git, [string]$ComfyDir) {
    $gitDir = Join-Path $ComfyDir ".git"
    if (-not (Test-Path -LiteralPath $gitDir -PathType Container)) {
        if (Test-Path -LiteralPath $ComfyDir) {
            $items = @(Get-ChildItem -LiteralPath $ComfyDir -Force)
            if ($items.Count -gt 0) {
                throw "The ComfyUI directory exists but is not a Git checkout: $ComfyDir"
            }
        }
        New-Item -ItemType Directory -Force -Path (Split-Path $ComfyDir -Parent) | Out-Null
        Invoke-Checked $Git @("clone", "--filter=blob:none", "--no-checkout", $ComfyRepository, $ComfyDir)
    }
    $origin = (& $Git -C $ComfyDir "remote" "get-url" "origin" 2>$null).Trim()
    if ($LASTEXITCODE -ne 0 -or $origin.TrimEnd("/") -ne $ComfyRepository.TrimEnd("/")) {
        throw "The managed ComfyUI directory has an unexpected Git origin: $ComfyDir"
    }
    & $Git -C $ComfyDir "cat-file" "-e" "$ComfyRevision`^{commit}" 2>$null
    if ($LASTEXITCODE -ne 0) {
        Invoke-Checked $Git @("-C", $ComfyDir, "fetch", "--depth", "1", "origin", $ComfyRevision)
    }
    # This directory is owned by setup. A forced checkout repairs deleted or
    # modified tracked files while leaving untracked outputs untouched.
    Invoke-Checked $Git @("-C", $ComfyDir, "checkout", "--detach", "--force", $ComfyRevision)
    Assert-ComfyUi $Git $ComfyDir
}

function Assert-ComfyUi([string]$Git, [string]$ComfyDir) {
    if (-not (Test-Path -LiteralPath (Join-Path $ComfyDir ".git") -PathType Container)) {
        throw "The managed ComfyUI checkout was not found: $ComfyDir"
    }
    $current = (& $Git -C $ComfyDir "rev-parse" "HEAD" 2>$null).Trim()
    if ($LASTEXITCODE -ne 0 -or $current -ne $ComfyRevision) {
        throw "ComfyUI revision mismatch: expected $ComfyRevision, found $current"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $ComfyDir "main.py") -PathType Leaf)) {
        throw "The managed ComfyUI checkout is incomplete: main.py is missing. Rerun setup without -VerifyOnly."
    }
    $changes = @(& $Git -C $ComfyDir "status" "--porcelain" "--untracked-files=no")
    if ($LASTEXITCODE -ne 0) {
        throw "Cannot inspect the managed ComfyUI checkout: $ComfyDir"
    }
    if (-not [string]::IsNullOrWhiteSpace(($changes -join "`n"))) {
        throw "The managed ComfyUI tracked files do not match the pinned revision. Rerun setup without -VerifyOnly."
    }
}

function Confirm-RestrictedLicenses([string]$SelectedProfile) {
    if ($SelectedProfile -eq "Minimal" -or $AcceptRestrictedModelLicenses) { return }
    Write-Host ""
    Write-Warning "This profile uses jina-reranker-v3 under CC BY-NC 4.0 (non-commercial only)."
    if ($SelectedProfile -in @("Images", "Full")) {
        Write-Warning "The Comfy-Org source does not declare a separate license for the FP4 text encoder/VAE."
    }
    Write-Host "Details: THIRD_PARTY_NOTICES.md"
    if ($NonInteractive) {
        throw "Review the notices and pass -AcceptRestrictedModelLicenses."
    }
    $answer = Read-Host "Type ACCEPT to continue"
    if ($answer -cne "ACCEPT") { throw "Setup cancelled because the licenses were not accepted." }
}

function Get-ManagedEnvBlock(
    [string]$InferenceRoot,
    [string]$SelectedProfile,
    [bool]$BuildComplete = $true
) {
    $rag = if ($BuildComplete -and $SelectedProfile -ne "Minimal") { "true" } else { "false" }
    $stt = if ($BuildComplete -and $SelectedProfile -in @("Voice", "Full")) { "true" } else { "false" }
    $tts = if ($BuildComplete -and $SelectedProfile -in @("Voice", "Full")) { "true" } else { "false" }
    $imageProvider = if ($BuildComplete -and $SelectedProfile -in @("Images", "Full")) { "local" } else { "grok" }
    $normalizedRoot = $InferenceRoot.Replace("\", "/")
    $block = @"
$ManagedEnvStart
GM_INFERENCE_HOME="$normalizedRoot"
GM_RAG_ENABLED=$rag
GM_RAG_RERANK_ENABLED=$rag
GM_STT_ENABLED=$stt
GM_TTS_ENABLED=$tts
GM_IMAGE_ENABLED=true
GM_IMAGE_PROVIDER=$imageProvider
USE_FLASH=auto
$ManagedEnvEnd
"@
    return $block.TrimEnd()
}

function Assert-ManagedEnv([string]$InferenceRoot, [string]$SelectedProfile) {
    $envPath = Join-Path $RepoRoot ".env"
    if (-not (Test-Path -LiteralPath $envPath -PathType Leaf)) {
        throw "The managed .env configuration is missing. Rerun setup without -VerifyOnly."
    }
    $body = [System.IO.File]::ReadAllText($envPath, [System.Text.Encoding]::UTF8)
    $pattern = "(?ms)^$([regex]::Escape($ManagedEnvStart))\r?\n.*?^$([regex]::Escape($ManagedEnvEnd))"
    $match = [regex]::Match($body, $pattern)
    $expected = (Get-ManagedEnvBlock $InferenceRoot $SelectedProfile) -replace "\r\n?", "`n"
    $actual = if ($match.Success) { $match.Value -replace "\r\n?", "`n" } else { "" }
    if (-not $match.Success -or $actual.TrimEnd() -ne $expected.TrimEnd()) {
        throw "The managed .env configuration does not match profile $SelectedProfile. Rerun setup without -VerifyOnly."
    }
}

function Set-ManagedEnv(
    [string]$InferenceRoot,
    [string]$SelectedProfile,
    [bool]$BuildComplete = $true
) {
    $envPath = Join-Path $RepoRoot ".env"
    $existing = if (Test-Path -LiteralPath $envPath -PathType Leaf) {
        [System.IO.File]::ReadAllText($envPath, [System.Text.Encoding]::UTF8)
    } else {
        ""
    }
    $pattern = "(?ms)^$([regex]::Escape($ManagedEnvStart))\r?\n.*?^$([regex]::Escape($ManagedEnvEnd))\r?\n?"
    $existing = [regex]::Replace($existing, $pattern, "").TrimEnd()
    $block = Get-ManagedEnvBlock $InferenceRoot $SelectedProfile $BuildComplete
    $body = if ([string]::IsNullOrWhiteSpace($existing)) { $block } else { "$existing`r`n`r`n$block" }
    Write-Utf8FileAtomically $envPath ($body.TrimEnd() + "`r`n")
}

function Build-Application([string]$Npm, [string]$Cargo) {
    Write-Step "Building the web interface"
    Push-Location (Join-Path $RepoRoot "web")
    try {
        Invoke-Checked $Npm @("ci")
        Invoke-Checked $Npm @("run", "build")
    } finally {
        Pop-Location
    }
    Write-Step "Building TaleShift"
    Push-Location $RepoRoot
    try {
        Invoke-Checked $Cargo @("build", "-p", "gml-app", "--release", "--locked")
    } finally {
        Pop-Location
    }
}

$SelectedProfile = Resolve-Profile
$InferenceRoot = Resolve-InferenceRoot
$NeedsLocalModels = $SelectedProfile -ne "Minimal"
$NeedsImages = $SelectedProfile -in @("Images", "Full")
$previousHfToken = $env:HF_TOKEN
$previousHfHubToken = $env:HUGGING_FACE_HUB_TOKEN
$previousHfHome = $env:HF_HOME
$previousHfHubCache = $env:HF_HUB_CACHE
$previousHfTelemetry = $env:HF_HUB_DISABLE_TELEMETRY
$previousUvCache = $env:UV_CACHE_DIR
$previousUvPythonInstall = $env:UV_PYTHON_INSTALL_DIR
$tokenPointer = [IntPtr]::Zero
Remove-Item Env:HF_TOKEN -ErrorAction SilentlyContinue
Remove-Item Env:HUGGING_FACE_HUB_TOKEN -ErrorAction SilentlyContinue

try {
    Write-Host "TaleShift setup"
    Write-Host "Profile:        $SelectedProfile"
    Write-Host "Inference home: $InferenceRoot"

    if ($VerifyOnly) {
        Assert-InstalledProfile $InferenceRoot $SelectedProfile
    } else {
        Confirm-RestrictedLicenses $SelectedProfile
    }
    if ($NeedsLocalModels) {
        Assert-Nvidia $NeedsImages
        if (-not $VerifyOnly) { Assert-FreeSpace $InferenceRoot $SelectedProfile }
    }

    $git = $null
    $cargo = $null
    $npm = $null
    if ($NeedsLocalModels) { $git = Get-RequiredCommand "git" "Install Git for Windows." }
    if (-not $SkipBuild -and -not $VerifyOnly) {
        $node = Get-RequiredCommand "node" "Install Node.js 20 LTS or 22+."
        Assert-Node $node
        $npm = Get-RequiredCommand "npm" "Install Node.js with npm."
        Assert-WindowsBuildPrerequisites
        $rustup = Get-RequiredCommand "rustup" "Install Rust from https://rustup.rs/."
        Invoke-Checked $rustup @(
            "toolchain", "install", $RustVersion,
            "--profile", "minimal",
            "--component", "rustfmt",
            "--component", "clippy"
        )
        $cargo = Get-RequiredCommand "cargo" "Install Rust through rustup."
        Assert-Rust $cargo
    }

    $runtimePython = $null
    $imagePython = $null
    if ($NeedsLocalModels) {
        $env:UV_CACHE_DIR = Join-Path $InferenceRoot "uv-cache"
        $env:UV_PYTHON_INSTALL_DIR = Join-Path $InferenceRoot "uv-python"
        $env:HF_HOME = Join-Path $InferenceRoot "hf"
        $env:HF_HUB_CACHE = Join-Path $env:HF_HOME "hub"
        $env:HF_HUB_DISABLE_TELEMETRY = "1"
        $runtimeVenv = Join-Path $InferenceRoot "runtime\.venv"
        $runtimePython = Join-Path $runtimeVenv "Scripts\python.exe"
        $uv = Get-Uv
        if ($VerifyOnly) {
            Assert-LockedEnvironment $uv $runtimePython $runtimeVenv $RuntimePythonVersion (Join-Path $RepoRoot "sidecar\requirements-runtime.lock")
            if ($NeedsImages) {
                $imageRoot = Join-Path $InferenceRoot "image"
                $imageVenv = Join-Path $imageRoot ".venv"
                $imagePython = Join-Path $imageVenv "Scripts\python.exe"
                Assert-LockedEnvironment $uv $imagePython $imageVenv $ImagePythonVersion (Join-Path $RepoRoot "sidecar\requirements-image.lock")
                Assert-ComfyUi $git (Join-Path $imageRoot "ComfyUI")
            }
        } else {
            Write-Step "Preparing the local inference runtime"
            $runtimePython = Ensure-Venv $uv $RuntimePythonVersion $runtimeVenv (Join-Path $RepoRoot "sidecar\requirements-runtime.lock")

            if ($NeedsImages) {
                $imageRoot = Join-Path $InferenceRoot "image"
                $comfyDir = Join-Path $imageRoot "ComfyUI"
                Write-Step "Preparing ComfyUI"
                Ensure-ComfyUi $git $comfyDir
                Write-Step "Preparing the image runtime"
                [void](Ensure-Venv $uv $ImagePythonVersion (Join-Path $imageRoot ".venv") (Join-Path $RepoRoot "sidecar\requirements-image.lock"))
                $imagePython = Join-Path $imageRoot ".venv\Scripts\python.exe"
            }
        }

        Assert-TorchCuda $runtimePython $false
        if ($NeedsImages) { Assert-TorchCuda $imagePython $true }

        $modelHfToken = if ([string]::IsNullOrWhiteSpace($previousHfToken)) {
            $previousHfHubToken
        } else {
            $previousHfToken
        }
        if (-not $VerifyOnly -and [string]::IsNullOrWhiteSpace($modelHfToken) -and -not $NonInteractive) {
            Write-Host "All current models are public. You may provide an HF read token; it will not be saved."
            $secureToken = Read-Host "Hugging Face token (Enter to skip)" -AsSecureString
            $tokenPointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureToken)
            $plainToken = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($tokenPointer)
            if (-not [string]::IsNullOrWhiteSpace($plainToken)) { $modelHfToken = $plainToken }
            $plainToken = $null
        }

        Write-Step $(if ($VerifyOnly) { "Verifying models" } else { "Downloading and verifying models" })
        $modelArguments = @(
            (Join-Path $RepoRoot "sidecar\install_models.py"),
            "--home", $InferenceRoot,
            "--profile", $SelectedProfile,
            "--accept-restricted"
        )
        if ($VerifyOnly) { $modelArguments += "--verify-only" }
        try {
            if (-not [string]::IsNullOrWhiteSpace($modelHfToken)) {
                $env:HF_TOKEN = $modelHfToken
            }
            Invoke-Checked $runtimePython $modelArguments
        } finally {
            Remove-Item Env:HF_TOKEN -ErrorAction SilentlyContinue
            $modelHfToken = $null
            if ($tokenPointer -ne [IntPtr]::Zero) {
                [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($tokenPointer)
                $tokenPointer = [IntPtr]::Zero
            }
        }
    }

    if (-not $VerifyOnly) {
        Set-ManagedEnv $InferenceRoot $SelectedProfile (-not $SkipBuild)
        New-Item -ItemType Directory -Force -Path $InferenceRoot | Out-Null
        if (-not $SkipBuild) { Build-Application $npm $cargo }
        $sourceFingerprint = $null
        $executableSha256 = $null
        $webDistFingerprint = $null
        if (-not $SkipBuild) {
            $executable = Get-GmlReleaseExecutablePath $RepoRoot
            if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
                throw "The release build did not produce $executable."
            }
            $sourceFingerprint = Get-GmlSourceFingerprint $RepoRoot
            $executableSha256 = Get-GmlFileSha256 $executable
            $webDistFingerprint = Get-GmlTreeFingerprint $RepoRoot (Join-Path $RepoRoot "web\dist")
        }
        $state = [ordered]@{
            schema_version = 2
            profile = $SelectedProfile
            repository = $RepoRoot
            build_complete = -not $SkipBuild
            source_fingerprint = $sourceFingerprint
            executable_sha256 = $executableSha256
            web_dist_fingerprint = $webDistFingerprint
            updated_at = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
        } | ConvertTo-Json
        Write-Utf8FileAtomically (Join-Path $InferenceRoot "install.json") ($state + "`n")
    }

    Write-Host "`nDone." -ForegroundColor Green
    if ($VerifyOnly) {
        Write-Host "Profile $SelectedProfile passed verification."
    } elseif ($SkipBuild) {
        Write-Host "Dependencies are installed. Run setup.cmd again without -SkipBuild to build the app."
    } else {
        Write-Host "Run: .\run.cmd"
    }
} finally {
    if ($tokenPointer -ne [IntPtr]::Zero) {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($tokenPointer)
    }
    foreach ($item in @(
            @{ Name = "HF_TOKEN"; Value = $previousHfToken },
            @{ Name = "HUGGING_FACE_HUB_TOKEN"; Value = $previousHfHubToken },
            @{ Name = "HF_HOME"; Value = $previousHfHome },
            @{ Name = "HF_HUB_CACHE"; Value = $previousHfHubCache },
            @{ Name = "HF_HUB_DISABLE_TELEMETRY"; Value = $previousHfTelemetry },
            @{ Name = "UV_CACHE_DIR"; Value = $previousUvCache },
            @{ Name = "UV_PYTHON_INSTALL_DIR"; Value = $previousUvPythonInstall }
        )) {
        if ($null -eq $item.Value) {
            Remove-Item "Env:$($item.Name)" -ErrorAction SilentlyContinue
        } else {
            Set-Item "Env:$($item.Name)" $item.Value
        }
    }
}
