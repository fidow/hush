// Desktop notifications and alert sound, both switchable from settings.
//
// The chime is synthesised with the Web Audio API rather than shipped as an
// audio file: no asset to bundle, and nothing for the strict CSP to block.

import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

const NOTIFY_KEY = "hush-notifications";
const SOUND_KEY = "hush-sound";

export function notificationsEnabled(): boolean {
  return localStorage.getItem(NOTIFY_KEY) !== "off";
}

export function soundEnabled(): boolean {
  return localStorage.getItem(SOUND_KEY) !== "off";
}

export function setNotificationsEnabled(on: boolean) {
  localStorage.setItem(NOTIFY_KEY, on ? "on" : "off");
  if (on) void ensurePermission();
}

export function setSoundEnabled(on: boolean) {
  localStorage.setItem(SOUND_KEY, on ? "on" : "off");
}

let permission: boolean | null = null;

async function ensurePermission(): Promise<boolean> {
  if (permission !== null) return permission;
  try {
    permission = (await isPermissionGranted()) || (await requestPermission()) === "granted";
  } catch {
    permission = false;
  }
  return permission;
}

let audio: AudioContext | null = null;

/// Two short descending notes, quiet and quick enough not to grate.
export function playChime() {
  if (!soundEnabled()) return;
  try {
    audio ??= new AudioContext();
    const now = audio.currentTime;
    for (const [index, frequency] of [880, 660].entries()) {
      const start = now + index * 0.11;
      const osc = audio.createOscillator();
      const gain = audio.createGain();
      osc.type = "sine";
      osc.frequency.value = frequency;
      // Fade in and out so the note doesn't click.
      gain.gain.setValueAtTime(0, start);
      gain.gain.linearRampToValueAtTime(0.14, start + 0.01);
      gain.gain.exponentialRampToValueAtTime(0.001, start + 0.18);
      osc.connect(gain).connect(audio.destination);
      osc.start(start);
      osc.stop(start + 0.2);
    }
  } catch {
    // No audio device, or the context was blocked: silence is acceptable.
  }
}

/// Shows a desktop notification if the user allows them.
export async function notify(title: string, body: string) {
  if (!notificationsEnabled()) return;
  if (!(await ensurePermission())) return;
  try {
    sendNotification({ title, body });
  } catch {
    // Notifications unavailable on this system; not worth surfacing.
  }
}

/// Alerts about something that happened while the user was away: a desktop
/// notification plus the chime, each subject to its own setting.
export async function alertUser(title: string, body: string) {
  playChime();
  await notify(title, body);
}
