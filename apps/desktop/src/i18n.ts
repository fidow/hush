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
      "Tus conversaciones se guardan cifradas con una clave de recuperación que se genera automáticamente. Podrás verla en Ajustes y usarla para recuperar el historial en otro dispositivo.",
    "auth.signin.note":
      "En un dispositivo nuevo se generan claves de cifrado nuevas. Para traerte el historial, entra y usa tu clave de recuperación desde Ajustes.",
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
      "Cambiar la contraseña no afecta a tu clave de recuperación: esa es la que restaura el historial en otro dispositivo. Se cerrará la sesión en todos tus dispositivos.",
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
    "chat.messagePlaceholder": "Escribe un mensaje cifrado…",
    "chat.send": "Enviar",
    "chat.emojis": "Emojis",
    "chat.settings": "Ajustes",
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
    "settings.notifications": "Mostrar notificaciones",
    "settings.sound": "Sonido de aviso",

    "notify.image": "📷 Te ha enviado una imagen",
    "notify.requestTitle": "Nueva solicitud de contacto",
    "notify.requestBody": "{who} quiere añadirte",

    "recovery.title": "Clave de recuperación",
    "recovery.note":
      "Guárdala en un lugar seguro. Es lo único que permite recuperar tu historial en otro dispositivo, y nadie más la tiene: ni siquiera el servidor.",
    "recovery.show": "Mostrar",
    "recovery.hide": "Ocultar",
    "recovery.copy": "Copiar",
    "recovery.copied": "Clave copiada",

    "restore.title": "Recuperar historial",
    "restore.note":
      "Pega aquí la clave de recuperación de otro dispositivo para traerte sus conversaciones.",
    "restore.placeholder": "XXXX-XXXX-XXXX-…",
    "restore.action": "Recuperar",
    "restore.done": "Historial recuperado: {n} mensajes",
    "restore.empty": "No había historial que recuperar",

    "image.preview": "Imagen a enviar",
    "image.send": "Enviar imagen",
    "image.cancel": "Cancelar",
    "image.tooBig": "La imagen es demasiado grande (máximo ~7 MB)",
    "image.copied": "Imagen copiada",
    "image.copyFailed": "No se pudo copiar la imagen",

    "ctx.view": "Ver imagen",
    "ctx.copyImage": "Copiar imagen",
    "ctx.copyText": "Copiar texto",
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
    "err.wrong_recovery_key": "Esa clave de recuperación no es la de esta cuenta",
    "err.invalid_recovery_key": "La clave de recuperación no tiene un formato válido",
    "err.no_recovery_key":
      "Este dispositivo aún no tiene la clave de la cuenta. Recupérala aquí abajo con la clave de tu otro dispositivo.",
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
      "Your conversations are stored encrypted under a recovery key generated automatically. You can view it in Settings and use it to restore your history on another device.",
    "auth.signin.note":
      "A new device generates new encryption keys. To bring your history along, sign in and use your recovery key from Settings.",
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
      "Changing your password does not affect your recovery key: that is what restores your history on another device. You will be signed out on all your devices.",
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
    "chat.messagePlaceholder": "Write an encrypted message…",
    "chat.send": "Send",
    "chat.emojis": "Emojis",
    "chat.settings": "Settings",
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
    "settings.notifications": "Show notifications",
    "settings.sound": "Alert sound",

    "notify.image": "📷 Sent you an image",
    "notify.requestTitle": "New contact request",
    "notify.requestBody": "{who} wants to add you",

    "recovery.title": "Recovery key",
    "recovery.note":
      "Keep it somewhere safe. It is the only thing that can restore your history on another device, and nobody else holds it: not even the server.",
    "recovery.show": "Show",
    "recovery.hide": "Hide",
    "recovery.copy": "Copy",
    "recovery.copied": "Recovery key copied",

    "restore.title": "Restore history",
    "restore.note":
      "Paste the recovery key from another device to bring its conversations here.",
    "restore.placeholder": "XXXX-XXXX-XXXX-…",
    "restore.action": "Restore",
    "restore.done": "History restored: {n} messages",
    "restore.empty": "There was no history to restore",

    "image.preview": "Image to send",
    "image.send": "Send image",
    "image.cancel": "Cancel",
    "image.tooBig": "That image is too large (about 7 MB max)",
    "image.copied": "Image copied",
    "image.copyFailed": "Could not copy the image",

    "ctx.view": "View image",
    "ctx.copyImage": "Copy image",
    "ctx.copyText": "Copy text",
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
    "err.wrong_recovery_key": "That recovery key does not belong to this account",
    "err.invalid_recovery_key": "That recovery key is not in a valid format",
    "err.no_recovery_key":
      "This device doesn't hold the account key yet. Restore it below using the key from your other device.",
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
