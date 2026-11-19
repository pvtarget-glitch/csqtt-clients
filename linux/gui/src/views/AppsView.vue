<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { app, fail, isActive, notify } from "../store";
import type { AppEntry } from "../types";

const apps = ref<AppEntry[]>([]);
const loaded = ref(false);
const query = ref("");
const busy = ref<string | null>(null);

async function load() {
  try {
    // Программы с ярлыком — наверху. Порядок фиксируется при загрузке, чтобы
    // строка не уезжала из-под курсора при переключении «В меню».
    const list = await invoke<AppEntry[]>("list_apps");
    apps.value = [...list].sort((a, b) => Number(b.launcher) - Number(a.launcher));
  } catch (error) {
    fail(error);
  }
  loaded.value = true;
}

onMounted(load);

const unavailable = computed(() => app.status?.direct_unavailable ?? null);
const tunnelUp = computed(() => isActive(app.status));
const running = computed(() => app.status?.direct_processes ?? 0);

const shown = computed(() => {
  const needle = query.value.trim().toLowerCase();
  if (!needle) return apps.value;
  return apps.value.filter(
    (item) => item.name.toLowerCase().includes(needle) || item.id.toLowerCase().includes(needle),
  );
});

async function launch(item: AppEntry) {
  busy.value = item.id;
  app.error = "";
  try {
    await invoke("launch_direct", { id: item.id });
    notify(`${item.name}: запущено мимо туннеля`);
  } catch (error) {
    fail(error);
  } finally {
    busy.value = null;
  }
}

async function toggleLauncher(item: AppEntry) {
  const enabled = !item.launcher;
  app.error = "";
  try {
    await invoke("set_direct_launcher", { id: item.id, enabled });
    item.launcher = enabled;
    notify(enabled ? `Ярлык «${item.name} (мимо туннеля)» добавлен в меню` : "Ярлык убран из меню");
  } catch (error) {
    fail(error);
  }
}
</script>

<template>
  <main class="view">
    <div class="stack">
      <header class="intro">
        <h1 class="title">Программы мимо туннеля</h1>
        <p class="quiet small">
          Запущенная отсюда программа и всё, что она откроет сама, ходят в интернет напрямую,
          а остальной трафик — через туннель. Если программа уже открыта, сначала закрой её:
          иначе новое окно достанется старому процессу.
        </p>
      </header>

      <div v-if="unavailable" class="note note--fault">
        <strong>Недоступно в этой системе</strong>
        <span class="small selectable">{{ unavailable }}</span>
      </div>
      <p v-else-if="!tunnelUp" class="quiet small">
        Туннель сейчас отключён — программы и так ходят напрямую.
      </p>
      <p v-if="!unavailable" class="small">
        <template v-if="running > 0">Сейчас мимо туннеля процессов: {{ running }}</template>
        <span v-else class="quiet">Сейчас мимо туннеля ничего не запущено</span>
      </p>

      <input
        v-model="query"
        class="input"
        type="search"
        placeholder="Найти программу"
        aria-label="Найти программу"
      />

      <div v-if="app.error" class="note note--fault selectable">{{ app.error }}</div>

      <p v-if="loaded && apps.length === 0" class="quiet small">
        Установленные программы не найдены.
      </p>
      <p v-else-if="loaded && shown.length === 0" class="quiet small">
        Ничего не найдено по запросу «{{ query }}».
      </p>

      <ul class="apps">
        <li v-for="item in shown" :key="item.id" class="app">
          <div class="app-name">
            <span>{{ item.name }}</span>
            <span class="quiet small selectable">{{ item.id }}</span>
          </div>
          <label class="check small menu" :title="'Ярлык «' + item.name + ' (мимо туннеля)» в меню'">
            <input
              type="checkbox"
              :checked="item.launcher"
              :disabled="!!unavailable"
              @change="toggleLauncher(item)"
            />
            <span>В меню</span>
          </label>
          <button
            class="button"
            type="button"
            :disabled="!!unavailable || busy === item.id"
            @click="launch(item)"
          >
            Запустить
          </button>
        </li>
      </ul>
    </div>
  </main>
</template>

<style scoped>
.intro {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.note .small {
  display: block;
  margin-top: 2px;
}

.apps {
  list-style: none;
  margin: 0;
  padding: 0;
}

.app {
  display: grid;
  grid-template-columns: 1fr auto auto;
  align-items: center;
  gap: 14px;
  padding: 10px 0;
  border-bottom: 1px solid var(--rule);
}

.app:first-child {
  border-top: 1px solid var(--rule);
}

.app-name {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.app-name > span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.menu {
  align-items: center;
  white-space: nowrap;
}

.menu input {
  margin-top: 0;
}

.app .button {
  padding: 7px 12px;
}
</style>
