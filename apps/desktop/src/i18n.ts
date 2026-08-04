// UI translations. The server always answers in English and identifies each
// failure with a stable code, so error text is localised here too.

export type Lang = "es" | "en";

const LANG_KEY = "hush-lang";

export const LANGUAGES: { code: Lang; name: string }[] = [
  { code: "es", name: "Español" },
  { code: "en", name: "English" },
];

const STRINGS: Record<Lang, Record<string, string>> = {
  es: {
    "app.tagline": "Mensajería privada con cifrado post-cuántico",
    "boot.connecting": "Conectando…",

    "auth.server": "Servidor",
    "auth.username": "Nombre de usuario",
    "auth.username.placeholder": "ej. alicia",
    "auth.alias": "Alias (nombre visible)",
    "auth.alias.placeholder": "ej. Alicia G.",
    "auth.email": "Email",
    "auth.password": "Contraseña",
    "auth.password.placeholder": "mínimo 8 caracteres",
    "auth.history.note":
      "Tus conversaciones se guardan cifradas solo en este dispositivo: el servidor no guarda historial. Desde Ajustes puedes exportarlas a un fichero con contraseña para llevártelas a otro sitio.",
    "auth.signin.note":
      "Solo puede haber un dispositivo a la vez: al entrar aquí se cerrará la sesión donde la tuvieras abierta. Para traerte las conversaciones, expórtalas allí e impórtalas aquí desde Ajustes.",
    "auth.signin": "Entrar",
    "auth.register": "Crear cuenta",
    "auth.forgot": "¿Has olvidado tu contraseña?",
    "forgot.note":
      "Te enviaremos un código a tu email para que puedas elegir una contraseña nueva.",
    "forgot.code": "Código recibido por email",
    "forgot.newPassword": "Contraseña nueva",
    "forgot.send": "Enviar código",
    "forgot.reset": "Cambiar contraseña",
    "forgot.sent": "Si la cuenta existe, recibirás un código por email",
    "forgot.done": "Contraseña cambiada, ya puedes entrar",
    "forgot.back": "Volver",
    "forgot.keyNote":
      "Cambiar la contraseña no afecta a las conversaciones guardadas en este dispositivo. Se cerrará la sesión.",
    "auth.toRegister": "¿No tienes cuenta? Crear una",
    "auth.toSignin": "¿Ya tienes cuenta? Entrar",

    "verify.title": "Verifica tu cuenta",
    "verify.note":
      "Te hemos enviado un código de 6 dígitos a tu email. Introdúcelo para activar la cuenta.",
    "verify.code": "Código de verificación",
    "verify.submit": "Verificar",

    "chat.addContact": "añadir contacto…",
    "contacts.title": "Contactos",
    "contacts.requests": "Solicitudes",
    "contacts.accept": "Aceptar",
    "contacts.reject": "Rechazar",
    "contacts.pending": "pendiente",
    "contacts.empty": "Aún no tienes contactos",
    "contacts.requestSent": "Solicitud enviada",
    "contacts.nowContacts": "Ya sois contactos",
    "contacts.remove": "Eliminar contacto",
    "chat.pickContact": "Selecciona un contacto",
    "chat.messagePlaceholder": "Escribe un mensaje…",
    "chat.send": "Enviar",
    "chat.emojis": "Emojis",
    "chat.settings": "Ajustes",
    "chat.back": "Volver a la lista",
    "chat.noRecentEmojis": "Aún no has usado emojis",
    "chat.recentEmojis": "Recientes",

    "emoji.smileys": "Caritas",
    "emoji.gestures": "Gestos",
    "emoji.animals": "Animales",
    "emoji.food": "Comida",
    "emoji.activities": "Actividades",
    "emoji.objects": "Objetos",
    "emoji.symbols": "Símbolos",

    "status.online": "En línea",
    "status.away": "Ausente",
    "status.busy": "Ocupado",
    "status.offline": "Desconectado",
    "status.lastSeen": "últ. vez {when}",

    "receipt.sent": "Enviado",
    "receipt.delivered": "Recibido",
    "receipt.read": "Leído",

    "settings.title": "Ajustes",
    "settings.alias": "Nombre visible",
    "settings.status": "Estado",
    "settings.language": "Idioma",
    "settings.save": "Guardar",
    "settings.close": "Cerrar",
    "settings.saved": "Ajustes guardados",
    "empty.pickTitle": "Ninguna conversación abierta",
    "empty.pickNote":
      "Elige un contacto de la lista para leer y escribir.",
    "empty.noContactsTitle": "Todavía no tienes contactos",
    "empty.noContactsNote":
      "Escribe un nombre de usuario arriba a la izquierda para enviarle una solicitud. Podréis hablar cuando la acepte.",

    "settings.fontSize": "Tamaño de letra",
    "font.small": "Pequeña",
    "font.normal": "Normal",
    "font.large": "Grande",
    "font.huge": "Muy grande",

    "settings.theme": "Apariencia",
    "theme.system": "Como el sistema",
    "theme.dark": "Oscura",
    "theme.light": "Clara",
    "settings.notifications": "Mostrar notificaciones",
    "settings.sound": "Sonido de aviso",
    "settings.alerts": "Avisos",
    "alerts.sound": "Sonido",
    "alerts.vibrate": "Vibración",
    "alerts.none": "Sin aviso",

    "notify.image": "📷 Te ha enviado una imagen",
    "notify.requestTitle": "Nueva solicitud de contacto",
    "notify.requestBody": "{who} quiere añadirte",

    "about.title": "Acerca de Hush",
    "about.version": "Versión",
    "about.account": "Cuenta",
    "about.server": "Servidor",
    "about.encryption": "Cifrado",
    "about.license": "Licencia",
    "about.website": "Sitio web",

    "avatar.change": "Cambiar foto",
    "avatar.remove": "Quitar",
    "avatar.note":
      "Tu foto se envía cifrada a cada contacto, igual que un mensaje: el servidor no la ve.",
    "avatar.updated": "Foto actualizada",
    "avatar.failed": "No se pudo cambiar la foto",

    "identity.title": "La clave de {name} ha cambiado",
    "identity.note":
      "Puede que {name} haya reinstalado la app o cambiado de dispositivo. También puede ser que alguien se haya puesto en medio. Hasta que lo confirmes no se le envía nada ni se lee nada suyo.",
    "identity.before": "La clave que teníamos:",
    "identity.after": "La que publica ahora:",
    "identity.later": "Ahora no",
    "identity.accept": "Es esa persona",
    "identity.accepted": "Clave aceptada",

    "update.title": "Versión {version} disponible",
    "update.note": "Hay una versión nueva de Hush lista para instalar.",
    "update.install": "Actualizar",
    "update.downloading": "Descargando la actualización…",
    "update.restarting": "Reiniciando Hush…",
    "update.failed": "No se pudo actualizar",
    "about.note":
      "Los mensajes van cifrados de extremo a extremo: el servidor solo transporta datos que no puede leer. El intercambio de claves es resistente a computación cuántica, y lo que se guarda en este equipo está cifrado con una clave propia del dispositivo.",

    "transfer.title": "Llevarte las conversaciones",
    "transfer.note":
      "El servidor no guarda historial: lo que tienes es lo que ha recibido este dispositivo. Para llevártelo a otro, expórtalo a un fichero e impórtalo allí.",
    "transfer.export": "Exportar",
    "transfer.import": "Importar",
    "transfer.exportTitle": "Exportar conversaciones",
    "transfer.exportNote":
      "Elige una contraseña larga: es lo único que protege el fichero, y si la pierdes no hay forma de abrirlo. Mínimo 10 caracteres.",
    "transfer.exported": "Conversaciones exportadas",
    "transfer.importTitle": "Importar conversaciones",
    "transfer.importNote": "Escribe la contraseña con la que se creó el fichero.",
    "transfer.imported": "Importados {n} mensajes",
    "transfer.importedNothing": "No había nada nuevo que importar",

    "image.preview": "Imagen a enviar",
    "image.send": "Enviar imagen",
    "image.cancel": "Cancelar",
    "image.tooBig": "La imagen es demasiado grande (máximo ~7 MB)",
    "image.copied": "Imagen copiada",
    "image.copyFailed": "No se pudo copiar la imagen",

    "ctx.view": "Ver imagen",
    "ctx.copyImage": "Copiar imagen",
    "ctx.delete": "Borrar mensaje",
    "ctx.select": "Seleccionar",
    "ctx.clearChat": "Borrar conversación",
    "clear.title": "¿Borrar la conversación con {who}?",
    "clear.note":
      "Se borra de este dispositivo. Puedes además retirar tus propios mensajes del dispositivo de la otra persona; los suyos se quedan donde están.",
    "clear.mine": "Borrar aquí",
    "clear.everyone": "Borrar aquí y mis mensajes allí",
    "clear.done": "Conversación borrada ({n} mensajes)",
    "select.count": "{n} seleccionados",
    "select.delete": "Borrar",
    "select.cancel": "Cancelar",
    "delete.someFailed": "No se pudieron borrar {n}",
    "ctx.removeContact": "Eliminar contacto",
    "ctx.block": "Bloquear",
    "ctx.unblock": "Desbloquear",

    "delete.title": "Borrar mensaje",
    "delete.note": "¿Quieres borrarlo solo para ti o también para la otra persona?",
    "delete.noteTheirs": "Se borrará solo en tu dispositivo; la otra persona conserva el suyo.",
    "delete.forMe": "Borrar para mí",
    "delete.forEveryone": "Borrar para todos",

    "confirm.cancel": "Cancelar",
    "confirm.removeTitle": "Eliminar contacto",
    "confirm.removeNote": "{who} dejará de ser contacto y ninguno de los dos podrá escribir al otro. Tus mensajes guardados no se borran.",
    "confirm.blockTitle": "Bloquear contacto",
    "confirm.blockNote": "{who} no podrá escribirte ni volver a añadirte, y no sabrá que le has bloqueado. Puedes desbloquearle cuando quieras.",

    "contacts.blocked": "Bloqueados",
    "settings.closeToTray": "Al cerrar, seguir en la bandeja del sistema",

    "err.message_not_found": "Ese mensaje ya no existe",
    "err.not_your_message": "Solo puedes borrar para todos tus propios mensajes",
    "err.payload_too_large": "El contenido es demasiado grande",
    "err.too_many_devices": "Esta cuenta ya tiene demasiados dispositivos; revoca uno primero",

    "ctx.copyText": "Copiar texto",
    "ctx.info": "Información",
    "info.title": "Información del mensaje",
    "info.sent": "Enviado",
    "info.delivered": "Recibido",
    "info.read": "Leído",
    "info.sentBy": "De",
    "info.received": "Recibido",
    "ctx.copyUser": "Copiar usuario",
    "ctx.cut": "Cortar",
    "ctx.copy": "Copiar",
    "ctx.paste": "Pegar",
    "ctx.selectAll": "Seleccionar todo",
    "ctx.copied": "Copiado",
    "ctx.copyFailed": "No se pudo copiar",
    "ctx.pasteFailed": "No se pudo pegar",

    "conn.offline": "Sin conexión con el servidor · reintentando…",
    "error.disconnected": "Conexión con el servidor perdida",
    "error.sendFailed": "No se pudo enviar el mensaje",
    "error.imageFailed": "No se pudo enviar la imagen",
    "error.addContactFailed": "No se pudo añadir el contacto",
    "error.historyFailed": "No se pudo cargar el historial",

    // Server error codes.
    "err.invalid_username":
      "El nombre de usuario debe tener de 1 a 32 caracteres (letras, números o _)",
    "err.alias_too_long": "El nombre visible es demasiado largo",
    "err.invalid_email": "El email no es válido",
    "err.weak_password": "La contraseña debe tener al menos 8 caracteres",
    "err.password_too_long": "La contraseña es demasiado larga",
    "err.invalid_request": "Datos no válidos",
    "err.username_taken": "Ese nombre de usuario ya está en uso",
    "err.rate_limited": "Demasiados intentos, prueba de nuevo más tarde",
    "err.user_not_found": "Ese usuario no existe",
    "err.already_verified": "La cuenta ya estaba verificada",
    "err.invalid_code": "Código incorrecto o caducado",
    "err.invalid_credentials": "Usuario o contraseña incorrectos",
    "err.not_verified": "La cuenta no está verificada; revisa tu email",
    "err.invalid_session": "Tu sesión no es válida",
    "err.no_keys": "Ese usuario aún no puede recibir mensajes",
    "err.mailbox_full": "El buzón del destinatario está lleno",
    "err.archive_full": "Tu archivo de historial está lleno",
    "err.invalid_id": "Identificador no válido",
    "err.too_many_prekeys": "Demasiadas claves",
    "err.invalid_keys": "Material de claves no válido",
    "err.invalid_status": "Estado desconocido",
    "err.internal_error": "Error del servidor, inténtalo de nuevo",
    "err.request_failed": "La petición fue rechazada",
    "err.connection_failed": "No se pudo conectar con el servidor",
    "err.no_session": "No has iniciado sesión",
    "err.export_password_too_short": "La contraseña debe tener al menos 10 caracteres",
    "err.import_wrong_password": "Contraseña incorrecta",
    "err.import_not_an_export": "Ese fichero no es una exportación de Hush",
    "err.import_unsupported_version":
      "Ese fichero lo hizo una versión más nueva de Hush",
    "err.identity_changed":
      "La clave de ese contacto ha cambiado. Confírmalo antes de seguir escribiéndole.",
    "err.server_busy": "El servidor está saturado, inténtalo en un momento",
    "err.request_timeout": "La petición tardó demasiado",
    "err.self_contact": "No puedes añadirte a ti mismo",
    "err.already_contacts": "Ya sois contactos",
    "err.request_pending": "Ya hay una solicitud pendiente",
    "err.no_request": "No hay ninguna solicitud que aceptar",
    "err.not_a_contact": "Solo puedes escribir a contactos aceptados",
  },
  en: {
    "app.tagline": "Private messaging with post-quantum encryption",
    "boot.connecting": "Connecting…",

    "auth.server": "Server",
    "auth.username": "Username",
    "auth.username.placeholder": "e.g. alice",
    "auth.alias": "Display name",
    "auth.alias.placeholder": "e.g. Alice G.",
    "auth.email": "Email",
    "auth.password": "Password",
    "auth.password.placeholder": "at least 8 characters",
    "auth.history.note":
      "Your conversations are kept encrypted on this device alone: the server stores no history. From Settings you can export them to a password-protected file to take them elsewhere.",
    "auth.signin.note":
      "Only one device at a time: signing in here signs you out wherever else you were. To bring your conversations along, export them there and import them here from Settings.",
    "auth.signin": "Sign in",
    "auth.register": "Create account",
    "auth.forgot": "Forgot your password?",
    "forgot.note": "We'll email you a code so you can choose a new password.",
    "forgot.code": "Code from your email",
    "forgot.newPassword": "New password",
    "forgot.send": "Send code",
    "forgot.reset": "Change password",
    "forgot.sent": "If the account exists, a code is on its way to your email",
    "forgot.done": "Password changed, you can sign in now",
    "forgot.back": "Back",
    "forgot.keyNote":
      "Changing your password does not affect the conversations stored on this device. You will be signed out.",
    "auth.toRegister": "No account yet? Create one",
    "auth.toSignin": "Already have an account? Sign in",

    "verify.title": "Verify your account",
    "verify.note":
      "We sent a 6-digit code to your email. Enter it to activate the account.",
    "verify.code": "Verification code",
    "verify.submit": "Verify",

    "chat.addContact": "add contact…",
    "contacts.title": "Contacts",
    "contacts.requests": "Requests",
    "contacts.accept": "Accept",
    "contacts.reject": "Decline",
    "contacts.pending": "pending",
    "contacts.empty": "No contacts yet",
    "contacts.requestSent": "Request sent",
    "contacts.nowContacts": "You are now contacts",
    "contacts.remove": "Remove contact",
    "chat.pickContact": "Pick a contact",
    "chat.messagePlaceholder": "Write a message…",
    "chat.send": "Send",
    "chat.emojis": "Emojis",
    "chat.settings": "Settings",
    "chat.back": "Back to the list",
    "chat.noRecentEmojis": "No emojis used yet",
    "chat.recentEmojis": "Recent",

    "emoji.smileys": "Smileys",
    "emoji.gestures": "Gestures",
    "emoji.animals": "Animals",
    "emoji.food": "Food",
    "emoji.activities": "Activities",
    "emoji.objects": "Objects",
    "emoji.symbols": "Symbols",

    "status.online": "Online",
    "status.away": "Away",
    "status.busy": "Busy",
    "status.offline": "Offline",
    "status.lastSeen": "last seen {when}",

    "receipt.sent": "Sent",
    "receipt.delivered": "Delivered",
    "receipt.read": "Read",

    "settings.title": "Settings",
    "settings.alias": "Display name",
    "settings.status": "Status",
    "settings.language": "Language",
    "settings.save": "Save",
    "settings.close": "Close",
    "settings.saved": "Settings saved",
    "empty.pickTitle": "No conversation open",
    "empty.pickNote":
      "Pick a contact from the list to read and write.",
    "empty.noContactsTitle": "No contacts yet",
    "empty.noContactsNote":
      "Type a username in the box on the left to send a request. You can talk once they accept.",

    "settings.fontSize": "Text size",
    "font.small": "Small",
    "font.normal": "Normal",
    "font.large": "Large",
    "font.huge": "Extra large",

    "settings.theme": "Appearance",
    "theme.system": "Match system",
    "theme.dark": "Dark",
    "theme.light": "Light",
    "settings.notifications": "Show notifications",
    "settings.sound": "Alert sound",
    "settings.alerts": "Alerts",
    "alerts.sound": "Sound",
    "alerts.vibrate": "Vibrate",
    "alerts.none": "Silent",

    "notify.image": "📷 Sent you an image",
    "notify.requestTitle": "New contact request",
    "notify.requestBody": "{who} wants to add you",

    "about.title": "About Hush",
    "about.version": "Version",
    "about.account": "Account",
    "about.server": "Server",
    "about.encryption": "Encryption",
    "about.license": "Licence",
    "about.website": "Website",

    "avatar.change": "Change picture",
    "avatar.remove": "Remove",
    "avatar.note":
      "Your picture is sent to each contact encrypted, like a message: the server never sees it.",
    "avatar.updated": "Picture updated",
    "avatar.failed": "Could not change the picture",

    "identity.title": "{name}'s key has changed",
    "identity.note":
      "{name} may have reinstalled the app or moved to another device. Somebody may also have stepped into the middle. Until you confirm it, nothing is sent to them and nothing of theirs is read.",
    "identity.before": "The key we had:",
    "identity.after": "The one they publish now:",
    "identity.later": "Not now",
    "identity.accept": "It is really them",
    "identity.accepted": "Key accepted",

    "update.title": "Version {version} available",
    "update.note": "A new version of Hush is ready to install.",
    "update.install": "Update",
    "update.downloading": "Downloading the update…",
    "update.restarting": "Restarting Hush…",
    "update.failed": "The update failed",
    "about.note":
      "Messages are end-to-end encrypted: the server only carries data it cannot read. The key exchange is quantum-resistant, and whatever is stored on this computer is encrypted with a key belonging to this device.",

    "transfer.title": "Taking your conversations with you",
    "transfer.note":
      "The server keeps no history: what you have is what this device received. To move it elsewhere, export it to a file and import that file there.",
    "transfer.export": "Export",
    "transfer.import": "Import",
    "transfer.exportTitle": "Export conversations",
    "transfer.exportNote":
      "Choose a long password: it is the only thing protecting the file, and losing it means the file can never be opened. At least 10 characters.",
    "transfer.exported": "Conversations exported",
    "transfer.importTitle": "Import conversations",
    "transfer.importNote": "Type the password the file was made with.",
    "transfer.imported": "Imported {n} messages",
    "transfer.importedNothing": "There was nothing new to import",

    "image.preview": "Image to send",
    "image.send": "Send image",
    "image.cancel": "Cancel",
    "image.tooBig": "That image is too large (about 7 MB max)",
    "image.copied": "Image copied",
    "image.copyFailed": "Could not copy the image",

    "ctx.view": "View image",
    "ctx.copyImage": "Copy image",
    "ctx.delete": "Delete message",
    "ctx.select": "Select",
    "ctx.clearChat": "Delete conversation",
    "clear.title": "Delete the conversation with {who}?",
    "clear.note":
      "It goes from this device. You can also withdraw your own messages from the other person's device; theirs stay where they are.",
    "clear.mine": "Delete here",
    "clear.everyone": "Delete here and my messages there",
    "clear.done": "Conversation deleted ({n} messages)",
    "select.count": "{n} selected",
    "select.delete": "Delete",
    "select.cancel": "Cancel",
    "delete.someFailed": "{n} could not be deleted",
    "ctx.removeContact": "Remove contact",
    "ctx.block": "Block",
    "ctx.unblock": "Unblock",

    "delete.title": "Delete message",
    "delete.note": "Delete it just for you, or for the other person as well?",
    "delete.noteTheirs": "It will be deleted on your device only; they keep their copy.",
    "delete.forMe": "Delete for me",
    "delete.forEveryone": "Delete for everyone",

    "confirm.cancel": "Cancel",
    "confirm.removeTitle": "Remove contact",
    "confirm.removeNote": "{who} stops being a contact and neither of you can message the other. Your saved messages are kept.",
    "confirm.blockTitle": "Block contact",
    "confirm.blockNote": "{who} will not be able to message you or add you again, and will not know they were blocked. You can unblock them at any time.",

    "contacts.blocked": "Blocked",
    "settings.closeToTray": "Keep running in the system tray when closed",

    "err.message_not_found": "That message no longer exists",
    "err.not_your_message": "You can only delete your own messages for everyone",
    "err.payload_too_large": "That content is too large",
    "err.too_many_devices": "This account already has too many devices; revoke one first",

    "ctx.copyText": "Copy text",
    "ctx.info": "Info",
    "info.title": "Message info",
    "info.sent": "Sent",
    "info.delivered": "Delivered",
    "info.read": "Read",
    "info.sentBy": "From",
    "info.received": "Received",
    "ctx.copyUser": "Copy username",
    "ctx.cut": "Cut",
    "ctx.copy": "Copy",
    "ctx.paste": "Paste",
    "ctx.selectAll": "Select all",
    "ctx.copied": "Copied",
    "ctx.copyFailed": "Could not copy",
    "ctx.pasteFailed": "Could not paste",

    "conn.offline": "No connection to the server · retrying…",
    "error.disconnected": "Lost connection to the server",
    "error.sendFailed": "Could not send the message",
    "error.imageFailed": "Could not send the image",
    "error.addContactFailed": "Could not add the contact",
    "error.historyFailed": "Could not load the history",

    "err.invalid_username":
      "Username must be 1-32 characters of letters, digits or _",
    "err.alias_too_long": "Display name is too long",
    "err.invalid_email": "Invalid email address",
    "err.weak_password": "Password must be at least 8 characters",
    "err.password_too_long": "Password is too long",
    "err.invalid_request": "Invalid data",
    "err.username_taken": "That username is already taken",
    "err.rate_limited": "Too many attempts, try again later",
    "err.user_not_found": "No such user",
    "err.already_verified": "This account is already verified",
    "err.invalid_code": "Incorrect or expired code",
    "err.invalid_credentials": "Incorrect username or password",
    "err.not_verified": "Account not verified; check your email",
    "err.invalid_session": "Your session is not valid",
    "err.no_keys": "That user cannot receive messages yet",
    "err.mailbox_full": "The recipient's mailbox is full",
    "err.archive_full": "Your history archive is full",
    "err.invalid_id": "Invalid identifier",
    "err.too_many_prekeys": "Too many keys",
    "err.invalid_keys": "Invalid key material",
    "err.invalid_status": "Unknown status",
    "err.internal_error": "Server error, please try again",
    "err.request_failed": "The request was rejected",
    "err.connection_failed": "Could not reach the server",
    "err.no_session": "You are not signed in",
    "err.export_password_too_short": "The password must be at least 10 characters",
    "err.import_wrong_password": "Wrong password",
    "err.import_not_an_export": "That file is not a Hush export",
    "err.import_unsupported_version": "That file was made by a newer version of Hush",
    "err.identity_changed":
      "That contact's key has changed. Confirm it before writing to them again.",
    "err.server_busy": "The server is busy, try again in a moment",
    "err.request_timeout": "The request took too long",
    "err.self_contact": "You cannot add yourself",
    "err.already_contacts": "You are already contacts",
    "err.request_pending": "A request is already pending",
    "err.no_request": "There is no request to accept",
    "err.not_a_contact": "You can only message accepted contacts",
  },
};

