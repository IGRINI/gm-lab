# Hot-reload dev server reachable from a phone over HTTPS (HMR + microphone).
#
# Run the Rust backend FIRST, in a separate terminal, in LAN mode so it both
# serves the API on :8000 and generates the self-signed TLS cert this script
# reuses (.tls SAN already includes your LAN IP):
#
#     $env:GM_HOST = "0.0.0.0"
#     cargo run -p gml-app -- --server
#
# Then, in web/ :   ./dev-phone.ps1
# On the phone:     https://<your-LAN-IP>:5173   (accept the cert once)

$ErrorActionPreference = "Stop"

$tls  = Join-Path $env:APPDATA "gm-lab\data\.tls"
$cert = Join-Path $tls "gmlab-cert.pem"
$key  = Join-Path $tls "gmlab-key.pem"

if (-not (Test-Path $cert) -or -not (Test-Path $key)) {
  Write-Host "TLS cert not found in $tls" -ForegroundColor Yellow
  Write-Host "Start the backend once in LAN mode to generate it, then re-run this:" -ForegroundColor Yellow
  Write-Host '    $env:GM_HOST = "0.0.0.0"; cargo run -p gml-app -- --server' -ForegroundColor Cyan
  exit 1
}

$env:GM_DEV_HOST = "1"
$env:GM_DEV_CERT = $cert
$env:GM_DEV_KEY  = $key

# Show the LAN URLs to open on the phone.
$ips = Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue |
  Where-Object { $_.IPAddress -notlike "127.*" -and $_.IPAddress -notlike "169.254.*" } |
  Select-Object -ExpandProperty IPAddress
$backend = if ($env:GM_BACKEND_URL) { $env:GM_BACKEND_URL } else { "http://127.0.0.1:8000" }
Write-Host "Vite (HMR) over HTTPS - open on your phone:" -ForegroundColor Green
foreach ($ip in $ips) { Write-Host "    https://${ip}:5173" -ForegroundColor Green }
Write-Host "Proxying API to $backend"
Write-Host ""

npm run dev
