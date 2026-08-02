/// <reference types="vite/client" />
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { CATEGORIES, MAX_RECENT, RECENT_KEY } from "./emoji";

interface Message {
  id: string;
  sender: string;
  kind: string; // "text" | "image" (imagen = data URL en `text`)
  text: string;
  created_at: number;
  mine: boolean;
}

interface StoredMessage {
  id: string;
  contact: string;
  mine: boolean;
  kind: string;
  text: string;
  created_at: number;
}

interface ProfileInfo {
  username: string;
  alias: string;
  server: string;
}

interface Contact {
  alias: string;
  messages: Message[];
  loaded: boolean;
}

const contacts = new Map<string, Contact>();
let me = "";
let myAlias = "";
let current: string | null = null;

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;

function toast(text: string) {
  const el = $("#toast");
  el.textContent = text;
  el.classList.remove("hidden");
  setTimeout(() => el.classList.add("hidden"), 4000);
}

function show(screen: "boot" | "login" | "verify" | "chat") {
  for (const s of ["boot", "login", "verify", "chat"]) {
    $(`#${s}`).classList.toggle("hidden", s !== screen);
  }
}

// ---- Contactos ----

function contactLabel(name: string): string {
  const alias = contacts.get(name)?.alias;
  return alias && alias !== name ? alias : name;
}

function renderContactItem(name: string) {
  const li = document.querySelector<HTMLElement>(`#contact-list li[data-name="${name}"]`);
  if (!li) return;
  li.replaceChildren();
  const alias = document.createElement("span");
  alias.textContent = contactLabel(name);
  const user = document.createElement("small");
  user.textContent = `@${name}`;
  li.append(alias, user);
}

function ensureContact(name: string, alias?: string) {
  if (contacts.has(name)) {
    if (alias) {
      contacts.get(name)!.alias = alias;
      renderContactItem(name);
      if (current === name) updateHeader();
    }
    return;
  }
  contacts.set(name, { alias: alias ?? name, messages: [], loaded: false });
  const li = document.createElement("li");
  li.dataset.name = name;
  li.addEventListener("click", () => selectContact(name));
  $("#contact-list").appendChild(li);
  renderContactItem(name);
  if (!alias) {
    invoke<string>("add_contact", { username: name })
      .then((a) => ensureContact(name, a || name))
      .catch(() => {});
  }
}

function updateHeader() {
  if (!current) return;
  $("#conv-header").textContent = `🔒 ${contactLabel(current)} · @${current}`;
}

async function selectContact(name: string) {
  current = name;
  document.querySelectorAll("#contact-list li").forEach((li) => {
    li.classList.toggle("active", (li as HTMLElement).dataset.name === name);
    if ((li as HTMLElement).dataset.name === name) li.classList.remove("unread");
  });
  updateHeader();
  ($("#send-input") as HTMLInputElement).disabled = false;
  ($("#send-btn") as HTMLButtonElement).disabled = false;
  ($("#emoji-btn") as HTMLButtonElement).disabled = false;

  const contact = contacts.get(name)!;
  if (!contact.loaded) {
    try {
      const hist = await invoke<StoredMessage[]>("get_history", { contact: name });
      const restored: Message[] = hist.map((m) => ({
        id: m.id,
        sender: m.mine ? me : m.contact,
        kind: m.kind,
        text: m.text,
        created_at: m.created_at,
        mine: m.mine,
      }));
      // Live messages may have arrived while loading; merge without dupes.
      const seen = new Set(restored.map((m) => m.id));
      for (const m of contact.messages) if (!seen.has(m.id)) restored.push(m);
      contact.messages = restored;
      contact.loaded = true;
    } catch (err) {
      toast(`No se pudo cargar el historial: ${err}`);
    }
  }
  renderMessages();
  $("#send-input").focus();
}

function renderMessages() {
  const container = $("#messages");
  container.replaceChildren();
  if (!current) return;
  for (const msg of contacts.get(current)?.messages ?? []) {
    const div = document.createElement("div");
    div.className = `bubble ${msg.mine ? "mine" : "theirs"}`;
    if (msg.kind === "image") {
      const img = document.createElement("img");
      img.src = msg.text;
      img.alt = "Imagen";
      img.title = "Clic para ampliar";
      img.addEventListener("click", () => openLightbox(msg.text));
      div.classList.add("image");
      div.appendChild(img);
    } else {
      div.textContent = msg.text;
    }
    div.title = new Date(msg.created_at).toLocaleTimeString();
    container.appendChild(div);
  }
  container.scrollTop = container.scrollHeight;
}

