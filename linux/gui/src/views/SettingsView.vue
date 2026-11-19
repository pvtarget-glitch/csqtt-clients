<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { app, fail, notify } from "../store";
import type { AppInfo, Settings } from "../types";

const settings = reactive<Settings>({
  close_to_tray: true,
  start_minimized: true,
  autostart: false,
  tray_available: false,
});
const info = ref<AppInfo | null>(null);
const loaded = ref(false);

onMounted(async () => {
  try {
    Object.assign(settings, await invoke<Settings>("get_settings"));
    info.value = await invoke<AppInfo>("app_info");
  } catch (error) {
    fail(error);
  }
  loaded.value = true;
});

async function save() {
  if (!loaded.value) return;
  try {
    await invoke("save_settings", { settings: { ...settings } });
    notify("Настройки сохранены");
  } catch (error) {
    fail(error);
  }
}
</script>

<template>
  <main class="view">
    <div class="stack">
      <section class="block">
        <h1 class="title">Окно</h1>
        <label class="check">
          <input v-model="settings.autostart" type="checkbox" @change="save" />
          <span>Запускать при входе в систему</span>
        </label>
        <label class="check">
          <input
            v-model="settings.start_minimized"
            type="checkbox"
            :disabled="!settings.autostart || !settings.tray_available"
            @change="save"
          />
          <span>При автозапуске не показывать окно</span>
        </label>
        <label class="check">
          <input
            v-model="settings.close_to_tray"
            type="checkbox"
            :disabled="!settings.tray_available"
            @change="save"
          />
          <span>Закрытие окна сворачивает его в трей</span>
        </label>
        <p v-if="loaded && !settings.tray_available" class="quiet small">
          Значок в трее недоступен: в системе нет библиотеки libayatana-appindicator3.
        </p>
        <p class="quiet small">
          Туннель держит системная служба, поэтому закрытие окна его не разрывает. Но окно
          проверки VK показывает только запущенное приложение.
        </p>
      </section>

      <section class="block">
        <h1 class="title">Служба</h1>
        <dl class="about">
          <div>
            <dt>Состояние</dt>
            <dd>{{ app.online ? "работает" : "не отвечает" }}</dd>
          </div>
          <div v-if="app.status">
            <dt>Версия службы</dt>
            <dd>{{ app.status.daemon_version }}</dd>
          </div>
          <div v-if="info">
            <dt>Версия окна</dt>
            <dd>{{ info.version }}</dd>
          </div>
          <div v-if="app.status?.restarts">
            <dt>Перезапусков ядра</dt>
            <dd>{{ app.status.restarts }}</dd>
          </div>
        </dl>
        <div v-if="app.error" class="note note--fault selectable">{{ app.error }}</div>
        <p v-if="info" class="quiet small selectable">
          Серверы хранятся в файле {{ info.profiles_path }} — он общий с командой
          <code>csqtt</code>. Сеть, DNS и IPv6 настраиваются в /etc/csqtt/csqttd.toml.
        </p>
        <p v-if="info && !info.graphical_session" class="note note--wait small">
          Графическая сессия не найдена: окно проверки VK показать не получится.
        </p>
      </section>
    </div>
  </main>
</template>

<style scoped>
.block {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.block + .block {
  border-top: 1px solid var(--rule);
  padding-top: var(--gap);
}

.about {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px 20px;
}

.about dt {
  font-size: var(--step--1);
  color: var(--muted);
}

.about dd {
  font-weight: 550;
}
</style>
