/// <reference types="vite/client" />
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { CATEGORIES, MAX_RECENT, RECENT_KEY } from "./emoji";
import { applyTranslations, LANGUAGES, lang, setLang, t, tError, type Lang } from "./i18n";

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
  status: string;
}

interface Contact {
  alias: string;
  status: string;
  messages: Message[];
  loaded: boolean;
}

/// Servers users can pick from, each with a friendly name. Add new
/// deployments here and both the sign-in and register forms pick them up.
const SERVERS: { name: string; url: string }[] = [
  { name: "Local", url: "http://127.0.0.1:8080" },
  { name: "Main Hush", url: "https://hush.villasante.es" },
];

const SETTABLE_STATUSES = ["online", "away", "busy"] as const;
const PRESENCE_POLL_MS = 20_000;

const contacts = new Map<string, Contact>();
let me = "";
let myAlias = "";
let myStatus = "online";
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
  const contact = contacts.get(name);
  const unread = li.classList.contains("unread");
  li.replaceChildren();

  const dot = document.createElement("span");
  dot.className = `dot status-${contact?.status ?? "offline"}`;
  dot.title = t(`status.${contact?.status ?? "offline"}`);
  const names = document.createElement("div");
  names.className = "contact-names";
  const alias = document.createElement("span");
  alias.textContent = contactLabel(name);
  const user = document.createElement("small");
  user.textContent = `@${name}`;
  names.append(alias, user);
  li.append(dot, names);
  li.classList.toggle("unread", unread);
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
  contacts.set(name, { alias: alias ?? name, status: "offline", messages: [], loaded: false });
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
  const status = contacts.get(current)?.status ?? "offline";
  $("#conv-header").textContent =
    `🔒 ${contactLabel(current)} · @${current} · ${t(`status.${status}`)}`;
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
    } catch (e) {
      toast(`${t("error.historyFailed")}: ${tError(e)}`);
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
      img.alt = "";
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

// ---- Presencia ----

async function refreshPresence() {
  if (contacts.size === 0) return;
  try {
    const presence = await invoke<[string, string][]>("get_presence");
    for (const [username, status] of presence) {
      const contact = contacts.get(username);
      if (contact && contact.status !== status) {
        contact.status = status;
        renderContactItem(username);
        if (current === username) updateHeader();
      }
    }
  } catch {
    // Transient: the next poll will catch up.
  }
}

function renderMyStatus() {
  $("#me-dot").className = `dot status-${myStatus}`;
  $("#me-dot").title = t(`status.${myStatus}`);
}

// ---- Sesión ----

async function enterChat(profile: ProfileInfo) {
  me = profile.username;
  myAlias = profile.alias || me;
  myStatus = profile.status || "online";
  $("#me-alias").textContent = myAlias;
  $("#me-name").textContent = `@${me}`;
  renderMyStatus();
  try {
    await invoke("connect");
  } catch (e) {
    show("login");
    toast(tError(e));
    return;
  }
  try {
    const list = await invoke<[string, string][]>("get_contacts");
    for (const [username, alias] of list) ensureContact(username, alias);
  } catch {
    /* sin contactos aún */
  }
  show("chat");
  refreshPresence();
  setInterval(refreshPresence, PRESENCE_POLL_MS);
}

async function boot() {
  populateServers();
  populateLanguages();
  populateStatuses();
  applyTranslations();
  try {
    const profile = await invoke<ProfileInfo | null>("load_profile");
    if (profile) {
      await enterChat(profile);
      return;
    }
  } catch (e) {
    toast(tError(e));
  }
  show("login");
}

// ---- Selectores ----

function populateServers() {
  for (const [selectId, urlId] of [
    ["#server-input", "#server-url"],
    ["#signin-server-input", "#signin-server-url"],
  ]) {
    const select = $(selectId) as HTMLSelectElement;
    select.replaceChildren();
    for (const server of SERVERS) {
      const option = document.createElement("option");
      option.value = server.url;
      option.textContent = server.name;
      select.appendChild(option);
    }
    const showUrl = () => ($(urlId).textContent = select.value);
    select.addEventListener("change", showUrl);
    showUrl();
  }
}

function populateLanguages() {
  for (const id of ["#lang-input", "#settings-lang"]) {
    const select = $(id) as HTMLSelectElement;
    select.replaceChildren();
    for (const language of LANGUAGES) {
      const option = document.createElement("option");
      option.value = language.code;
      option.textContent = language.name;
      select.appendChild(option);
    }
    select.value = lang();
  }
}

function populateStatuses() {
  const select = $("#settings-status") as HTMLSelectElement;
  select.replaceChildren();
  for (const status of SETTABLE_STATUSES) {
    const option = document.createElement("option");
    option.value = status;
    option.textContent = t(`status.${status}`);
    select.appendChild(option);
  }
}

/// Re-renders every piece of text after a language change.
function refreshLanguage(next: Lang) {
  setLang(next);
  applyTranslations();
  populateStatuses();
  ($("#settings-status") as HTMLSelectElement).value = myStatus;
  for (const id of ["#lang-input", "#settings-lang"]) {
    ($(id) as HTMLSelectElement).value = next;
  }
  for (const name of contacts.keys()) renderContactItem(name);
  renderMyStatus();
  if (current) updateHeader();
}

$("#lang-input").addEventListener("change", (e) =>
  refreshLanguage((e.target as HTMLSelectElement).value as Lang),
);
$("#settings-lang").addEventListener("change", (e) =>
  refreshLanguage((e.target as HTMLSelectElement).value as Lang),
);

// ---- Ajustes ----

$("#settings-btn").addEventListener("click", () => {
  ($("#settings-alias") as HTMLInputElement).value = myAlias;
  ($("#settings-status") as HTMLSelectElement).value = myStatus;
  ($("#settings-lang") as HTMLSelectElement).value = lang();
  $("#settings-error").textContent = "";
  // The key is only revealed on demand, never just by opening settings.
  $("#recovery-code").textContent = "••••";
  $("#recovery-show").textContent = t("recovery.show");
  recoveryShown = false;
  $("#settings").classList.remove("hidden");
});

$("#settings-close").addEventListener("click", () =>
  $("#settings").classList.add("hidden"),
);

// ---- Clave de recuperación ----

let recoveryShown = false;

async function recoveryCode(): Promise<string> {
  return invoke<string>("get_recovery_code");
}

$("#recovery-show").addEventListener("click", async () => {
  const label = $("#recovery-code");
  if (recoveryShown) {
    label.textContent = "••••";
    $("#recovery-show").textContent = t("recovery.show");
    recoveryShown = false;
    return;
  }
  try {
    label.textContent = await recoveryCode();
    $("#recovery-show").textContent = t("recovery.hide");
    recoveryShown = true;
  } catch (err) {
    $("#settings-error").textContent = tError(err);
  }
});

$("#recovery-copy").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText(await recoveryCode());
    toast(t("recovery.copied"));
  } catch (err) {
    $("#settings-error").textContent = tError(err);
  }
});

