// Desktop notifications and alert sound, both switchable from settings.
//
// The chime is synthesised with the Web Audio API rather than shipped as an
// audio file: no asset to bundle, and nothing for the strict CSP to block.

import { invoke } from "@tauri-apps/api/core";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

const NOTIFY_KEY = "hush-notifications";
const SOUND_KEY = "hush-sound";
const ALERT_KEY = "hush-alert-mode";

/// How a phone should announce a message. Sound and vibration are left to
/// Android through the notification, which is what makes a phone on silent
/// stay silent; a tone synthesised here would ignore that entirely.
export type AlertMode = "sound" | "vibrate" | "none";

const IS_MOBILE = /android/i.test(navigator.userAgent);

export function alertMode(): AlertMode {
  const stored = localStorage.getItem(ALERT_KEY);
  return stored === "vibrate" || stored === "none" ? stored : "sound";
}

export function setAlertMode(mode: AlertMode) {
  localStorage.setItem(ALERT_KEY, mode);
  void invoke("set_alert_mode", { value: mode }).catch(() => {});
}

/// Tells the Rust side the current choice: a message arriving while the app is
/// in the background is announced from there.
export function publishAlertMode() {
  void invoke("set_alert_mode", { value: alertMode() }).catch(() => {});
}

export function notificationsEnabled(): boolean {
  return localStorage.getItem(NOTIFY_KEY) !== "off";
}

export function soundEnabled(): boolean {
  // On a phone the choice is the three-way alert mode instead.
  if (IS_MOBILE) return alertMode() === "sound";
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

/// Asks for the notification permission up front.
///
/// On Android 13 and later nothing is shown without it, and the messages that
/// most need a notification arrive while the app is in the background, where
/// there is no good moment to ask.
export async function requestNotificationPermission(): Promise<void> {
  await ensurePermission();
}

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
///
/// Desktop only: a phone announces messages through the system notification,
/// which obeys the ringer and Do Not Disturb. Synthesising a tone here would
/// play it with the phone on silent, which is exactly what nobody wants.
export function playChime() {
  if (IS_MOBILE || !soundEnabled()) return;
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
///
/// It goes through our own command so the notification is attributed to Hush.
/// The plugin is the fallback: it registers the app identity only for an
/// installed build, so on Windows it can label the toast as PowerShell.
export async function notify(title: string, body: string) {
  if (!notificationsEnabled()) return;
  try {
    await invoke("notify", { title, body });
    return;
  } catch {
    // Fall through to the plugin.
  }
  if (!(await ensurePermission())) return;
  try {
    sendNotification({ title, body });
  } catch {
    // Notifications unavailable on this system; not worth surfacing.
  }
}

/// Alerts about something that happened while the user was away.
///
/// On a phone that is the notification alone, with its sound and vibration
/// decided by the channel the alert mode picks. On a desktop it is the
/// notification plus the chime, each with its own setting.
export async function alertUser(title: string, body: string) {
  if (IS_MOBILE && alertMode() === "none") {
    // Still worth showing, just without announcing itself.
    await notify(title, body);
    return;
  }
  playChime();
  await notify(title, body);
}
