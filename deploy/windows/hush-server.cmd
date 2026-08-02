@echo off
rem ---------------------------------------------------------------------------
rem Arranca hush-server con su configuracion. Este es el fichero que edita el
rem administrador; la tarea programada lo llama a el, no al .exe directamente,
rem porque las variables de entorno se definen aqui.
rem
rem Edita los valores de abajo y no toques el resto.
rem ---------------------------------------------------------------------------

rem --- Red -------------------------------------------------------------------
rem Escuchar SOLO en localhost: quien entra de fuera lo hace por Apache. No
rem cambiar a 0.0.0.0 salvo que sepas lo que haces: el servidor habla HTTP sin
rem cifrar y exponerlo directamente anularia el TLS.
set HUSH_ADDR=127.0.0.1:8080

rem Apache va por delante, asi que el limite de peticiones por IP debe usar la
rem cabecera X-Forwarded-For en vez de ver siempre la IP de Apache.
set HUSH_TRUST_PROXY=1

rem --- Base de datos ---------------------------------------------------------
rem Ruta del fichero SQLite. La carpeta debe existir y la cuenta que ejecuta el
rem servicio necesita permiso de escritura. Aqui viven las cuentas, la cola de
rem mensajes y el archivo cifrado: es lo unico que hay que respaldar.
set HUSH_DB=sqlite://C:/hush/data/hush.sqlite3?mode=rwc

rem --- Registro --------------------------------------------------------------
rem info en produccion. Con debug se registra quien escribe a quien y cuando,
rem que es justo el metadato que no conviene dejar en disco.
set HUSH_LOG=info

rem Fichero de log. Como tarea programada no hay consola donde mirar, asi que
rem sin esto el registro se pierde. Vale cualquier ruta absoluta: otra unidad
rem (D:\logs\hush.log) o un recurso de red (\\servidor\logs\hush.log). La
rem carpeta se crea sola. Rota a diario: hush.log.2026-08-02, y los ficheros
rem antiguos se van borrando segun HUSH_LOG_KEEP_DAYS.
set HUSH_LOG_FILE=C:\hush\logs\hush.log

rem Dias de logs rotados que se conservan. Los mas antiguos se borran al
rem arrancar y una vez al dia. Solo se tocan los ficheros generados por el
rem propio servidor, nunca otros que haya en esa carpeta. Con 0 no se borra
rem nada y se conservan indefinidamente.
set HUSH_LOG_KEEP_DAYS=30

rem --- Correo ----------------------------------------------------------------
rem Sin esto nadie puede verificar su cuenta ni recuperar la contrasena.
set HUSH_SMTP_HOST=192.168.210.101
set HUSH_SMTP_PORT=25
set HUSH_SMTP_FROM=hush@lineage2.es
rem Descomenta si el relay pide autenticacion o STARTTLS:
rem set HUSH_SMTP_USER=usuario
rem set HUSH_SMTP_PASS=contrasena
rem set HUSH_SMTP_STARTTLS=1

rem --- NUNCA en produccion ---------------------------------------------------
rem HUSH_ECHO_CODE devolveria los codigos de verificacion en la respuesta HTTP.
rem El servidor ya lo ignora cuando hay SMTP configurado, pero no lo definas.

cd /d "%~dp0"
hush-server.exe
