/// <reference types="vite/client" />
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { CATEGORIES, MAX_RECENT, RECENT_KEY } from "./emoji";
import { applyTranslations, LANGUAGES, lang, setLang, t, tError, type Lang } from "./i18n";
import {
  alertUser,
  notificationsEnabled,
  playChime,
  setNotificationsEnabled,
  setSoundEnabled,
  soundEnabled,
} from "./notify";

interface Message {
  id: string;
  sender: string;
  kind: string; // "text" | "image" (imagen = data URL en `text`)
  text: string;
  /// Para los mensajes propios: "sent", "delivered" o "read".
  state: string;
  delivered_at: number | null;
  read_at: number | null;
  created_at: number;
  mine: boolean;
}

interface StoredMessage {
  id: string;
  contact: string;
  mine: boolean;
  kind: string;
  text: string;
  state: string;
  delivered_at: number | null;
  read_at: number | null;
  created_at: number;
}

interface ProfileInfo {
  username: string;
  alias: string;
  server: string;
  status: string;
}

interface ContactEntry {
  username: string;
  alias: string;
  state: "incoming" | "outgoing" | "accepted";
  status: string;
  last_seen: number | null;
}

interface Contact {
  alias: string;
  state: string;
  status: string;
  lastSeen: number | null;
  messages: Message[];
  loaded: boolean;
}

/// Servers users can pick from, each with a friendly name. Add new
/// deployments here and both the sign-in and register forms pick them up.
/// The local one is a development convenience and is left out of packaged
/// builds, where it would only offer a connection that cannot work.
const SERVERS: { name: string; url: string }[] = [
  { name: "Main Hush", url: "https://hush.villasante.es" },
  ...(import.meta.env.DEV ? [{ name: "Local", url: "http://127.0.0.1:8080" }] : []),
];

const SETTABLE_STATUSES = ["online", "away", "busy"] as const;
/// Presence and contact changes arrive pushed over the stream; this poll is
/// only a safety net for a push that got lost.
const PRESENCE_POLL_MS = 60_000;

const contacts = new Map<string, Contact>();
/// Contacts with messages arrived while their chat was not open.
const unread = new Set<string>();

/// Whether the app window has the user's attention. `document.hasFocus()` is
/// unreliable inside the webview, so track the real window state instead.
let windowFocused = true;

/// True when the user can actually see `name`'s conversation right now.
function isWatching(name: string): boolean {
  return windowFocused && current === name;
}

/// Reports the conversation as read; safe to call when there is nothing to do.
function reportRead(name: string) {
  void invoke("mark_read", { contact: name }).catch(() => {});
}

getCurrentWindow()
  .onFocusChanged(({ payload: focused }) => {
    windowFocused = focused;
    // Coming back to an open conversation counts as reading it.
    if (focused && current) reportRead(current);
  })
  .catch(() => {});
let me = "";
let myAlias = "";
let myServer = "";
let myStatus = "online";
let current: string | null = null;

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;

function toast(text: string) {
  const el = $("#toast");
  el.textContent = text;
  el.classList.remove("hidden");
  setTimeout(() => el.classList.add("hidden"), 4000);
}

function show(screen: "boot" | "login" | "verify" | "chat" | "forgot") {
  for (const s of ["boot", "login", "verify", "chat", "forgot"]) {
    $(`#${s}`).classList.toggle("hidden", s !== screen);
  }
}

// ---- Contactos ----

function contactLabel(name: string): string {
  const alias = contacts.get(name)?.alias;
  return alias && alias !== name ? alias : name;
}

/// Rebuilds the sidebar: pending requests first, then accepted contacts.
function renderContactList() {
  const list = $("#contact-list");
  list.replaceChildren();

  const entries = [...contacts.entries()];
  const order = { incoming: 0, outgoing: 1, accepted: 2 } as Record<string, number>;
  entries.sort(
    ([an, a], [bn, b]) =>
      (order[a.state] ?? 3) - (order[b.state] ?? 3) || an.localeCompare(bn),
  );

  let lastSection: string | null = null;
  for (const [name, contact] of entries) {
    const section = contact.state === "accepted" ? "accepted" : "requests";
    if (section !== lastSection) {
      const header = document.createElement("li");
      header.className = "contact-section";
      header.textContent =
        section === "requests" ? t("contacts.requests") : t("contacts.title");
      list.appendChild(header);
      lastSection = section;
    }
    list.appendChild(contactItem(name, contact));
  }

  if (entries.length === 0) {
    const empty = document.createElement("li");
    empty.className = "contact-empty";
    empty.textContent = t("contacts.empty");
    list.appendChild(empty);
  }
}

