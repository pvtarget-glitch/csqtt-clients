<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { app, copyText, loadProfiles } from "../store";

const emit = defineEmits<{ close: [] }>();

interface AssetsInfo {
  available: boolean;
  version: string | null;
  reason: string | null;
}

interface DeployResult {
  profile: string;
  panel_url: string;
  web_user: string;
  web_password: string;
  link: string;
}

interface ProgressEvent {
  fraction: number | null;
  text: string;
  line: boolean;
}

const form = reactive({
  host: "",
  ssh_port: 22,
  user: "root",
  auth: "password" as "password" | "key" | "agent",
  password: "",
  key_path: "~/.ssh/id_ed25519",
  key_passphrase: "",
  sudo_password: "",
  peer_port: 46000,
  web_port: 46002,
  web_user: "admin",
  docker: false,
  name: "",
});

const assets = ref<AssetsInfo | null>(null);
const running = ref<"install" | "remove" | null>(null);
const fraction = ref(0);
const step = ref("");
const lines = ref<string[]>([]);
const error = ref("");
const result = ref<DeployResult | null>(null);
const removed = ref(false);
const confirmRemove = ref(false);

let unlisten: UnlistenFn | undefined;

onMounted(async () => {
  assets.value = await invoke<AssetsInfo>("server_assets");
  unlisten = await listen<ProgressEvent>("deploy-progress", ({ payload }) => {
    if (payload.line) {
      lines.value.push(payload.text);
      if (lines.value.length > 400) lines.value.splice(0, lines.value.length - 400);
    } else {
      step.value = payload.text;
      if (payload.fraction !== null) fraction.value = payload.fraction;
    }
  });
});

onUnmounted(() => unlisten?.());

// sudo спрашивается, только если пароль SSH для него не подходит.
const needsSudo = computed(() => form.user.trim() !== "root" && form.auth !== "password");

const ready = computed(() => {
  if (!assets.value?.available || !form.host.trim() || !form.user.trim()) return false;
  if (form.auth === "password") return form.password.length > 0;
  if (form.auth === "key") return form.key_path.trim().length > 0;
  return true;
});

function payload() {
  return { ...form, ssh_port: Number(form.ssh_port), peer_port: Number(form.peer_port), web_port: Number(form.web_port) };
}

function reset() {
  error.value = "";
  result.value = null;
  removed.value = false;
  lines.value = [];
  fraction.value = 0;
  step.value = "";
}

async function install() {
  reset();
  running.value = "install";
  try {
    result.value = await invoke<DeployResult>("deploy_install", { form: payload() });
    await loadProfiles();
  } catch (failure) {
    error.value = String(failure);
  } finally {
    running.value = null;
  }
}

async function remove() {
  confirmRemove.value = false;
  reset();
  running.value = "remove";
  try {
    await invoke("deploy_remove", { form: payload() });
    removed.value = true;
  } catch (failure) {
    error.value = String(failure);
  } finally {
    running.value = null;
  }
}

function cancel() {
  void invoke("deploy_cancel");
}

function addHashes() {
  if (!result.value) return;
  app.editing = result.value.profile;
  emit("close");
}
</script>

