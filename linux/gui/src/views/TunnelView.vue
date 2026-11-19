<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import StreamField from "../components/StreamField.vue";
import {
  app,
  connect,
  disconnect,
  fail,
  formatBytes,
  formatDuration,
  isActive,
  loadProfiles,
  notify,
} from "../store";

const now = ref(Date.now());
let timer: number | undefined;
onMounted(() => (timer = window.setInterval(() => (now.value = Date.now()), 1000)));
onUnmounted(() => window.clearInterval(timer));

const status = computed(() => app.status);
const state = computed(() => status.value?.state ?? "disconnected");
const active = computed(() => isActive(status.value));
const profile = computed(() => app.profiles.find((item) => item.name === app.selected) ?? null);

const headline = computed(() => {
  if (!app.online) return "Служба не отвечает";
  if (app.profiles.length === 0 && !active.value) return "Сервер не добавлен";
  switch (state.value) {
    case "connecting":
      return "Подключение";
    case "connected":
      return "Подключено";
    case "reconnecting":
      return "Восстановление связи";
    case "disconnecting":
      return "Отключение";
    case "failed":
      return "Не подключено";
    default:
      return "Отключено";
  }
});

const where = computed(() => {
  // Имя сервера показываем, только если оно не повторяет адрес.
  const label = (peer: string, name: string | null, host: string) =>
    name && name !== host ? `${peer}, сервер «${name}»` : peer;
  if (active.value && status.value?.peer) {
    const peer = status.value.peer;
    return label(peer, status.value.profile, peer.replace(/:\d+$/, ""));
  }
  if (profile.value) {
    const peer = `${profile.value.host}:${profile.value.port}`;
    return label(peer, profile.value.name, profile.value.host);
  }
  return "";
});

function normalizeWorkers(requested: number): number {
  if (requested <= 5) return 5;
  return Math.floor(Math.min(100, Math.max(10, requested)) / 10) * 10;
}

const total = computed(() => {
  if (active.value && status.value) {
    return Math.max(status.value.workers_target, status.value.active_workers, 5);
  }
  return normalizeWorkers(profile.value?.workers ?? 10);
});

const fieldMode = computed(() => {
  if (!app.online || !active.value) return "idle";
  if (state.value === "connected") return "live";
  if (state.value === "reconnecting") return "fault";
  return "opening";
});

const uptime = computed(() => {
  const since = status.value?.connected_since;
  if (!since || state.value !== "connected") return "";
  return formatDuration(Math.max(0, Math.floor(now.value / 1000) - since));
});

const link = ref("");
const importing = ref(false);

async function addFromLink() {
  if (!link.value.trim()) return;
  importing.value = true;
  app.error = "";
  try {
    const saved = await invoke<{ name: string }>("import_link", { link: link.value, name: null });
    link.value = "";
    await loadProfiles();
    app.selected = saved.name;
    notify(`Сервер «${saved.name}» добавлен`);
  } catch (error) {
    fail(error);
  } finally {
    importing.value = false;
  }
}

function editSelected() {
  app.editing = app.selected;
  app.view = "profiles";
}

const needsHashes = computed(
  () => !!profile.value && profile.value.hashes.length === 0 && !profile.value.vk_token,
);
</script>