function contactItem(name: string, contact: Contact): HTMLElement {
  const li = document.createElement("li");
  li.dataset.name = name;
  li.classList.toggle("active", current === name);
  li.classList.toggle("unread", unread.has(name));
  li.classList.add(`state-${contact.state}`);

  const dot = document.createElement("span");
  dot.className = `dot status-${contact.status}`;
  dot.title = presenceLabel(contact);

  const names = document.createElement("div");
  names.className = "contact-names";
  const alias = document.createElement("span");
  alias.textContent = contactLabel(name);
  const user = document.createElement("small");
  user.textContent =
    contact.state === "outgoing" ? `@${name} · ${t("contacts.pending")}` : `@${name}`;
  names.append(alias, user);
  li.append(dot, names);

  if (contact.state === "incoming") {
    const actions = document.createElement("div");
    actions.className = "contact-actions";
    const accept = document.createElement("button");
    accept.type = "button";
    accept.className = "mini";
    accept.textContent = t("contacts.accept");
    accept.addEventListener("click", (e) => {
      e.stopPropagation();
      void respondToRequest(name, true);
    });
    const reject = document.createElement("button");
    reject.type = "button";
    reject.className = "mini secondary";
    reject.textContent = t("contacts.reject");
    reject.addEventListener("click", (e) => {
      e.stopPropagation();
      void respondToRequest(name, false);
    });
    actions.append(accept, reject);
    li.appendChild(actions);
  } else if (contact.state === "accepted") {
    li.addEventListener("click", () => selectContact(name));
  }
  return li;
}

async function respondToRequest(name: string, accept: boolean) {
  try {
    await invoke(accept ? "accept_contact" : "remove_contact", { username: name });
    await refreshContacts();
  } catch (err) {
    toast(tError(err));
  }
}

/// Pulls the contact list from the server (states, aliases and presence).
async function refreshContacts() {
  try {
    const entries = await invoke<ContactEntry[]>("get_contacts");
    const seen = new Set<string>();
    for (const entry of entries) {
      seen.add(entry.username);
      const existing = contacts.get(entry.username);
      if (existing) {
        existing.alias = entry.alias;
        existing.state = entry.state;
        existing.status = entry.status;
        existing.lastSeen = entry.last_seen;
      } else {
        contacts.set(entry.username, {
          alias: entry.alias,
          state: entry.state,
          status: entry.status,
          lastSeen: entry.last_seen,
          messages: [],
          loaded: false,
        });
      }
    }
    for (const name of [...contacts.keys()]) {
      if (!seen.has(name)) contacts.delete(name);
    }
    if (current && !contacts.has(current)) closeConversation();
    renderContactList();
    if (current) updateHeader();
    updateConversationPane();
  } catch (err) {
    // Offline: keep showing whatever the cache holds instead of emptying it.
    if (String(err) !== "connection_failed") toast(tError(err));
  }
}

function closeConversation() {
  current = null;
  $("#messages").replaceChildren();
  updateConversationPane();
}

/// Swaps the composer for an explanation while no conversation is open, and
/// tailors it to whether there is anyone to talk to yet.
function updateConversationPane() {
  const pane = document.querySelector(".conversation")!;
  pane.classList.toggle("no-chat", current === null);
  if (current !== null) return;

  const hasContacts = [...contacts.values()].some((c) => c.state === "accepted");
  $("#conv-empty-title").textContent = t(
    hasContacts ? "empty.pickTitle" : "empty.noContactsTitle",
  );
  $("#conv-empty-note").textContent = t(
    hasContacts ? "empty.pickNote" : "empty.noContactsNote",
  );
}

/// "hace 5 minutos" / "5 minutes ago", in the active language.
function relativeTime(timestamp: number): string {
  const seconds = Math.round((timestamp - Date.now()) / 1000);
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ["second", 60],
    ["minute", 60],
    ["hour", 24],
    ["day", 7],
    ["week", 4.35],
    ["month", 12],
    ["year", Infinity],
  ];
  let value = seconds;
  for (const [unit, span] of units) {
    if (Math.abs(value) < span) {
      return new Intl.RelativeTimeFormat(lang(), { numeric: "auto" }).format(
        Math.round(value),
        unit,
      );
    }
    value /= span;
  }
  return new Date(timestamp).toLocaleDateString(lang());
}