function addMessage(contact: string, msg: Message) {
  ensureContact(contact);
  contacts.get(contact)!.messages.push(msg);
  if (current === contact) {
    renderMessages();
  } else {
    document
      .querySelector(`#contact-list li[data-name="${contact}"]`)
      ?.classList.add("unread");
  }
}

// ---- Sesión ----

async function enterChat(profile: ProfileInfo) {
  me = profile.username;
  myAlias = profile.alias || me;
  $("#me-alias").textContent = myAlias;
  $("#me-name").textContent = `@${me}`;
  try {
    await invoke("connect");
  } catch (err) {
    show("login");
    toast(`No se pudo conectar: ${err}`);
    return;
  }
  try {
    const list = await invoke<[string, string][]>("get_contacts");
    for (const [username, alias] of list) ensureContact(username, alias);
  } catch {
    /* sin contactos aún */
  }
  show("chat");
}

async function boot() {
  try {
    const profile = await invoke<ProfileInfo | null>("load_profile");
    if (profile) {
      await enterChat(profile);
      return;
    }
  } catch (err) {
    toast(String(err));
  }
  show("login");
}

// ---- Alternar entrar / crear cuenta (login por defecto) ----

function setAuthMode(register: boolean) {
  $("#login-form").classList.toggle("hidden", !register);
  $("#signin-form").classList.toggle("hidden", register);
  $(register ? "#username-input" : "#signin-username-input").focus();
}

$("#to-register").addEventListener("click", () => setAuthMode(true));
$("#to-signin").addEventListener("click", () => setAuthMode(false));

$("#login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const btn = $("#login-btn") as HTMLButtonElement;
  btn.disabled = true;
  $("#login-error").textContent = "";
  try {
    const devCode = await invoke<string | null>("register", {
      server: ($("#server-input") as HTMLInputElement).value.trim(),
      username: ($("#username-input") as HTMLInputElement).value.trim().toLowerCase(),
      alias: ($("#alias-input") as HTMLInputElement).value.trim(),
      email: ($("#email-input") as HTMLInputElement).value.trim(),
      password: ($("#password-input") as HTMLInputElement).value,
      historyPassphrase: ($("#history-input") as HTMLInputElement).value,
    });
    me = ($("#username-input") as HTMLInputElement).value.trim().toLowerCase();
    myAlias = ($("#alias-input") as HTMLInputElement).value.trim() || me;
    show("verify");
    if (devCode) ($("#code-input") as HTMLInputElement).value = devCode;
    $("#code-input").focus();
  } catch (err) {
    $("#login-error").textContent = String(err);
  } finally {
    btn.disabled = false;
  }
});

$("#verify-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const btn = $("#verify-btn") as HTMLButtonElement;
  btn.disabled = true;
  $("#verify-error").textContent = "";
  try {
    await invoke("verify", {
      code: ($("#code-input") as HTMLInputElement).value.trim(),
    });
    await enterChat({
      username: me,
      alias: myAlias,
      server: ($("#server-input") as HTMLInputElement).value.trim(),
    });
  } catch (err) {
    $("#verify-error").textContent = String(err);
  } finally {
    btn.disabled = false;
  }
});

$("#signin-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const btn = $("#signin-btn") as HTMLButtonElement;
  btn.disabled = true;
  $("#signin-error").textContent = "";
  try {
    const profile = await invoke<ProfileInfo>("login", {
      server: ($("#signin-server-input") as HTMLInputElement).value.trim(),
      username: ($("#signin-username-input") as HTMLInputElement).value.trim().toLowerCase(),
      password: ($("#signin-password-input") as HTMLInputElement).value,
      historyPassphrase: ($("#signin-history-input") as HTMLInputElement).value,
    });
    await enterChat(profile);
  } catch (err) {
    $("#signin-error").textContent = String(err);
  } finally {
    btn.disabled = false;
  }
});

// ---- Contactos y envío ----

$("#add-contact-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $("#add-contact-input") as HTMLInputElement;
  const name = input.value.trim().toLowerCase();
  input.value = "";
  if (!name || name === me) return;
  try {
    const alias = await invoke<string>("add_contact", { username: name });
    ensureContact(name, alias || name);
    selectContact(name);
  } catch (err) {
    toast(`No se pudo añadir a ${name}: ${err}`);
  }
});

$("#send-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $("#send-input") as HTMLInputElement;
  const text = input.value.trim();
  if (!text || !current) return;
  const recipient = current;
  input.value = "";
  hideEmojiPanel();
  try {
    const stored = await invoke<StoredMessage>("send_message", { recipient, text });
    addMessage(recipient, {
      id: stored.id,
      sender: me,
      kind: stored.kind,
      text: stored.text,
      created_at: stored.created_at,
      mine: true,
    });
  } catch (err) {
    toast(`Error al enviar: ${err}`);
    input.value = text;
  }
});

