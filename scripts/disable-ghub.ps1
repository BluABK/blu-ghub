# Turn G HUB off without uninstalling it: no autostart, no updater service, no
# Dynamic Lighting bridge service. Undo with enable-ghub.ps1.
#
# Run from an elevated PowerShell (the services need it). Saves the current
# state to %APPDATA%\BluGHub\ghub-state-<time>.txt first.
#
# What stays: Logitech's kernel filter on the receiver and headset dongle
# (logi_lamparray), G HUB's virtual keyboard driver (idle without the agent),
# and the headset's audio processing (noise reduction). G HUB can still be
# started by hand, e.g. for a firmware update.

$ErrorActionPreference = 'Stop'

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).
    IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Error 'Run this from an elevated PowerShell (Run as administrator).'
}

$services = 'LGHUBUpdaterService', 'logi_lamparray_service'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$approvedKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'

# 1. Record the current state.
$dir = Join-Path $env:APPDATA 'BluGHub'
New-Item -ItemType Directory -Force $dir | Out-Null
$state = Join-Path $dir ("ghub-state-{0:yyyyMMdd-HHmmss}.txt" -f (Get-Date))
& {
    foreach ($s in $services) { sc.exe qc $s; sc.exe qfailure $s }
    Get-ItemProperty $runKey -Name LGHUB -ErrorAction SilentlyContinue | Format-List LGHUB
    $v = (Get-ItemProperty $approvedKey -Name LGHUB -ErrorAction SilentlyContinue).LGHUB
    "StartupApproved\Run\LGHUB = " + ($(if ($v) { ($v | ForEach-Object { '{0:X2}' -f $_ }) -join ' ' } else { '(absent)' }))
} *> $state
Write-Host "Saved the current state to $state"

# 2. Stop G HUB itself.
Stop-Process -Name lghub_system_tray, lghub, lghub_agent -ErrorAction SilentlyContinue

# 3. Disable its autostart the way Task Manager's Startup tab does. This
#    survives G HUB rewriting its Run value; the value itself is left alone.
if (-not (Test-Path $approvedKey)) { New-Item $approvedKey | Out-Null }
Set-ItemProperty $approvedKey -Name LGHUB -Type Binary -Value ([byte[]](3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))

# 4. Disable and stop the services.
foreach ($s in $services) {
    if (Get-Service $s -ErrorAction SilentlyContinue) {
        sc.exe config $s start= disabled | Out-Null
        sc.exe stop $s | Out-Null
    }
}

Start-Sleep -Seconds 2
Get-Service $services -ErrorAction SilentlyContinue | Format-Table Name, Status, StartType -AutoSize
$left = Get-Process lghub*, logi_lamparray* -ErrorAction SilentlyContinue
if ($left) {
    Write-Warning 'Still running:'
    $left | Format-Table Id, ProcessName -AutoSize
} else {
    Write-Host 'G HUB is off. Check again after the next reboot: both services should still be Disabled.'
}