function presenceLabel(contact: Contact | undefined): string {
  if (!contact) return t("status.offline");
  if (contact.status !== "offline") return t(`status.${contact.status}`);
  return contact.lastSeen
    ? t("status.lastSeen").replace("{when}", relativeTime(contact.lastSeen))
    : t("status.offline");
}

function updateHeader() {
  if (!current) return;
  $("#conv-header").textContent =
    `🔒 ${contactLabel(current)} · @${current} · ${presenceLabel(contacts.get(current))}`;
}

async function selectContact(name: string) {
  if (contacts.get(name)?.state !== "accepted") return;
  current = name;
  unread.delete(name);
  document.querySelectorAll("#contact-list li").forEach((li) => {
    const target = (li as HTMLElement).dataset.name === name;
    li.classList.toggle("active", target);
    if (target) li.classList.remove("unread");
  });
  updateHeader();
  updateConversationPane();
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
        state: m.state,
        delivered_at: m.delivered_at,
        read_at: m.read_at,
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
  // Opening the conversation is what "read" means.
  reportRead(name);
  $("#send-input").focus();
}

function renderMessages() {
  const container = $("#messages");
  container.replaceChildren();
  if (!current) return;
  for (const msg of contacts.get(current)?.messages ?? []) {
    const div = document.createElement("div");
    div.className = `bubble ${msg.mine ? "mine" : "theirs"}`;
    div.dataset.id = msg.id;
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
    if (msg.mine) {
      // Delivery ticks: one for sent, two for delivered, blue two for read.
      const ticks = document.createElement("span");
      ticks.className = `ticks ticks-${msg.state}`;
      ticks.textContent = msg.state === "sent" ? "✓" : "✓✓";
      ticks.title = t(`receipt.${msg.state}`);
      div.appendChild(ticks);
    }
    div.title = new Date(msg.created_at).toLocaleTimeString();
    container.appendChild(div);
  }
  container.scrollTop = container.scrollHeight;
}

function addMessage(contact: string, msg: Message) {
  const known = contacts.get(contact);
  if (!known) {
    // A message from someone not in the cached list: refresh and stash it.
    void refreshContacts();
    contacts.set(contact, {
      alias: contact,
      state: "accepted",
      status: "offline",
      lastSeen: null,
      messages: [],
      loaded: true,
    });
  }
  contacts.get(contact)!.messages.push(msg);
  if (current === contact) {
    renderMessages();
  } else {
    unread.add(contact);
    document
      .querySelector(`#contact-list li[data-name="${contact}"]`)
      ?.classList.add("unread");
  }
}

// ---- Ausencia automática ----
//
// Only "online" is managed automatically: picking away or busy by hand is a
// deliberate choice and stays until the user changes it back.

const IDLE_SECONDS = 5 * 60;
const IDLE_CHECK_MS = 20_000;
/// True while the current "away" was set by us, not by the user.
let autoAway = false;
/// Last webview activity, the fallback where system idle is unavailable.
let lastActivity = Date.now();

async function applyStatus(status: string) {
  myStatus = status;
  renderMyStatus();
  ($("#settings-status") as HTMLSelectElement).value = status;
  try {
    await invoke("update_me", { alias: null, status });
  } catch {
    // Offline: the status is re-sent on reconnect.
  }
}

async function checkIdle() {
  const system = await invoke<number | null>("idle_seconds").catch(() => null);
  const idle = system ?? Math.floor((Date.now() - lastActivity) / 1000);

  if (!autoAway && myStatus === "online" && idle >= IDLE_SECONDS) {
    autoAway = true;
    await applyStatus("away");
  } else if (autoAway && idle < IDLE_SECONDS) {
    autoAway = false;
    await applyStatus("online");
  }
}

for (const event of ["mousemove", "mousedown", "keydown", "wheel", "focus"]) {
  window.addEventListener(event, () => {
    lastActivity = Date.now();
    // Coming back is worth reflecting immediately, not at the next check.
    if (autoAway) void checkIdle();
  });
}

function renderMyStatus() {
  $("#me-dot").className = `dot status-${myStatus}`;
  $("#me-dot").title = t(`status.${myStatus}`);
}