$("#restore-btn").addEventListener("click", async () => {
  const input = $("#restore-input") as HTMLInputElement;
  const code = input.value.trim();
  if (!code) return;
  const btn = $("#restore-btn") as HTMLButtonElement;
  btn.disabled = true;
  $("#settings-error").textContent = "";
  try {
    const count = await invoke<number>("restore_history", { code });
    input.value = "";
    // Restored messages land in the local database; drop the cached
    // conversations so they are re-read on next open.
    for (const contact of contacts.values()) {
      contact.loaded = false;
      contact.messages = [];
    }
    const list = await invoke<[string, string][]>("get_contacts");
    for (const [username, alias] of list) ensureContact(username, alias);
    if (current) await selectContact(current);
    toast(count > 0 ? t("restore.done").replace("{n}", String(count)) : t("restore.empty"));
    refreshPresence();
  } catch (err) {
    $("#settings-error").textContent = tError(err);
  } finally {
    btn.disabled = false;
  }
});

$("#settings-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const alias = ($("#settings-alias") as HTMLInputElement).value.trim();
  const status = ($("#settings-status") as HTMLSelectElement).value;
  const btn = $("#settings-save") as HTMLButtonElement;
  btn.disabled = true;
  $("#settings-error").textContent = "";
  try {
    await invoke("update_me", {
      alias: alias !== myAlias ? alias : null,
      status: status !== myStatus ? status : null,
    });
    myAlias = alias;
    myStatus = status;
    $("#me-alias").textContent = myAlias;
    renderMyStatus();
    $("#settings").classList.add("hidden");
    toast(t("settings.saved"));
  } catch (err) {
    $("#settings-error").textContent = tError(err);
  } finally {
    btn.disabled = false;
  }
});

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
      server: ($("#server-input") as HTMLSelectElement).value,
      username: ($("#username-input") as HTMLInputElement).value.trim().toLowerCase(),
      alias: ($("#alias-input") as HTMLInputElement).value.trim(),
      email: ($("#email-input") as HTMLInputElement).value.trim(),
      password: ($("#password-input") as HTMLInputElement).value,
    });
    me = ($("#username-input") as HTMLInputElement).value.trim().toLowerCase();
    myAlias = ($("#alias-input") as HTMLInputElement).value.trim() || me;
    show("verify");
    if (devCode) ($("#code-input") as HTMLInputElement).value = devCode;
    $("#code-input").focus();
  } catch (err) {
    $("#login-error").textContent = tError(err);
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
      server: ($("#server-input") as HTMLSelectElement).value,
      status: "online",
    });
  } catch (err) {
    $("#verify-error").textContent = tError(err);
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
      server: ($("#signin-server-input") as HTMLSelectElement).value,
      username: ($("#signin-username-input") as HTMLInputElement).value.trim().toLowerCase(),
      password: ($("#signin-password-input") as HTMLInputElement).value,
    });
    await enterChat(profile);
  } catch (err) {
    $("#signin-error").textContent = tError(err);
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
    refreshPresence();
  } catch (err) {
    toast(`${t("error.addContactFailed")}: ${tError(err)}`);
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
    toast(`${t("error.sendFailed")}: ${tError(err)}`);
    input.value = text;
  }
});