<template>
  <main class="view">
    <div class="stack">
      <header class="head">
        <h1 class="state" :data-state="app.online ? state : 'offline'">{{ headline }}</h1>
        <p v-if="where" class="quiet selectable">{{ where }}</p>
      </header>

      <template v-if="!app.online">
        <div class="note">
          <strong>Окно не может связаться со службой csqttd</strong>
          <p class="small selectable">{{ app.offlineReason }}</p>
        </div>
        <p class="quiet small">
          Туннелем управляет системная служба. Запусти её и окно подключится само:
          <code>sudo systemctl start csqttd</code>
        </p>
      </template>

      <template v-else-if="app.profiles.length === 0 && !active">
        <p>Вставь ссылку <code>csqtt://</code> — ту же, что в приложении на телефоне.</p>
        <form class="row" @submit.prevent="addFromLink">
          <input
            v-model="link"
            class="input grow"
            placeholder="csqtt://…"
            aria-label="Ссылка на сервер"
            spellcheck="false"
          />
          <button class="button" type="submit" :disabled="importing || !link.trim()">
            Добавить сервер
          </button>
        </form>
        <div v-if="app.error" class="note note--fault selectable">{{ app.error }}</div>
      </template>

      <template v-else>
        <section class="streams">
          <StreamField :total="total" :active="status?.active_workers ?? 0" :mode="fieldMode" />
          <p class="small quiet">
            <template v-if="active">
              Потоков активно: {{ status?.active_workers ?? 0 }} из {{ total }}
            </template>
            <template v-else>Потоков по настройке сервера: {{ total }}</template>
          </p>
        </section>

        <dl v-if="state === 'connected' || state === 'reconnecting'" class="facts">
          <div>
            <dt>Получено</dt>
            <dd>{{ formatBytes(status?.bytes_down ?? 0) }}</dd>
          </div>
          <div>
            <dt>Отправлено</dt>
            <dd>{{ formatBytes(status?.bytes_up ?? 0) }}</dd>
          </div>
          <div v-if="status?.tunnel_ip">
            <dt>Адрес в туннеле</dt>
            <dd class="selectable">{{ status.tunnel_ip }}</dd>
          </div>
          <div v-if="uptime">
            <dt>На связи</dt>
            <dd>{{ uptime }}</dd>
          </div>
        </dl>

        <div v-if="status?.captcha" class="note note--wait">
          <strong>VK просит пройти проверку</strong>
          <span class="small">Окно с проверкой уже открыто — пройди её, и подключение продолжится.</span>
        </div>
        <div v-if="state === 'reconnecting'" class="note note--wait">
          <strong>Связь с сервером прервалась</strong>
          <span class="small">
            Пока она восстанавливается, трафик не идёт мимо туннеля. Чтобы вернуть обычный
            интернет, нажми «Отключить».
          </span>
        </div>
        <div v-if="status?.dead_hashes.length" class="note note--wait">
          <strong>Звонков VK завершено: {{ status.dead_hashes.length }}</strong>
          <span class="small">
            Их хеши больше не работают.
            <button class="link" type="button" @click="editSelected">Заменить хеши</button>
          </span>
        </div>
        <div v-if="status?.diagnosis" class="note note--wait">
          <strong>Похоже, в чём дело</strong>
          <span class="small selectable">{{ status.diagnosis }}</span>
        </div>
        <div v-if="state === 'failed' && status?.last_error" class="note note--fault">
          <strong>Подключиться не удалось</strong>
          <span class="small selectable">{{ status.last_error }}</span>
        </div>
        <div v-if="app.error" class="note note--fault selectable">{{ app.error }}</div>
        <div v-if="!active && needsHashes" class="note note--wait">
          <strong>У сервера нет хешей VK-звонков</strong>
          <span class="small">
            Без них подключиться нельзя.
            <button class="link" type="button" @click="editSelected">Добавить хеши</button>
          </span>
        </div>

        <button
          v-if="active"
          class="button button--main button--stop"
          type="button"
          :disabled="app.pending || state === 'disconnecting'"
          @click="disconnect"
        >
          {{ state === "connected" || state === "reconnecting" ? "Отключить" : "Отменить" }}
        </button>
        <button
          v-else
          class="button button--main"
          type="button"
          :disabled="app.pending || needsHashes"
          @click="connect"
        >
          Подключить
        </button>

        <label v-if="!active && app.profiles.length > 1" class="field">
          <span>Сервер</span>
          <select v-model="app.selected" class="input">
            <option v-for="item in app.profiles" :key="item.name" :value="item.name">
              {{ item.name }} — {{ item.host }}
            </option>
          </select>
        </label>
      </template>
    </div>
  </main>
</template>

<style scoped>
.head {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.state {
  font-size: var(--step-3);
  line-height: 1.1;
  font-weight: 650;
  letter-spacing: -0.025em;
}

.state[data-state="connected"] {
  color: var(--line);
}

.state[data-state="reconnecting"],
.state[data-state="connecting"] {
  color: var(--wait);
}

.state[data-state="offline"],
.state[data-state="failed"] {
  color: var(--fault);
}

.streams {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 4px 0 2px;
}

.facts {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 14px 20px;
}

.facts dt {
  font-size: var(--step--1);
  color: var(--muted);
}

.facts dd {
  font-size: var(--step-1);
  font-weight: 550;
}

.note .small {
  display: block;
  margin-top: 2px;
}
</style>