// ---- Sesión ----

// ---- Conexión ----

const MIN_RECONNECT_MS = 1000;
const MAX_RECONNECT_MS = 30_000;
/// A connection has to survive this long before it counts as healthy.
const STABLE_CONNECTION_MS = 30_000;

let reconnectTimer: number | null = null;
let reconnectDelay = MIN_RECONNECT_MS;
let connecting = false;
let connectedAt = 0;

function setOffline(offline: boolean) {
  $("#conn-banner").classList.toggle("hidden", !offline);
}

/// Schedules a reconnection. Never immediate and never concurrent: a stream
/// that dies as soon as it opens would otherwise spin as fast as the network
/// allows, hammering the server hundreds of times a second.
function scheduleReconnect() {
  if (reconnectTimer !== null || connecting) return;
  reconnectTimer = window.setTimeout(() => {
    reconnectTimer = null;
    void connectWithRetry();
  }, reconnectDelay);
  reconnectDelay = Math.min(reconnectDelay * 2, MAX_RECONNECT_MS);
}

/// Opens the stream. A signed-in user stays in the app while it is down,
/// reading cached conversations.
async function connectWithRetry() {
  if (connecting) return;
  connecting = true;
  try {
    await invoke("connect");
    connectedAt = Date.now();
    setOffline(false);
    await refreshContacts();
  } catch {
    setOffline(true);
    scheduleReconnect();
  } finally {
    connecting = false;
  }
}

/// Called when the stream drops. The backoff only resets after a connection
/// that actually lasted, so a flapping server is backed off just like an
/// unreachable one.
function onDisconnected() {
  setOffline(true);
  if (Date.now() - connectedAt > STABLE_CONNECTION_MS) {
    reconnectDelay = MIN_RECONNECT_MS;
  }
  scheduleReconnect();
}

async function enterChat(profile: ProfileInfo) {
  me = profile.username;
  myAlias = profile.alias || me;
  myServer = profile.server;
  myStatus = profile.status || "online";
  $("#me-alias").textContent = myAlias;
  $("#me-name").textContent = `@${me}`;
  renderMyStatus();
  // Show the chat first: being unable to reach the server is not a reason to
  // ask a signed-in user to log in again.
  await refreshContacts();
  show("chat");
  await connectWithRetry();
  // Presence and request states come with the contact list.
  setInterval(refreshContacts, PRESENCE_POLL_MS);
  setInterval(checkIdle, IDLE_CHECK_MS);
}

