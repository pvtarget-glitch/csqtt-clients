// Формы данных совпадают с csqtt_common::ipc и csqtt_common::profile.

export type TunnelState =
  | "disconnected"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "disconnecting"
  | "failed";

export interface CaptchaRequest {
  id: number;
  mode: string;
  url: string;
  timeout_secs: number;
}

export interface Status {
  state: TunnelState;
  daemon_version: string;
  profile: string | null;
  peer: string | null;
  tunnel_ip: string | null;
  dns: string[];
  interface: string | null;
  active_workers: number;
  workers_target: number;
  bytes_up: number;
  bytes_down: number;
  connected_since: number | null;
  restarts: number;
  last_error: string | null;
  diagnosis: string | null;
  streams: StreamInfo[];
  direct_processes: number;
  tunnel_processes: number;
  selected_only: boolean;
  direct_unavailable: string | null;
  dead_hashes: string[];
  captcha: CaptchaRequest | null;
  captcha_handlers: number;
}

export interface LogLine {
  seq: number;
  ts: number;
  line: string;
}

export type DaemonEvent =
  | { kind: "status"; status: Status }
  | { kind: "log"; entry: LogLine }
  | { kind: "captcha"; request: CaptchaRequest }
  | { kind: "captcha_closed"; id: number };

export type ListMode = "off" | "blacklist" | "whitelist";

export interface Routing {
  domain_mode: ListMode;
  exclude_ru_zones: boolean;
  template_domains: boolean;
  domains: string[];
  ip_mode: ListMode;
  ips: string[];
}

export interface Profile {
  name: string;
  host: string;
  port: number;
  password: string;
  hashes: string[];
  vk_token: string;
  workers: number;
  fingerprint: string;
  client_ids: string;
  obfs: string;
  turn_transport: string;
  srtp_mode: string;
  captcha_mode: string;
  vk_auth_mode: string;
  custom_turn_ips: string;
  routing: Routing;
  persistent: boolean;
  /** Через туннель только программы из вкладки «Программы» / csqtt tunnel. */
  selected_apps_only: boolean;
}

export interface Settings {
  close_to_tray: boolean;
  start_minimized: boolean;
  autostart: boolean;
  tray_available: boolean;
}

export interface AppInfo {
  version: string;
  profiles_path: string;
  socket_path: string;
  graphical_session: boolean;
}

/** Рабочий поток ядра: узел VK и начало работы (мс Unix). */
export interface StreamInfo {
  worker: number;
  server: string;
  since: number;
  bytes_up: number;
  bytes_down: number;
}

/** Установленное приложение из .desktop-файла. */
export interface AppEntry {
  id: string;
  name: string;
  exec: string;
  icon: string | null;
  terminal: boolean;
  categories: string | null;
  /** Есть ярлык «… (мимо туннеля)» в меню. */
  launcher: boolean;
  /** Есть ярлык «… (через туннель)» в меню. */
  tunnel_launcher: boolean;
}
