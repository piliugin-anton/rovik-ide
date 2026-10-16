# Whether msedgedriver can drive a build's WebView2: started as msedgedriver
# starts it (a debugging port asked for in WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS,
# a user data folder of its own), does its browser process listen there?
# Prints what it finds: the browser process's command line, what it listens
# on, its data folder, any WebView2/Edge policies. Exits 0 when DevTools
# answered on the port, 1 when not.
param(
  [Parameter(Mandatory)] [string] $App,
  [int] $Port = 9333
)

$base = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$data = Join-Path $base "wv2-probe-$Port"
New-Item -ItemType Directory -Force $data | Out-Null
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$Port"
$env:WEBVIEW2_USER_DATA_FOLDER = $data
$p = Start-Process $App -PassThru
Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, Env:WEBVIEW2_USER_DATA_FOLDER

$devtools = $null
for ($i = 1; $i -le 20 -and -not $devtools -and -not $p.HasExited; $i++) {
  Start-Sleep 1
  try { $devtools = Invoke-RestMethod "http://127.0.0.1:$Port/json/version" -TimeoutSec 2 } catch {}
}
if ($p.HasExited) { "The app exited after $i s, code $($p.ExitCode)" } else { "The app runs ($i s)" }
if ($devtools) { "DevTools: $($devtools.Browser)" } else { "No DevTools on port $Port" }

$browsers = @(Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" |
    Where-Object { $_.CommandLine -notmatch ' --type=' })
foreach ($b in $browsers) {
  "WebView2 browser process $($b.ProcessId) (parent $($b.ParentProcessId)): $($b.CommandLine)"
}
if (-not $browsers) { 'No WebView2 browser process' }
$ids = @(Get-Process msedgewebview2 -ErrorAction SilentlyContinue | ForEach-Object Id)
Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
  Where-Object { $ids -contains $_.OwningProcess } |
  ForEach-Object { "Listening: $($_.LocalAddress):$($_.LocalPort) (process $($_.OwningProcess))" }
Get-ChildItem $data -Recurse -Depth 1 -ErrorAction SilentlyContinue | Select-Object -First 12 |
  ForEach-Object { "In its data folder: $($_.FullName)" }
$policies = @(
  'HKLM:\SOFTWARE\Policies\Microsoft\Edge\WebView2',
  'HKCU:\SOFTWARE\Policies\Microsoft\Edge\WebView2',
  'HKLM:\SOFTWARE\Policies\Microsoft\Edge'
)
foreach ($key in $policies) {
  if (Test-Path $key) { "Policies in ${key}:"; (Get-ItemProperty $key | Format-List | Out-String).Trim() }
}

Get-Process rovik-ide, msedgewebview2 -ErrorAction SilentlyContinue |
  Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep 2
if ($devtools) { exit 0 } else { exit 1 }