listen<{ id: string; sender: string; kind: string; text: string; created_at: number }>(
  "hush://message",
  ({ payload }) => {
    addMessage(payload.sender, { ...payload, mine: false });
  },
);

// ---- Pegar imágenes ----

let pendingImage: string | null = null;

document.addEventListener("paste", (e) => {
  if (!current || $("#chat").classList.contains("hidden")) return;
  const item = Array.from(e.clipboardData?.items ?? []).find((i) =>
    i.type.startsWith("image/"),
  );
  if (!item) return;
  e.preventDefault();
  const file = item.getAsFile();
  if (!file) return;
  const reader = new FileReader();
  reader.onload = () => {
    const dataUrl = reader.result as string;
    if (dataUrl.length > 10 * 1024 * 1024) {
      toast("La imagen es demasiado grande (máximo ~7 MB)");
      return;
    }
    pendingImage = dataUrl;
    ($("#img-preview-img") as HTMLImageElement).src = dataUrl;
    $("#img-preview").classList.remove("hidden");
  };
  reader.readAsDataURL(file);
});

function closeImagePreview() {
  pendingImage = null;
  $("#img-preview").classList.add("hidden");
}

$("#img-cancel").addEventListener("click", closeImagePreview);

$("#img-send").addEventListener("click", async () => {
  if (!pendingImage || !current) return;
  const recipient = current;
  const dataUrl = pendingImage;
  const btn = $("#img-send") as HTMLButtonElement;
  btn.disabled = true;
  try {
    const stored = await invoke<StoredMessage>("send_image", { recipient, dataUrl });
    addMessage(recipient, {
      id: stored.id,
      sender: me,
      kind: stored.kind,
      text: stored.text,
      created_at: stored.created_at,
      mine: true,
    });
    closeImagePreview();
  } catch (err) {
    toast(`Error al enviar la imagen: ${err}`);
  } finally {
    btn.disabled = false;
  }
});

listen("hush://disconnected", () => toast("Conexión con el servidor perdida"));

// ---- Selector de emojis ----

function loadRecent(): string[] {
  try {
    return JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
  } catch {
    return [];
  }
}

function pushRecent(emoji: string) {
  const recent = [emoji, ...loadRecent().filter((e) => e !== emoji)].slice(0, MAX_RECENT);
  localStorage.setItem(RECENT_KEY, JSON.stringify(recent));
}

let activeCategory = 0; // -1 = recientes

function insertEmoji(emoji: string) {
  const input = $("#send-input") as HTMLInputElement;
  const start = input.selectionStart ?? input.value.length;
  const end = input.selectionEnd ?? input.value.length;
  input.value = input.value.slice(0, start) + emoji + input.value.slice(end);
  const pos = start + emoji.length;
  input.setSelectionRange(pos, pos);
  input.focus();
  pushRecent(emoji);
}

function renderEmojiGrid() {
  const grid = $("#emoji-grid");
  grid.replaceChildren();
  const emojis = activeCategory === -1 ? loadRecent() : CATEGORIES[activeCategory].emojis;
  if (activeCategory === -1 && emojis.length === 0) {
    const empty = document.createElement("p");
    empty.className = "emoji-empty";
    empty.textContent = "Aún no has usado emojis";
    grid.appendChild(empty);
    return;
  }
  for (const emoji of emojis) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "emoji";
    btn.textContent = emoji;
    btn.addEventListener("click", () => insertEmoji(emoji));
    grid.appendChild(btn);
  }
}

function renderEmojiTabs() {
  const tabs = $("#emoji-tabs");
  tabs.replaceChildren();
  const makeTab = (icon: string, index: number, title: string) => {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = `emoji-tab ${activeCategory === index ? "active" : ""}`;
    btn.textContent = icon;
    btn.title = title;
    btn.addEventListener("click", () => {
      activeCategory = index;
      renderEmojiTabs();
      renderEmojiGrid();
    });
    tabs.appendChild(btn);
  };
  makeTab("🕘", -1, "Recientes");
  CATEGORIES.forEach((cat, i) => makeTab(cat.icon, i, cat.name));
}

function hideEmojiPanel() {
  $("#emoji-panel").classList.add("hidden");
}

$("#emoji-btn").addEventListener("click", () => {
  const panel = $("#emoji-panel");
  if (panel.classList.contains("hidden")) {
    if (loadRecent().length > 0) activeCategory = -1;
    renderEmojiTabs();
    renderEmojiGrid();
    panel.classList.remove("hidden");
  } else {
    hideEmojiPanel();
  }
});