async function boot() {
  applyTheme(currentTheme());
  applyFontSize(currentFontSize());
  populateServers();
  populateLanguages();
  populateThemes();
  populateFontSizes();
  populateStatuses();
  applyTranslations();
  updateConversationPane();
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
    ["#forgot-server-input", "#forgot-server-url"],
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

// ---- Tema claro / oscuro ----

const THEME_KEY = "hush-theme";
const THEMES = ["system", "dark", "light"] as const;
type Theme = (typeof THEMES)[number];

function currentTheme(): Theme {
  const stored = localStorage.getItem(THEME_KEY);
  return THEMES.includes(stored as Theme) ? (stored as Theme) : "system";
}

/// The stylesheet keys off this attribute; "system" lets the media query in
/// the CSS follow whatever Windows is set to.
function applyTheme(theme: Theme) {
  localStorage.setItem(THEME_KEY, theme);
  document.documentElement.dataset.theme = theme;
}

function populateThemes() {
  const select = $("#settings-theme") as HTMLSelectElement;
  select.replaceChildren();
  for (const theme of THEMES) {
    const option = document.createElement("option");
    option.value = theme;
    option.textContent = t(`theme.${theme}`);
    select.appendChild(option);
  }
  select.value = currentTheme();
}

$("#settings-theme").addEventListener("change", (e) =>
  applyTheme((e.target as HTMLSelectElement).value as Theme),
);

// ---- Tamaño de letra ----
//
// Everything is sized in rem, so scaling the root font size scales the whole
// interface consistently rather than just the message text.

const FONT_KEY = "hush-font-size";
const FONT_SIZES: { key: string; px: number }[] = [
  { key: "font.small", px: 13 },
  { key: "font.normal", px: 15 },
  { key: "font.large", px: 17 },
  { key: "font.huge", px: 19 },
];

function currentFontSize(): number {
  const stored = Number(localStorage.getItem(FONT_KEY));
  return FONT_SIZES.some((f) => f.px === stored) ? stored : 15;
}

function applyFontSize(px: number) {
  localStorage.setItem(FONT_KEY, String(px));
  document.documentElement.style.fontSize = `${px}px`;
}

function populateFontSizes() {
  const select = $("#settings-font") as HTMLSelectElement;
  select.replaceChildren();
  for (const size of FONT_SIZES) {
    const option = document.createElement("option");
    option.value = String(size.px);
    option.textContent = t(size.key);
    select.appendChild(option);
  }
  select.value = String(currentFontSize());
}

$("#settings-font").addEventListener("change", (e) =>
  applyFontSize(Number((e.target as HTMLSelectElement).value)),
);

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
  populateThemes();
  populateFontSizes();
  updateConversationPane();
  ($("#settings-status") as HTMLSelectElement).value = myStatus;
  for (const id of ["#lang-input", "#settings-lang"]) {
    ($(id) as HTMLSelectElement).value = next;
  }
  renderContactList();
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

// Quick status switcher: clicking your own name opens the picker, so the
// most frequent change doesn't need a trip through settings.
$("#me-status-btn").addEventListener("click", (e) => {
  e.stopPropagation();
  const button = $("#me-status-btn");
  const rect = button.getBoundingClientRect();
  showContextMenu(
    SETTABLE_STATUSES.map((status) => ({
      label: t(`status.${status}`),
      dot: status,
      checked: status === myStatus,
      action: () => {
        // Choosing by hand ends any automatic absence.
        autoAway = false;
        lastActivity = Date.now();
        void applyStatus(status);
      },
    })),
    rect.left,
    rect.bottom + 4,
  );
});

$("#settings-btn").addEventListener("click", () => {
  ($("#settings-alias") as HTMLInputElement).value = myAlias;
  ($("#settings-status") as HTMLSelectElement).value = myStatus;
  ($("#settings-lang") as HTMLSelectElement).value = lang();
  void fillAbout();
  ($("#settings-notifications") as HTMLInputElement).checked = notificationsEnabled();
  ($("#settings-sound") as HTMLInputElement).checked = soundEnabled();
  $("#settings-error").textContent = "";
  // The key is only revealed on demand, never just by opening settings.
  $("#recovery-code").textContent = HIDDEN_KEY;
  $("#recovery-code").classList.remove("recovery-missing");
  $("#recovery-show").textContent = t("recovery.show");
  recoveryShown = false;
  $("#settings").classList.remove("hidden");
});

$("#settings-close").addEventListener("click", () =>
  $("#settings").classList.add("hidden"),
);

/// Fills the "about" block. The encryption details live here rather than
/// being repeated across the interface.
async function fillAbout() {
  $("#about-version").textContent = await getVersion().catch(() => "—");
  $("#about-account").textContent = me ? `${myAlias} (@${me})` : "—";
  const server = SERVERS.find((s) => s.url === myServer);
  $("#about-server").textContent = server ? server.name : myServer || "—";
  $("#about-encryption").textContent = "PQXDH · X25519 + Kyber-1024 · Double Ratchet";
}

// Alert switches apply immediately; the sound one previews itself so the
// user hears what they just turned on.
$("#settings-notifications").addEventListener("change", (e) => {
  setNotificationsEnabled((e.target as HTMLInputElement).checked);
});

$("#settings-sound").addEventListener("change", (e) => {
  const on = (e.target as HTMLInputElement).checked;
  setSoundEnabled(on);
  if (on) playChime();
});

// ---- Clave de recuperación ----

const HIDDEN_KEY = "••••••••••••••••";
let recoveryShown = false;

async function recoveryCode(): Promise<string> {
  return invoke<string>("get_recovery_code");
}

$("#recovery-show").addEventListener("click", async () => {
  const label = $("#recovery-code");
  if (recoveryShown) {
    label.textContent = HIDDEN_KEY;
    $("#recovery-show").textContent = t("recovery.show");
    recoveryShown = false;
    return;
  }
  try {
    label.textContent = await recoveryCode();
    label.classList.remove("recovery-missing");
    $("#recovery-show").textContent = t("recovery.hide");
    recoveryShown = true;
  } catch (err) {
    // Typically `no_recovery_key`: this device has not adopted the account's
    // key yet, which the restore box right below fixes.
    label.textContent = tError(err);
    label.classList.add("recovery-missing");
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
    await refreshContacts();
    if (current) await selectContact(current);
    toast(count > 0 ? t("restore.done").replace("{n}", String(count)) : t("restore.empty"));
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
    // A hand-picked status ends any automatic absence.
    autoAway = false;
    lastActivity = Date.now();
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
  // Carry what was already typed across, so switching forms is not a retype.
  const [from, to] = register
    ? [["#signin-username-input", "#username-input"], ["#signin-password-input", "#password-input"]]
    : [["#username-input", "#signin-username-input"], ["#password-input", "#signin-password-input"]];
  for (const [src, dst] of [from, to]) {
    const value = ($(src) as HTMLInputElement).value;
    if (value) ($(dst) as HTMLInputElement).value = value;
  }
  // The server is a per-form choice too; keep the same one selected.
  const [serverFrom, serverTo] = register
    ? ["#signin-server-input", "#server-input"]
    : ["#server-input", "#signin-server-input"];
  ($(serverTo) as HTMLSelectElement).value = ($(serverFrom) as HTMLSelectElement).value;
  ($(serverTo) as HTMLSelectElement).dispatchEvent(new Event("change"));

  $("#login-form").classList.toggle("hidden", !register);
  $("#signin-form").classList.toggle("hidden", register);
  // Land on the first field still empty.
  const focusOn = register
    ? ["#username-input", "#alias-input", "#email-input", "#password-input"]
    : ["#signin-username-input", "#signin-password-input"];
  const target = focusOn.find((id) => !($(id) as HTMLInputElement).value) ?? focusOn[0];
  $(target).focus();
}

$("#to-register").addEventListener("click", () => setAuthMode(true));
$("#to-signin").addEventListener("click", () => setAuthMode(false));

// ---- Recuperar contraseña ----

/// False while asking for the code, true once it has been sent.
let resetCodeSent = false;

$("#to-forgot").addEventListener("click", () => {
  resetCodeSent = false;
  $("#forgot-step2").classList.add("hidden");
  $("#forgot-btn").textContent = t("forgot.send");
  $("#forgot-error").textContent = "";
  ($("#forgot-server-input") as HTMLSelectElement).value = (
    $("#signin-server-input") as HTMLSelectElement
  ).value;
  ($("#forgot-server-input") as HTMLSelectElement).dispatchEvent(new Event("change"));
  ($("#forgot-username-input") as HTMLInputElement).value = (
    $("#signin-username-input") as HTMLInputElement
  ).value;
  show("forgot");
  $("#forgot-username-input").focus();
});

$("#forgot-back").addEventListener("click", () => show("login"));

$("#forgot-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const btn = $("#forgot-btn") as HTMLButtonElement;
  const server = ($("#forgot-server-input") as HTMLSelectElement).value;
  const username = ($("#forgot-username-input") as HTMLInputElement).value.trim().toLowerCase();
  if (!username) return;
  btn.disabled = true;
  $("#forgot-error").textContent = "";
  try {
    if (!resetCodeSent) {
      const devCode = await invoke<string | null>("forgot_password", { server, username });
      resetCodeSent = true;
      $("#forgot-step2").classList.remove("hidden");
      btn.textContent = t("forgot.reset");
      if (devCode) ($("#forgot-code-input") as HTMLInputElement).value = devCode;
      $("#forgot-code-input").focus();
      toast(t("forgot.sent"));
    } else {
      const code = ($("#forgot-code-input") as HTMLInputElement).value.trim();
      const password = ($("#forgot-password-input") as HTMLInputElement).value;
      await invoke("reset_password", { server, username, code, password });
      // Prefill the sign-in form with the new credentials.
      ($("#signin-username-input") as HTMLInputElement).value = username;
      ($("#signin-password-input") as HTMLInputElement).value = password;
      show("login");
      toast(t("forgot.done"));
    }
  } catch (err) {
    $("#forgot-error").textContent = tError(err);
  } finally {
    btn.disabled = false;
  }
});

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
    const state = await invoke<string>("request_contact", { username: name });
    await refreshContacts();
    toast(state === "accepted" ? t("contacts.nowContacts") : t("contacts.requestSent"));
    if (state === "accepted") selectContact(name);
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
      state: stored.state,
      delivered_at: stored.delivered_at,
      read_at: stored.read_at,
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
    addMessage(payload.sender, {
      ...payload,
      state: "delivered",
      delivered_at: Date.now(),
      read_at: null,
      mine: false,
    });
    // Reading it right away if that chat is open on screen; otherwise alert,
    // since the user is not looking at it.
    if (isWatching(payload.sender)) {
      reportRead(payload.sender);
    } else {
      const who = contactLabel(payload.sender);
      const preview =
        payload.kind === "image" ? t("notify.image") : payload.text.slice(0, 120);
      void alertUser(who, preview);
    }
  },
);