<template>
  <main class="view">
    <div class="stack">
      <header class="head">
        <button class="link small" type="button" :disabled="!!running" @click="emit('close')">
          Назад к списку
        </button>
        <h1 class="title">Сервер на своём VPS</h1>
        <p class="quiet small">
          Окно подключится к VPS по SSH и поставит сервер csqtt
          {{ assets?.version ?? "" }} штатным установщиком — тем же, что в приложении для
          Android. Подойдёт чистый VPS с Ubuntu или Debian. Если сервер уже стоит, он
          обновится.
        </p>
      </header>

      <div v-if="assets && !assets.available" class="note note--fault">
        <strong>Установка недоступна</strong>
        <span class="small selectable">{{ assets.reason }}</span>
      </div>

      <!-- Готово -->
      <section v-if="result" class="done">
        <div class="note">
          <strong>Сервер установлен</strong>
          <span class="small">
            Профиль «{{ result.profile }}» добавлен. Осталось указать хеши VK-звонков — без
            них подключиться нельзя.
          </span>
        </div>
        <button class="button button--main" type="button" @click="addHashes">
          Добавить хеши
        </button>
        <dl class="facts">
          <div>
            <dt>Панель сервера</dt>
            <dd class="selectable mono">{{ result.panel_url }}</dd>
          </div>
          <div>
            <dt>Логин</dt>
            <dd class="selectable mono">{{ result.web_user }}</dd>
          </div>
          <div>
            <dt>Пароль</dt>
            <dd class="selectable mono">{{ result.web_password }}</dd>
          </div>
        </dl>
        <p class="note note--wait small">
          Сохрани адрес, логин и пароль панели: окно их не запоминает и повторно не покажет.
        </p>
        <div class="row">
          <button
            class="button"
            type="button"
            @click="copyText(`${result.panel_url}\nЛогин: ${result.web_user}\nПароль: ${result.web_password}`, 'Данные панели скопированы')"
          >
            Скопировать данные панели
          </button>
          <button class="button" type="button" @click="copyText(result.link, 'Ссылка скопирована')">
            Скопировать ссылку
          </button>
        </div>
      </section>

      <!-- Форма -->
      <form v-else class="form" @submit.prevent="install">
        <fieldset class="form" :disabled="!!running">
          <label class="field">
            <span>Адрес VPS</span>
            <input v-model="form.host" class="input" placeholder="203.0.113.7" spellcheck="false" autocomplete="off" />
          </label>
          <div class="pair">
            <label class="field">
              <span>Пользователь SSH</span>
              <input v-model="form.user" class="input" spellcheck="false" autocomplete="off" />
            </label>
            <label class="field">
              <span>Порт SSH</span>
              <input v-model.number="form.ssh_port" class="input" type="number" min="1" max="65535" />
            </label>
          </div>

          <div class="field" role="radiogroup" aria-label="Вход на VPS">
            <span>Вход</span>
            <div class="choices">
              <label class="check"><input v-model="form.auth" type="radio" value="password" /><span>Пароль</span></label>
              <label class="check"><input v-model="form.auth" type="radio" value="key" /><span>Ключ из файла</span></label>
              <label class="check"><input v-model="form.auth" type="radio" value="agent" /><span>ssh-agent</span></label>
            </div>
          </div>
          <label v-if="form.auth === 'password'" class="field">
            <span>Пароль SSH</span>
            <input v-model="form.password" class="input" type="password" autocomplete="off" />
          </label>
          <template v-if="form.auth === 'key'">
            <label class="field">
              <span>Файл ключа</span>
              <input v-model="form.key_path" class="input mono" spellcheck="false" />
            </label>
            <label class="field">
              <span>Пароль ключа (если есть)</span>
              <input v-model="form.key_passphrase" class="input" type="password" autocomplete="off" />
            </label>
          </template>
          <label v-if="needsSudo" class="field">
            <span>Пароль sudo для {{ form.user }} (пусто — если sudo без пароля)</span>
            <input v-model="form.sudo_password" class="input" type="password" autocomplete="off" />
          </label>

          <details class="more">
            <summary class="small">Порты, панель, Docker</summary>
            <div class="form">
              <div class="pair">
                <label class="field">
                  <span>UDP-порт сервера</span>
                  <input v-model.number="form.peer_port" class="input" type="number" min="1" max="65535" />
                </label>
                <label class="field">
                  <span>Порт панели</span>
                  <input v-model.number="form.web_port" class="input" type="number" min="1" max="65535" />
                </label>
              </div>
              <label class="field">
                <span>Логин панели</span>
                <input v-model="form.web_user" class="input" spellcheck="false" autocomplete="off" />
              </label>
              <label class="field">
                <span>Имя профиля (по умолчанию — адрес)</span>
                <input v-model="form.name" class="input" spellcheck="false" autocomplete="off" />
              </label>
              <label class="check">
                <input v-model="form.docker" type="checkbox" />
                <span>Установить в Docker вместо службы systemd</span>
              </label>
            </div>
          </details>
        </fieldset>

        <div v-if="running" class="progress" role="status">
          <div class="bar"><div class="bar__fill" :style="{ width: `${Math.round(fraction * 100)}%` }"></div></div>
          <p class="small">{{ step || "Подключение…" }}</p>
        </div>

        <div v-if="removed" class="note">
          <strong>Сервер удалён</strong>
          <span class="small">csqtt удалён с {{ form.host }}.</span>
        </div>
        <div v-if="error" class="note note--fault">
          <strong>{{ running === null && lines.length ? "Не получилось" : "Ошибка" }}</strong>
          <span class="small selectable">{{ error }}</span>
        </div>

        <div class="row">
          <button v-if="!running" class="button button--main" type="submit" :disabled="!ready">
            Установить сервер
          </button>
          <button v-else class="button button--main button--stop" type="button" @click="cancel">
            Отменить
          </button>
        </div>
        <p v-if="!running && !confirmRemove" class="small">
          <button class="link button--danger" type="button" :disabled="!ready" @click="confirmRemove = true">
            Удалить сервер с VPS
          </button>
        </p>
        <div v-if="confirmRemove" class="note note--fault">
          <span class="small">Удалить csqtt с {{ form.host }}? Подключения через этот сервер перестанут работать.</span>
          <div class="row confirm">
            <button class="button button--danger" type="button" @click="remove">Удалить</button>
            <button class="button" type="button" @click="confirmRemove = false">Не удалять</button>
          </div>
        </div>

        <details v-if="lines.length" class="more" :open="!!error">
          <summary class="small">Вывод установщика ({{ lines.length }})</summary>
          <pre class="log selectable">{{ lines.join("\n") }}</pre>
        </details>
      </form>
    </div>
  </main>
</template>

<style scoped>
.head {
  display: flex;
  flex-direction: column;
  gap: 8px;
  align-items: flex-start;
}

.form {
  display: flex;
  flex-direction: column;
  gap: 14px;
  border: 0;
  padding: 0;
  margin: 0;
  min-width: 0;
}

.choices {
  display: flex;
  flex-wrap: wrap;
  gap: 8px 18px;
}

.choices .check {
  align-items: center;
}

.choices .check input {
  margin-top: 0;
}

.more summary {
  cursor: pointer;
  color: var(--muted);
  width: fit-content;
}

.more[open] summary {
  margin-bottom: 12px;
}

.progress {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.bar {
  height: 6px;
  border-radius: 3px;
  background: var(--rule);
  overflow: hidden;
}

.bar__fill {
  height: 100%;
  background: var(--line);
  transition: width 0.3s ease;
}

@media (prefers-reduced-motion: reduce) {
  .bar__fill {
    transition: none;
  }
}

.note strong {
  display: block;
}

.note .small {
  display: block;
  margin-top: 2px;
}

.confirm {
  margin-top: 10px;
}

.log {
  font-family: var(--mono);
  font-size: var(--step--1);
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 260px;
  overflow-y: auto;
  background: var(--raised);
  border: 1px solid var(--rule);
  border-radius: var(--radius);
  padding: 10px;
}

.done {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.done .row {
  flex-wrap: wrap;
}

.done .row .button {
  flex: 1 1 auto;
}

.facts {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.facts dt {
  font-size: var(--step--1);
  color: var(--muted);
}

.mono {
  font-family: var(--mono);
  word-break: break-all;
}
</style>
