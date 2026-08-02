import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { CATEGORIES, MAX_RECENT, RECENT_KEY } from "./emoji";

interface Message {
  id: string;
  sender: string;
  text: string;
  created_at: number;
  mine: boolean;
}

interface Contact {
  alias: string;
  messages: Message[];
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
  contacts.set(name, { alias: alias ?? name, messages: [] });
  const li = document.createElement("li");
  li.dataset.name = name;
  li.addEventListener("click", () => selectContact(name));
  $("#contact-list").appendChild(li);
  renderContactItem(name);
  if (!alias) {
    // Resolve the alias in the background (e.g. contact added by incoming message).
    invoke<string>("get_profile", { username: name })
      .then((a) => ensureContact(name, a || name))
      .catch(() => {});
  }
}

function updateHeader() {
  if (!current) return;
  $("#conv-header").textContent = `🔒 ${contactLabel(current)} · @${current}`;
}

function selectContact(name: string) {
  current = name;
  document
    .querySelectorAll("#contact-list li")
    .forEach((li) => {
      li.classList.toggle("active", (li as HTMLElement).dataset.name === name);
      if ((li as HTMLElement).dataset.name === name) li.classList.remove("unread");
    });
  updateHeader();
  ($("#send-input") as HTMLInputElement).disabled = false;
  ($("#send-btn") as HTMLButtonElement).disabled = false;
  ($("#emoji-btn") as HTMLButtonElement).disabled = false;
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
    div.textContent = msg.text;
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

// ---- Registro y verificación ----

$("#login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const btn = $("#login-btn") as HTMLButtonElement;
  btn.disabled = true;
  $("#login-error").textContent = "";
  try {
    const devCode = await invoke<string | null>("register", {
      server: ($("#server-input") as HTMLInputElement).value.trim(),
      username: ($("#username-input") as HTMLInputElement).value.trim(),
      alias: ($("#alias-input") as HTMLInputElement).value.trim(),
      email: ($("#email-input") as HTMLInputElement).value.trim(),
      password: ($("#password-input") as HTMLInputElement).value,
    });
    me = ($("#username-input") as HTMLInputElement).value.trim();
    myAlias = ($("#alias-input") as HTMLInputElement).value.trim() || me;
    $("#login").classList.add("hidden");
    $("#verify").classList.remove("hidden");
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
    $("#me-alias").textContent = myAlias;
    $("#me-name").textContent = `@${me}`;
    $("#verify").classList.add("hidden");
    $("#chat").classList.remove("hidden");
  } catch (err) {
    $("#verify-error").textContent = String(err);
  } finally {
    btn.disabled = false;
  }
});

// ---- Contactos y mensajes ----

$("#add-contact-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $("#add-contact-input") as HTMLInputElement;
  const name = input.value.trim();
  input.value = "";
  if (!name || name === me) return;
  try {
    const alias = await invoke<string>("get_profile", { username: name });
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
    await invoke("send_message", { recipient, text });
    addMessage(recipient, {
      id: crypto.randomUUID(),
      sender: me,
      text,
      created_at: Date.now(),
      mine: true,
    });
  } catch (err) {
    toast(`Error al enviar: ${err}`);
    input.value = text;
  }
});

listen<{ id: string; sender: string; text: string; created_at: number }>(
  "hush://message",
  ({ payload }) => {
    addMessage(payload.sender, { ...payload, mine: false });
  },
);

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

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") hideEmojiPanel();
});