listen<{ id: string; sender: string; kind: string; text: string; created_at: number }>(
  "hush://message",
  ({ payload }) => {
    addMessage(payload.sender, { ...payload, mine: false });
  },
);

listen("hush://disconnected", () => toast(t("error.disconnected")));

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
      toast(t("image.tooBig"));
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
    toast(`${t("error.imageFailed")}: ${tError(err)}`);
  } finally {
    btn.disabled = false;
  }
});

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
    empty.textContent = t("chat.noRecentEmojis");
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
  makeTab("🕘", -1, t("chat.recentEmojis"));
  CATEGORIES.forEach((cat, i) => makeTab(cat.icon, i, t(cat.key)));
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
    $("#settings").classList.add("hidden");
  }
});

// ---- Menú contextual propio ----
// The browser menu is suppressed everywhere; the app shows its own with
// actions relevant to what was clicked. Shift+right-click bypasses it.

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

function copyText(text: string) {
  navigator.clipboard.writeText(text).then(
    () => toast(t("ctx.copied")),
    () => toast(t("ctx.copyFailed")),
  );
}

async function copyImageToClipboard(dataUrl: string) {
  try {
    const blob = await (await fetch(dataUrl)).blob();
    await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
    toast(t("image.copied"));
  } catch {
    toast(t("image.copyFailed"));
  }
}

document.addEventListener("contextmenu", (e) => {
  if (e.shiftKey) return;
  const target = e.target as HTMLElement;
  e.preventDefault();

  // Text fields get the usual editing actions, but rendered by the app.
  if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) {
    const input = target;
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
        label: t("ctx.cut"),
        action: () => {
          navigator.clipboard.writeText(selected());
          replaceSelection("");
        },
      });
      items.push({
        label: t("ctx.copy"),
        action: () => void navigator.clipboard.writeText(selected()),
      });
    }
    items.push({
      label: t("ctx.paste"),
      action: async () => {
        try {
          replaceSelection(await navigator.clipboard.readText());
        } catch {
          toast(t("ctx.pasteFailed"));
        }
      },
    });
    if (input.value) {
      items.push({
        label: t("ctx.selectAll"),
        action: () => {
          input.focus();
          input.select();
        },
      });
    }
    showContextMenu(items, e.clientX, e.clientY);
    return;
  }

  const bubble = target.closest<HTMLElement>(".bubble");
  if (bubble) {
    if (bubble.classList.contains("image")) {
      const src = bubble.querySelector("img")?.src ?? "";
      showContextMenu(
        [
          { label: t("ctx.view"), action: () => openLightbox(src) },
          { label: t("ctx.copyImage"), action: () => copyImageToClipboard(src) },
        ],
        e.clientX,
        e.clientY,
      );
    } else {
      const text = bubble.textContent ?? "";
      showContextMenu(
        [{ label: t("ctx.copyText"), action: () => copyText(text) }],
        e.clientX,
        e.clientY,
      );
    }
    return;
  }

  const contactLi = target.closest<HTMLElement>("#contact-list li");
  if (contactLi?.dataset.name) {
    const name = contactLi.dataset.name;
    showContextMenu(
      [{ label: t("ctx.copyUser"), action: () => copyText(name) }],
      e.clientX,
      e.clientY,
    );
  }
});

boot();