listen<{ id: string; state: string; at: number }>("hush://receipt", ({ payload }) => {
  for (const contact of contacts.values()) {
    const msg = contact.messages.find((m) => m.id === payload.id);
    if (!msg) continue;
    msg.state = payload.state;
    if (payload.state === "read") {
      msg.read_at = payload.at;
      // A read message was obviously delivered, even if that notice was lost.
      msg.delivered_at ??= payload.at;
    } else {
      msg.delivered_at = payload.at;
    }
    renderMessages();
    break;
  }
});

listen("hush://contacts", async () => {
  const before = new Set(
    [...contacts.entries()].filter(([, c]) => c.state === "incoming").map(([n]) => n),
  );
  await refreshContacts();
  for (const [name, contact] of contacts) {
    if (contact.state === "incoming" && !before.has(name)) {
      void alertUser(t("notify.requestTitle"), t("notify.requestBody").replace("{who}", contactLabel(name)));
    }
  }
});
listen("hush://disconnected", () => {
  toast(t("error.disconnected"));
  onDisconnected();
});

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
      state: stored.state,
      delivered_at: stored.delivered_at,
      read_at: stored.read_at,
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

// ---- Información de un mensaje ----

function formatMoment(timestamp: number | null | undefined): string {
  if (!timestamp) return "—";
  return new Date(timestamp).toLocaleString(lang(), {
    dateStyle: "medium",
    timeStyle: "short",
  });
}

