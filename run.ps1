[CmdletBinding()]
param(
    [switch]$Server
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$RepoRoot = $PSScriptRoot
. (Join-Path $RepoRoot "scripts\install_state.ps1")

function Resolve-InstalledInferenceRoot {
    if (-not [string]::IsNullOrWhiteSpace($env:GM_INFERENCE_HOME)) {
        return [System.IO.Path]::GetFullPath($env:GM_INFERENCE_HOME)
    }

    $envPath = Join-Path $RepoRoot ".env"
    if (Test-Path -LiteralPath $envPath -PathType Leaf) {
        $body = [System.IO.File]::ReadAllText($envPath, [System.Text.Encoding]::UTF8)
        $managed = [regex]::Match(
            $body,
            '(?ms)^# BEGIN GM-LAB SETUP\r?\n(?<block>.*?)^# END GM-LAB SETUP'
        )
        if ($managed.Success) {
            $home = [regex]::Match(
                $managed.Groups['block'].Value,
                '(?m)^GM_INFERENCE_HOME\s*=\s*(?:"(?<quoted>[^"]*)"|(?<plain>[^\r\n#]+))\s*$'
            )
            if ($home.Success) {
                $value = if ($home.Groups['quoted'].Success) {
                    $home.Groups['quoted'].Value
                } else {
                    $home.Groups['plain'].Value.Trim()
                }
                if (-not [string]::IsNullOrWhiteSpace($value)) {
                    if (-not [System.IO.Path]::IsPathRooted($value)) {
                        $value = Join-Path $RepoRoot $value
                    }
                    return [System.IO.Path]::GetFullPath($value)
                }
            }
        }
    }

    foreach ($base in @($env:LOCALAPPDATA, $env:APPDATA)) {
        if (-not [string]::IsNullOrWhiteSpace($base)) {
            return Join-Path $base "gm-lab\inference"
        }
    }
    throw "Cannot resolve the TaleShift inference directory. Rerun .\setup.cmd."
}

$InferenceRoot = Resolve-InstalledInferenceRoot
try {
    [void](Assert-GmlReleaseState $RepoRoot $InferenceRoot)
} catch {
    Write-Host "TaleShift cannot start: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
$Executable = Get-GmlReleaseExecutablePath $RepoRoot

Push-Location $RepoRoot
try {
    if ($Server) {
        & $Executable --server
    } else {
        & $Executable
    }
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
