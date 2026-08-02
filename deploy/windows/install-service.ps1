# Registers hush-server as a scheduled task that starts with the machine and
# restarts itself if the process dies. Task Scheduler is used instead of sc.exe
# because hush-server is a plain executable, not a Windows service.
#
# Run as administrator from the deployment folder:
#   powershell -ExecutionPolicy Bypass -File install-service.ps1

#Requires -RunAsAdministrator

$ErrorActionPreference = "Stop"

$root = $PSScriptRoot
$launcher = Join-Path $root "hush-server.cmd"
$taskName = "HushServer"

foreach ($required in @($launcher, (Join-Path $root "hush-server.exe"))) {
    if (-not (Test-Path $required)) {
        throw "$required is missing. Copy the whole deployment package into the same folder."
    }
}

# The database folder has to exist before the first start.
$dbLine = Select-String -Path $launcher -Pattern '^set HUSH_DB=sqlite://(.+?)\?' | Select-Object -First 1
if ($dbLine) {
    $dbPath = $dbLine.Matches[0].Groups[1].Value -replace '/', '\'
    $dbDir = Split-Path $dbPath -Parent
    if (-not (Test-Path $dbDir)) {
        New-Item -ItemType Directory -Path $dbDir -Force | Out-Null
        Write-Host "Created the data folder: $dbDir"
    }
}

if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
    Write-Host "Task $taskName already exists; recreating it."
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
}

$action = New-ScheduledTaskAction -Execute "cmd.exe" -Argument "/c `"$launcher`"" -WorkingDirectory $root
$trigger = New-ScheduledTaskTrigger -AtStartup
# SYSTEM so it starts without anyone logging in. If the SMTP relay or the data
# folder require a different identity, change the principal to that account.
$principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -StartWhenAvailable `
    -RestartCount 999 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero)

Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger `
    -Principal $principal -Settings $settings `
    -Description "Hush relay server (HTTP on 127.0.0.1:8080, behind Apache)" | Out-Null

Start-ScheduledTask -TaskName $taskName
Start-Sleep -Seconds 3

# Actually check that it came up. This fails instead of warning: a registered
# task that never starts the process looks installed when it is not.
$listening = $false
foreach ($attempt in 1..10) {
    Start-Sleep -Seconds 2
    try {
        Invoke-WebRequest "http://127.0.0.1:8080/" -TimeoutSec 5 -UseBasicParsing | Out-Null
        $listening = $true
        break
    } catch {}
}

if ($listening) {
    Write-Host "Hush server is running and answering on 127.0.0.1:8080." -ForegroundColor Green
} else {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    throw @"
The server is not answering on 127.0.0.1:8080; the task has been removed so it
is not left registered and broken.

Run hush-server.cmd by hand in this folder to see the error. The usual causes
are a HUSH_DB or HUSH_LOG_FILE path the service account cannot write to, or
port 8080 already in use.
"@
}