/// Stored choice, or the system language when the user has not picked one.
function detectLang(): Lang {
  const stored = localStorage.getItem(LANG_KEY);
  if (stored === "es" || stored === "en") return stored;
  return navigator.language.toLowerCase().startsWith("es") ? "es" : "en";
}

let current: Lang = detectLang();

export function lang(): Lang {
  return current;
}

export function setLang(next: Lang) {
  current = next;
  localStorage.setItem(LANG_KEY, next);
}

export function t(key: string): string {
  return STRINGS[current][key] ?? STRINGS.en[key] ?? key;
}

/// Translates an error thrown by a Tauri command. Errors carry a server code
/// (or an internal one); anything unrecognised is shown verbatim.
export function tError(e: unknown): string {
  const raw = String(e).trim();
  const key = `err.${raw}`;
  const translated = STRINGS[current][key] ?? STRINGS.en[key];
  return translated ?? raw;
}

/// Applies translations to every element tagged in the HTML.
export function applyTranslations(root: ParentNode = document) {
  root.querySelectorAll<HTMLElement>("[data-i18n]").forEach((el) => {
    el.textContent = t(el.dataset.i18n!);
  });
  root.querySelectorAll<HTMLElement>("[data-i18n-placeholder]").forEach((el) => {
    (el as HTMLInputElement).placeholder = t(el.dataset.i18nPlaceholder!);
  });
  root.querySelectorAll<HTMLElement>("[data-i18n-title]").forEach((el) => {
    el.title = t(el.dataset.i18nTitle!);
  });
}
