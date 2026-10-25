# Undo disable-ghub.ps1: G HUB starts with Windows again, and its updater and
# Dynamic Lighting services are back to automatic and running.
#
# Run from an elevated PowerShell.

$ErrorActionPreference = 'Stop'

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).
    IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Error 'Run this from an elevated PowerShell (Run as administrator).'
}

$services = 'LGHUBUpdaterService', 'logi_lamparray_service'
$approvedKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'

# 02 = enabled, as Task Manager's Startup tab writes it.
if (-not (Test-Path $approvedKey)) { New-Item $approvedKey | Out-Null }
Set-ItemProperty $approvedKey -Name LGHUB -Type Binary -Value ([byte[]](2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))

foreach ($s in $services) {
    if (Get-Service $s -ErrorAction SilentlyContinue) {
        sc.exe config $s start= auto | Out-Null
        sc.exe start $s | Out-Null
    }
}

Start-Sleep -Seconds 2
Get-Service $services -ErrorAction SilentlyContinue | Format-Table Name, Status, StartType -AutoSize
Write-Host 'G HUB will start at the next sign-in; start it now from the Start menu if needed.'
