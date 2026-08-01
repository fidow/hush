import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Message {
  id: string;
  sender: string;
  text: string;
  created_at: number;
  mine: boolean;
}

const conversations = new Map<string, Message[]>();
let me = "";
let current: string | null = null;

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;

function toast(text: string) {
  const el = $("#toast");
  el.textContent = text;
  el.classList.remove("hidden");
  setTimeout(() => el.classList.add("hidden"), 4000);
}

function ensureContact(name: string) {
  if (conversations.has(name)) return;
  conversations.set(name, []);
  const li = document.createElement("li");
  li.dataset.name = name;
  li.textContent = name;
  li.addEventListener("click", () => selectContact(name));
  $("#contact-list").appendChild(li);
}

function selectContact(name: string) {
  current = name;
  document
    .querySelectorAll("#contact-list li")
    .forEach((li) => li.classList.toggle("active", (li as HTMLElement).dataset.name === name));
  $("#conv-header").textContent = `🔒 ${name}`;
  ($("#send-input") as HTMLInputElement).disabled = false;
  ($("#send-btn") as HTMLButtonElement).disabled = false;
  renderMessages();
  $("#send-input").focus();
}

function renderMessages() {
  const container = $("#messages");
  container.replaceChildren();
  if (!current) return;
  for (const msg of conversations.get(current) ?? []) {
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
  conversations.get(contact)!.push(msg);
  if (current === contact) {
    renderMessages();
  } else {
    document
      .querySelector(`#contact-list li[data-name="${contact}"]`)
      ?.classList.add("unread");
  }
}

$("#login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const server = ($("#server-input") as HTMLInputElement).value.trim();
  const username = ($("#username-input") as HTMLInputElement).value.trim();
  const btn = $("#login-btn") as HTMLButtonElement;
  btn.disabled = true;
  $("#login-error").textContent = "";
  try {
    await invoke("register", { server, username });
    me = username;
    $("#me-name").textContent = me;
    $("#login").classList.add("hidden");
    $("#chat").classList.remove("hidden");
  } catch (err) {
    $("#login-error").textContent = String(err);
  } finally {
    btn.disabled = false;
  }
});

$("#add-contact-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const input = $("#add-contact-input") as HTMLInputElement;
  const name = input.value.trim();
  if (name && name !== me) {
    ensureContact(name);
    selectContact(name);
  }
  input.value = "";
});

$("#send-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $("#send-input") as HTMLInputElement;
  const text = input.value.trim();
  if (!text || !current) return;
  const recipient = current;
  input.value = "";
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

listen<string>("hush://error", ({ payload }) => toast(payload));
listen("hush://disconnected", () => toast("Conexión con el servidor perdida"));