function showMessageInfo(id: string) {
  const msg = current
    ? contacts.get(current)?.messages.find((m) => m.id === id)
    : undefined;
  if (!msg) return;

  const body = $("#info-body");
  body.replaceChildren();
  // Receipts only exist for what we sent; for received messages the useful
  // facts are when it was sent and when it got here.
  const rows: [string, string][] = msg.mine
    ? [
        ["info.sent", formatMoment(msg.created_at)],
        ["info.delivered", formatMoment(msg.delivered_at)],
        ["info.read", formatMoment(msg.read_at)],
      ]
    : [
        ["info.sentBy", contactLabel(msg.sender)],
        ["info.received", formatMoment(msg.created_at)],
      ];
  for (const [key, value] of rows) {
    const term = document.createElement("dt");
    term.textContent = t(key);
    const def = document.createElement("dd");
    def.textContent = value;
    body.append(term, def);
  }
  $("#msg-info").classList.remove("hidden");
}

$("#info-close").addEventListener("click", () =>
  $("#msg-info").classList.add("hidden"),
);

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
    $("#msg-info").classList.add("hidden");
  }
});

// ---- Menú contextual propio ----
// The browser menu is suppressed everywhere; the app shows its own with
// actions relevant to what was clicked. Shift+right-click bypasses it.

interface CtxItem {
  label: string;
  action: () => void;
  /// Optional coloured dot, used by the status picker.
  dot?: string;
  checked?: boolean;
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
    if (item.dot) {
      const dot = document.createElement("span");
      dot.className = `dot status-${item.dot}`;
      btn.appendChild(dot);
    }
    btn.appendChild(document.createTextNode(item.label));
    if (item.checked) {
      const tick = document.createElement("span");
      tick.className = "ctx-check";
      tick.textContent = "✓";
      btn.appendChild(tick);
    }
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
    const id = bubble.dataset.id ?? "";
    const items: CtxItem[] = bubble.classList.contains("image")
      ? [
          { label: t("ctx.view"), action: () => openLightbox(bubble.querySelector("img")!.src) },
          {
            label: t("ctx.copyImage"),
            action: () => copyImageToClipboard(bubble.querySelector("img")!.src),
          },
        ]
      : [{ label: t("ctx.copyText"), action: () => copyText(bubble.textContent ?? "") }];
    items.push({ label: t("ctx.info"), action: () => showMessageInfo(id) });
    showContextMenu(items, e.clientX, e.clientY);
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
