<script setup lang="ts">
import { computed, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { app, copyText, fail, loadProfiles, notify } from "../store";
import type { Profile } from "../types";

const props = defineProps<{ name: string }>();
const emit = defineEmits<{ close: [] }>();

const isNew = props.name === "";

function blank(): Profile {
  return {
    name: "",
    host: "",
    port: 46000,
    password: "",
    hashes: [],
    vk_token: "",
    vk_hash_mode: "auto_api",
    workers: 18,
    fingerprint: "firefox",
    client_ids: "8202606,6287487",
    obfs: "unified",
    turn_transport: "udp",
    srtp_mode: "auto",
    captcha_mode: "auto",
    vk_auth_mode: "vkcalls",
    custom_turn_ips: "",
    routing: {
      domain_mode: "off",
      exclude_ru_zones: false,
      template_domains: false,
      domains: [],
      ip_mode: "off",
      ips: [],
    },
    persistent: false,
    selected_apps_only: false,
  };
}

const original = app.profiles.find((profile) => profile.name === props.name);
const draft = reactive<Profile>(original ? JSON.parse(JSON.stringify(original)) : blank());

// Списки редактируются как текст: по одному значению в строке.
const hashes = ref(draft.hashes.join("\n"));
const domains = ref(draft.routing.domains.join("\n"));
const ips = ref(draft.routing.ips.join("\n"));
const source = ref<"hashes" | "token">(draft.vk_token ? "token" : "hashes");
if (!draft.vk_hash_mode) draft.vk_hash_mode = "auto_js";
const manualToken = ref(false);
const loggingIn = ref(false);
const loginNote = ref("");

async function vkLogin() {
  loggingIn.value = true;
  loginNote.value = "";
  error.value = "";
  try {
    const result = await invoke<{ token: string; expires_in: number }>("vk_login");
    draft.vk_token = result.token;
    loginNote.value =
      result.expires_in === 0
        ? "Вход выполнен. Сохрани сервер, чтобы применить."
        : `VK выдал токен на ${Math.round(result.expires_in / 3600)} ч — потом вход придётся повторить.`;
  } catch (reason) {
    error.value = String(reason);
  } finally {
    loggingIn.value = false;
  }
}
const showPassword = ref(false);
const confirmDelete = ref(false);
const saving = ref(false);
const error = ref("");

const lines = (text: string) =>
  text
    .split(/[\s,]+/)
    .map((item) => item.trim())
    .filter(Boolean);

const hashCount = computed(() => lines(hashes.value).length);
const isDefault = computed(() => !isNew && props.name === app.defaultProfile);

function collect(): Profile {
  const profile: Profile = JSON.parse(JSON.stringify(draft));
  profile.port = Number(profile.port) || 0;
  profile.workers = Number(profile.workers) || 0;
  if (source.value === "token") {
    profile.hashes = [];
    profile.vk_token = profile.vk_token.trim();
    if (profile.vk_hash_mode === "auto_api" && profile.vk_auth_mode === "auto_js") {
      profile.vk_auth_mode = "vkcalls";
    }
  } else {
    profile.hashes = lines(hashes.value);
    profile.vk_token = "";
    if (profile.vk_auth_mode === "auto_js") profile.vk_auth_mode = "vkcalls";
  }
  profile.routing.domains = lines(domains.value);
  profile.routing.ips = lines(ips.value);
  return profile;
}

async function save() {
  saving.value = true;
  error.value = "";
  try {
    const saved = await invoke<Profile>("save_profile", {
      profile: collect(),
      original: isNew ? null : props.name,
    });
    await loadProfiles();
    if (app.selected === props.name || isNew) app.selected = saved.name;
    notify("Сервер сохранён");
    emit("close");
  } catch (reason) {
    error.value = String(reason);
  } finally {
    saving.value = false;
  }
}

async function remove() {
  if (!confirmDelete.value) {
    confirmDelete.value = true;
    return;
  }
  try {
    await invoke("delete_profile", { name: props.name });
    await loadProfiles();
    notify("Сервер удалён");
    emit("close");
  } catch (reason) {
    fail(reason);
  }
}

async function makeDefault() {
  try {
    await invoke("set_default_profile", { name: props.name });
    await loadProfiles();
    notify("Теперь это основной сервер");
  } catch (reason) {
    error.value = String(reason);
  }
}

async function copyLink() {
  try {
    const link = await invoke<string>("export_link", { name: props.name });
    await copyText(link, "Ссылка скопирована");
  } catch (reason) {
    error.value = String(reason);
  }
}
</script>

<template>
  <main class="view">
    <form class="stack" @submit.prevent="save">
      <div class="row">
        <button class="link" type="button" @click="emit('close')">Назад к списку</button>
      </div>
      <h1 class="title">{{ isNew ? "Новый сервер" : draft.name }}</h1>

      <fieldset class="group">
        <legend>Сервер</legend>
        <label class="field">
          <span>Название</span>
          <input v-model="draft.name" class="input" required maxlength="64" spellcheck="false" />
        </label>
        <div class="pair pair--wide">
          <label class="field">
            <span>Адрес</span>
            <input v-model="draft.host" class="input" required spellcheck="false" />
          </label>
          <label class="field">
            <span>Порт</span>
            <input v-model="draft.port" class="input" type="number" min="1" max="65535" required />
          </label>
        </div>
        <label class="field">
          <span>Пароль подключения</span>
          <div class="row">
            <input
              v-model="draft.password"
              class="input grow"
              :type="showPassword ? 'text' : 'password'"
              required
              spellcheck="false"
              autocomplete="off"
            />
            <button class="button" type="button" @click="showPassword = !showPassword">
              {{ showPassword ? "Скрыть" : "Показать" }}
            </button>
          </div>
        </label>
      </fieldset>

      <fieldset class="group">
        <legend>Звонки VK</legend>
        <div class="pair">
          <label class="check">
            <input v-model="source" type="radio" value="hashes" />
            <span>Хеши звонков</span>
          </label>
          <label class="check">
            <input v-model="source" type="radio" value="token" />
            <span>Вход в VK</span>
          </label>
        </div>
        <label v-if="source === 'hashes'" class="field">
          <span>
            По одному в строке, до шести. Можно вставлять ссылки vk.com/call/join/… Сейчас:
            {{ hashCount }}
          </span>
          <textarea v-model="hashes" class="input" rows="4" spellcheck="false" />
        </label>
        <template v-else>
          <p class="hint">
            Звонки создаются в твоём аккаунте VK при подключении и завершаются при
            отключении — чужие ссылки не нужны.
          </p>
          <div class="login-row">
            <button class="button" type="button" :disabled="loggingIn" @click="vkLogin">
              {{ loggingIn ? "Жду вход в окне VK…" : draft.vk_token ? "Войти заново" : "Войти в VK" }}
            </button>
            <span class="login-state">{{ draft.vk_token ? "Токен получен" : "Вход не выполнен" }}</span>
          </div>
          <p v-if="loginNote" class="hint">{{ loginNote }}</p>
          <label class="field">
            <span>Кто создаёт звонки</span>
            <select v-model="draft.vk_hash_mode" class="input">
              <option value="auto_api">Приложение через API VK (Auto API)</option>
              <option value="auto_js">Ядро через аккаунт (Auto JS)</option>
            </select>
          </label>
          <label class="check">
            <input v-model="manualToken" type="checkbox" />
            <span>Ввести токен вручную</span>
          </label>
          <label v-if="manualToken" class="field">
            <span>VK access token</span>
            <input v-model="draft.vk_token" class="input" spellcheck="false" autocomplete="off" />
          </label>
        </template>
      </fieldset>

      <fieldset class="group">
        <legend>Соединение</legend>
        <div class="pair">
          <label class="field">
            <span>Потоков (10–100, шаг 10)</span>
            <input v-model="draft.workers" class="input" type="number" min="5" max="100" />
          </label>
          <label class="field">
            <span>Транспорт до VK</span>
            <select v-model="draft.turn_transport" class="input">
              <option value="udp">UDP</option>
              <option value="tcp_tls">TCP/TLS</option>
            </select>
          </label>
          <label class="field">
            <span>Шифрование потока</span>
            <select v-model="draft.srtp_mode" class="input">
              <option value="auto">Авто</option>
              <option value="dtls-srtp">DTLS-SRTP</option>
              <option value="wrap-srtp">WRAP-SRTP</option>
            </select>
          </label>
          <label class="field">
            <span>Проверка VK</span>
            <select v-model="draft.captcha_mode" class="input">
              <option value="auto">Сначала ядро, затем окно</option>
              <option value="rjs">Только ядро</option>
              <option value="wv">Сразу окно</option>
            </select>
          </label>
          <label class="field">
            <span>Отпечаток браузера</span>
            <select v-model="draft.fingerprint" class="input">
              <option value="firefox">Firefox</option>
              <option value="chrome">Chrome</option>
              <option value="safari">Safari</option>
              <option value="edge">Edge</option>
              <option value="opera">Opera</option>
            </select>
          </label>
          <label class="field">
            <span>Вход в VK</span>
            <select v-model="draft.vk_auth_mode" class="input">
              <option value="vkcalls">VK Calls</option>
              <option value="legacy">Старый способ</option>
              <option
                value="auto_js"
                :disabled="source !== 'token' || draft.vk_hash_mode === 'auto_api'"
              >
                Auto JS (нужен вход и Auto JS для звонков)
              </option>
            </select>
          </label>
        </div>
        <label class="check">
          <input v-model="draft.selected_apps_only" type="checkbox" />
          <span>
            Через туннель только выбранные программы. Остальное, включая DNS системы, идёт
            напрямую; программы для туннеля запускаются на вкладке «Программы».
          </span>
        </label>
        <label class="check">
          <input v-model="draft.persistent" type="checkbox" />
          <span>
            Не сдаваться: переподключаться без ограничения попыток. Для компьютера, который
            включается раньше, чем появляется сеть.
          </span>
        </label>
        <label class="field">
          <span>Свои адреса узлов VK через запятую — пусто, если ядро выбирает само</span>
          <input v-model="draft.custom_turn_ips" class="input" spellcheck="false" />
        </label>
      </fieldset>

      <fieldset class="group">
        <legend>Что идёт мимо туннеля</legend>
        <label class="field">
          <span>Сайты</span>
          <select v-model="draft.routing.domain_mode" class="input">
            <option value="off">Всё через туннель</option>
            <option value="blacklist">Перечисленные — напрямую</option>
            <option value="whitelist">Через туннель только перечисленные</option>
          </select>
        </label>
        <template v-if="draft.routing.domain_mode === 'blacklist'">
          <label class="check">
            <input v-model="draft.routing.exclude_ru_zones" type="checkbox" />
            <span>Зоны .ru, .su и .рф напрямую</span>
          </label>
          <label class="check">
            <input v-model="draft.routing.template_domains" type="checkbox" />
            <span>Готовый список сайтов из поставки csqtt</span>
          </label>
        </template>
        <label v-if="draft.routing.domain_mode !== 'off'" class="field">
          <span>Домены, по одному в строке</span>
          <textarea v-model="domains" class="input" rows="3" spellcheck="false" />
        </label>
        <label class="field">
          <span>Адреса IPv4</span>
          <select v-model="draft.routing.ip_mode" class="input">
            <option value="off">Всё через туннель</option>
            <option value="blacklist">Перечисленные — напрямую</option>
            <option value="whitelist">Через туннель только перечисленные</option>
          </select>
        </label>
        <label v-if="draft.routing.ip_mode !== 'off'" class="field">
          <span>Адреса и подсети, по одному в строке: 192.0.2.0/24</span>
          <textarea v-model="ips" class="input" rows="3" spellcheck="false" />
        </label>
      </fieldset>

      <div v-if="error" class="note note--fault selectable">{{ error }}</div>

      <div class="actions">
        <button class="button button--main" type="submit" :disabled="saving">Сохранить</button>
        <div v-if="!isNew" class="row actions__more">
          <button v-if="!isDefault" class="button" type="button" @click="makeDefault">
            Сделать основным
          </button>
          <button class="button" type="button" @click="copyLink">Скопировать ссылку</button>
          <button class="button button--danger" type="button" @click="remove">
            {{ confirmDelete ? "Точно удалить" : "Удалить" }}
          </button>
        </div>
        <p class="quiet small">Изменения применятся при следующем подключении.</p>
      </div>
    </form>
  </main>
</template>

<style scoped>
.group {
  border: 0;
  border-top: 1px solid var(--rule);
  margin: 0;
  padding: 14px 0 0;
  display: flex;
  flex-direction: column;
  gap: 14px;
  min-width: 0;
}

.group legend {
  padding: 0 10px 0 0;
  font-weight: 600;
}

.pair--wide {
  grid-template-columns: 2fr 1fr;
}

.actions {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.actions__more {
  flex-wrap: wrap;
}

.hint {
  margin: 0;
  color: var(--muted);
}

.login-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
}

.login-state {
  color: var(--muted);
}
</style>