// ---- Visor de imágenes ----

function openLightbox(src: string) {
  ($("#img-lightbox-img") as HTMLImageElement).src = src;
  $("#img-lightbox").classList.remove("hidden");
}

$("#img-lightbox").addEventListener("click", () => {
  $("#img-lightbox").classList.add("hidden");
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    hideEmojiPanel();
    $("#img-lightbox").classList.add("hidden");
  }
});

// ---- Menú contextual propio ----
// The browser menu is suppressed everywhere except text inputs; the app shows
// its own menu with actions relevant to what was clicked. Shift+right-click
// bypasses it (handy in dev to reach Inspect).

interface CtxItem {
  label: string;
  action: () => void;
}

function closeContextMenu() {
  document.getElementById("ctx-menu")?.remove();
}

function showContextMenu(items: CtxItem[], x: number, y: number) {
  closeContextMenu();
  const menu = document.createElement("div");
  menu.id = "ctx-menu";
  menu.className = "ctx-menu";
  for (const item of items) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.textContent = item.label;
    btn.addEventListener("click", () => {
      closeContextMenu();
      item.action();
    });
    menu.appendChild(btn);
  }
  document.body.appendChild(menu);
  const rect = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(x, window.innerWidth - rect.width - 8)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - rect.height - 8)}px`;
}

document.addEventListener("mousedown", (e) => {
  if (!(e.target as HTMLElement).closest("#ctx-menu")) closeContextMenu();
});

async function copyImageToClipboard(dataUrl: string) {
  try {
    const blob = await (await fetch(dataUrl)).blob();
    await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
    toast("Imagen copiada");
  } catch {
    toast("No se pudo copiar la imagen");
  }
}

document.addEventListener("contextmenu", (e) => {
  if (e.shiftKey) return;
  const t = e.target as HTMLElement;
  e.preventDefault();

  // Text fields get the usual editing actions, but rendered by the app.
  if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement) {
    const input = t;
    const hasSelection = input.selectionStart !== input.selectionEnd;
    const selected = () =>
      input.value.slice(input.selectionStart ?? 0, input.selectionEnd ?? 0);
    const replaceSelection = (text: string) => {
      const start = input.selectionStart ?? input.value.length;
      const end = input.selectionEnd ?? input.value.length;
      input.value = input.value.slice(0, start) + text + input.value.slice(end);
      const pos = start + text.length;
      input.setSelectionRange(pos, pos);
      input.focus();
    };
    const items: CtxItem[] = [];
    if (hasSelection) {
      items.push({
        label: "Cortar",
        action: () => {
          navigator.clipboard.writeText(selected());
          replaceSelection("");
        },
      });
      items.push({
        label: "Copiar",
        action: () => void navigator.clipboard.writeText(selected()),
      });
    }
    items.push({
      label: "Pegar",
      action: async () => {
        try {
          replaceSelection(await navigator.clipboard.readText());
        } catch {
          toast("No se pudo pegar");
        }
      },
    });
    if (input.value) {
      items.push({
        label: "Seleccionar todo",
        action: () => {
          input.focus();
          input.select();
        },
      });
    }
    showContextMenu(items, e.clientX, e.clientY);
    return;
  }

  const bubble = t.closest<HTMLElement>(".bubble");
  if (bubble) {
    if (bubble.classList.contains("image")) {
      const src = bubble.querySelector("img")?.src ?? "";
      showContextMenu(
        [
          { label: "Ver imagen", action: () => openLightbox(src) },
          { label: "Copiar imagen", action: () => copyImageToClipboard(src) },
        ],
        e.clientX,
        e.clientY,
      );
    } else {
      const text = bubble.textContent ?? "";
      showContextMenu(
        [
          {
            label: "Copiar texto",
            action: () => {
              navigator.clipboard.writeText(text).then(
                () => toast("Texto copiado"),
                () => toast("No se pudo copiar"),
              );
            },
          },
        ],
        e.clientX,
        e.clientY,
      );
    }
    return;
  }

  const contactLi = t.closest<HTMLElement>("#contact-list li");
  if (contactLi?.dataset.name) {
    const name = contactLi.dataset.name;
    showContextMenu(
      [
        {
          label: `Copiar @${name}`,
          action: () => {
            navigator.clipboard.writeText(name).then(
              () => toast("Usuario copiado"),
              () => toast("No se pudo copiar"),
            );
          },
        },
      ],
      e.clientX,
      e.clientY,
    );
  }
});

boot();
