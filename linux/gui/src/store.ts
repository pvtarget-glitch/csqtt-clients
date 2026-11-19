import { reactive } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DaemonEvent, LogLine, Profile, Status } from "./types";

const MAX_LOG_LINES = 6000;

export type View = "tunnel" | "profiles" | "log" | "settings";

export const app = reactive({
  view: "tunnel" as View,
  online: false,
  offlineReason: "",
  status: null as Status | null,
  logs: [] as LogLine[],
  profiles: [] as Profile[],
  defaultProfile: null as string | null,
  /** Профиль, выбранный для следующего подключения. */
  selected: null as string | null,
  /** Профиль, открытый в редакторе (имя) или "" для нового. */
  editing: null as string | null,
  pending: false,
  notice: "",
  error: "",
});

let noticeTimer: number | undefined;

export function notify(text: string) {
  app.notice = text;
  window.clearTimeout(noticeTimer);
  noticeTimer = window.setTimeout(() => (app.notice = ""), 2600);
}

/** Ошибки команд приходят строками на русском — показываем как есть. */
export function fail(error: unknown) {
  app.error = typeof error === "string" ? error : String((error as Error)?.message ?? error);
}

export function isActive(status: Status | null): boolean {
  return (
    !!status &&
    ["connecting", "connected", "reconnecting", "disconnecting"].includes(status.state)
  );
}

function appendLog(entry: LogLine) {
  const last = app.logs[app.logs.length - 1];
  // Номера начинаются заново с каждой новой сессией.
  if (last && entry.seq <= last.seq) {
    app.logs = [];
  }
  app.logs.push(entry);
  if (app.logs.length > MAX_LOG_LINES) {
    app.logs.splice(0, app.logs.length - MAX_LOG_LINES);
  }
}

function applyEvent(event: DaemonEvent) {
  if (event.kind === "status") {
    app.status = event.status;
  } else if (event.kind === "log") {
    appendLog(event.entry);
  }
}

export async function loadProfiles() {
  const data = await invoke<{ default: string | null; profiles: Profile[] }>("profiles");
  app.profiles = data.profiles;
  app.defaultProfile = data.default;
  const stillThere = app.profiles.some((profile) => profile.name === app.selected);
  if (!stillThere) {
    app.selected = data.default ?? app.profiles[0]?.name ?? null;
  }
}

async function refresh() {
  const snapshot = await invoke<{ online: boolean; status: Status | null; message: string | null }>(
    "snapshot",
  );
  app.online = snapshot.online;
  app.offlineReason = snapshot.message ?? "";
  app.status = snapshot.status;
  if (snapshot.online) {
    try {
      app.logs = await invoke<LogLine[]>("fetch_logs");
    } catch {
      /* журнал подтянется событиями */
    }
  }
}

export async function init() {
  await listen<{ online: boolean; message: string | null }>("daemon-online", (event) => {
    app.online = event.payload.online;
    app.offlineReason = event.payload.message ?? "";
    if (event.payload.online) {
      void refresh();
    } else {
      app.status = null;
    }
  });
  await listen<DaemonEvent>("daemon-event", (event) => applyEvent(event.payload));
  await Promise.all([refresh(), loadProfiles().catch(fail)]);
}

export async function connect() {
  app.error = "";
  app.pending = true;
  try {
    await invoke("connect", { name: app.selected });
  } catch (error) {
    fail(error);
  } finally {
    app.pending = false;
  }
}

export async function disconnect() {
  app.error = "";
  app.pending = true;
  try {
    await invoke("disconnect");
  } catch (error) {
    fail(error);
  } finally {
    app.pending = false;
  }
}

export async function copyText(text: string, done: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    document.execCommand("copy");
    area.remove();
  }
  notify(done);
}

export function formatBytes(bytes: number): string {
  const units = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${bytes} Б` : `${value.toFixed(1).replace(".", ",")} ${units[unit]}`;
}

export function formatDuration(seconds: number): string {
  const part = (value: number) => String(Math.floor(value)).padStart(2, "0");
  return `${part(seconds / 3600)}:${part((seconds / 60) % 60)}:${part(seconds % 60)}`;
}
