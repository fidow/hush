# Registra hush-server como tarea programada que arranca con la maquina y se
# reinicia sola si el proceso muere. Se usa el Programador de tareas en vez de
# sc.exe porque hush-server es un ejecutable normal, no un servicio de Windows.
#
# Ejecutar como administrador desde la carpeta del despliegue:
#   powershell -ExecutionPolicy Bypass -File install-service.ps1

#Requires -RunAsAdministrator

$ErrorActionPreference = "Stop"

$root = $PSScriptRoot
$launcher = Join-Path $root "hush-server.cmd"
$taskName = "HushServer"

foreach ($required in @($launcher, (Join-Path $root "hush-server.exe"))) {
    if (-not (Test-Path $required)) {
        throw "Falta $required. Copia todo el paquete de despliegue a la misma carpeta."
    }
}

# La carpeta de la base de datos debe existir antes del primer arranque.
$dbLine = Select-String -Path $launcher -Pattern '^set HUSH_DB=sqlite://(.+?)\?' | Select-Object -First 1
if ($dbLine) {
    $dbPath = $dbLine.Matches[0].Groups[1].Value -replace '/', '\'
    $dbDir = Split-Path $dbPath -Parent
    if (-not (Test-Path $dbDir)) {
        New-Item -ItemType Directory -Path $dbDir -Force | Out-Null
        Write-Host "Creada la carpeta de datos: $dbDir"
    }
}

if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
    Write-Host "La tarea $taskName ya existe; se vuelve a crear."
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
}

$action = New-ScheduledTaskAction -Execute "cmd.exe" -Argument "/c `"$launcher`"" -WorkingDirectory $root
$trigger = New-ScheduledTaskTrigger -AtStartup
# SYSTEM para que arranque sin que nadie inicie sesion. Si el relay SMTP o la
# carpeta de datos exigen otra identidad, cambia el principal por esa cuenta.
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
    -Description "Hush relay server (HTTP en 127.0.0.1:8080, detras de Apache)" | Out-Null

Start-ScheduledTask -TaskName $taskName
Start-Sleep -Seconds 3

# Comprobacion real de que arranco. Falla en vez de avisar: una tarea
# registrada que nunca levanta el proceso parece instalada y no lo esta.
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
    Write-Host "Hush server en marcha y respondiendo en 127.0.0.1:8080." -ForegroundColor Green
} else {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    throw @"
El servidor no responde en 127.0.0.1:8080; se ha quitado la tarea para no
dejarla registrada sin funcionar.

Ejecuta hush-server.cmd a mano en esta carpeta para ver el error. Lo mas
habitual es una ruta de HUSH_DB o HUSH_LOG_FILE en la que la cuenta del
servicio no puede escribir, o el puerto 8080 ya ocupado.
"@
}
