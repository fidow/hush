# Despliegue de hush-server (Windows, detrás de Apache)

Contenido de este paquete:

| Fichero | Qué es |
|---|---|
| `hush-server.exe` | El servidor. Binario único, sin dependencias que instalar. |
| `hush-server.cmd` | Su configuración: variables de entorno. **Es lo único que hay que editar.** |
| `install-service.ps1` | Registra el arranque automático y comprueba que responde. Opcional. |

---

## Qué hace el servidor

Es un proceso HTTP **sin TLS** que escucha en `127.0.0.1:8080`. Expone una API
REST bajo `/v1/...`, una página de descarga en `/`, y un endpoint de
**Server-Sent Events** (`/v1/messages/stream`) que mantiene una conexión HTTP
abierta indefinidamente por cada usuario conectado.

Guarda todo en un único fichero SQLite. No necesita base de datos externa,
runtime ni servicio adicional.

---

## Lo que hay que hacer en la máquina

**1. Copiar el paquete** a una carpeta, por ejemplo `C:\hush\`.

**2. Editar `hush-server.cmd`**: la ruta de la base de datos y los datos del
relay SMTP. Sin SMTP nadie puede verificar su cuenta ni recuperar la
contraseña, así que es obligatorio que funcione.

**3. Crear la carpeta de datos** que indique `HUSH_DB` y dar permiso de
escritura a la cuenta que ejecutará el proceso. Ese fichero contiene las
cuentas y el archivo cifrado: **es lo único que hay que respaldar**.

La ruta del log se define aparte, en `HUSH_LOG_FILE`, y admite cualquier
ubicación absoluta —otra unidad o un recurso de red— con la carpeta creada
automáticamente. Rota a diario (`hush.log.2026-08-02`) y **los ficheros
antiguos no se borran solos**: si el volumen es pequeño, conviene una tarea de
limpieza. Sin esta variable el servidor escribe por consola, que como tarea
programada significa perder el registro.

**4. Arrancarlo al iniciar la máquina** y que se reinicie si muere.
`install-service.ps1` lo hace con el Programador de tareas; si preferís NSSM o
cualquier otro sistema, vale igual. No es un servicio de Windows nativo: es un
ejecutable normal, así que necesita un envoltorio.

**5. No abrir el 8080 en el firewall.** El acceso desde fuera entra por Apache.

---

## Lo que necesita del Apache que ya existe

El virtualhost de `hush.villasante.es` tiene que hacer de proxy inverso hacia
`http://127.0.0.1:8080`, con estos requisitos:

**Todo el dominio al backend.** No hay rutas que deba servir Apache: la raíz
`/` la sirve el propio servidor (página de descarga) y el resto es la API.

**El endpoint `/v1/messages/stream` necesita trato especial.** Es lo único
delicado del despliegue y si falla la app parece rota:

- **Sin buffering ni compresión** en esa ruta. Si Apache acumula la respuesta
  (típicamente por `mod_deflate`), los mensajes no llegan en tiempo real o no
  llegan.
- **Timeout largo**, del orden de una hora. Con el valor por defecto Apache
  corta la conexión al minuto y el cliente entra en un ciclo de reconexión
  continuo.

**Reenviar la IP del cliente.** El servidor limita intentos de registro, login
y verificación por IP. Detrás de un proxy solo ve la de Apache, así que
necesita `X-Forwarded-For` — que `mod_proxy` ya añade — y que en
`hush-server.cmd` esté `HUSH_TRUST_PROXY=1` (viene puesto).

> Esa variable **solo** debe estar activa si de verdad hay un proxy delante.
> Si el servidor quedara accesible directamente, cualquiera podría falsificar
> la cabecera y saltarse los límites.

**Permitir cuerpos de petición de unos 20 MB.** Las imágenes viajan dentro del
mensaje cifrado; el servidor ya rechaza lo que pase de 15 MB por su cuenta.

**Redirigir HTTP a HTTPS.** La app siempre habla por HTTPS.

Opcional pero recomendable: activar en Apache el intercambio de claves híbrido
post-cuántico (`X25519MLKEM768`) si la versión de OpenSSL lo soporta, para que
también el transporte lo sea. No es imprescindible — el cifrado de los
mensajes es independiente del TLS y ya es resistente a lo cuántico.

---

## Comprobar que funciona

Desde la propia máquina, `http://127.0.0.1:8080/` debe devolver la página de
descarga. Desde fuera, `https://hush.villasante.es/` debe devolver lo mismo.

La prueba que de verdad importa es el stream: abrir
`https://hush.villasante.es/v1/messages/stream` en un navegador debe quedarse
**colgado sin devolver nada** (da 401 sin sesión, pero no debe cerrarse ni dar
error de proxy). Si responde al instante con un error 502 o 504, el proxy no
está bien configurado para SSE.

---

## Actualizaciones

Parar el proceso, reemplazar `hush-server.exe`, arrancarlo. La base de datos se
migra sola al arrancar; no hay que borrarla ni ejecutar nada.
