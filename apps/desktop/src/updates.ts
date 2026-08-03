// Self-update: the server publishes the newest installer, the app checks on
// start and offers to install it.
//
// Nothing is trusted on the server's word: the downloaded installer is
// verified against the public key built into this app, so a server that has
// been tampered with cannot push anything the developer did not sign.

import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

import { t } from "./i18n";

/// Asks about a pending update and installs it if the user agrees.
/// `confirm` shows the question; it resolves to what the user chose.
export async function offerUpdate(
  confirm: (version: string, notes: string) => Promise<boolean>,
  progress: (message: string) => void,
): Promise<void> {
  let update: Update | null = null;
  try {
    update = await check();
  } catch (e) {
    // An old server without the endpoint, or no connection: not worth
    // bothering the user about.
    console.warn("update check failed", e);
    return;
  }
  if (!update) return;

  if (!(await confirm(update.version, update.body ?? ""))) return;

  try {
    progress(t("update.downloading"));
    await update.downloadAndInstall();
    progress(t("update.restarting"));
    await relaunch();
  } catch (e) {
    progress(`${t("update.failed")}: ${e}`);
  }
}
