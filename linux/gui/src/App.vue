<script setup lang="ts">
import { onMounted } from "vue";
import { app, init, type View } from "./store";
import TunnelView from "./views/TunnelView.vue";
import ProfilesView from "./views/ProfilesView.vue";
import LogView from "./views/LogView.vue";
import AppsView from "./views/AppsView.vue";
import SettingsView from "./views/SettingsView.vue";

const tabs: { id: View; label: string }[] = [
  { id: "tunnel", label: "Туннель" },
  { id: "profiles", label: "Серверы" },
  { id: "apps", label: "Программы" },
  { id: "log", label: "Журнал" },
  { id: "settings", label: "Настройки" },
];

onMounted(() => void init());
</script>

<template>
  <div class="shell">
    <nav class="tabs" aria-label="Разделы">
      <button
        v-for="tab in tabs"
        :key="tab.id"
        class="tab"
        type="button"
        :aria-current="app.view === tab.id ? 'page' : undefined"
        @click="app.view = tab.id"
      >
        {{ tab.label }}
      </button>
    </nav>
    <TunnelView v-if="app.view === 'tunnel'" />
    <ProfilesView v-else-if="app.view === 'profiles'" />
    <AppsView v-else-if="app.view === 'apps'" />
    <LogView v-else-if="app.view === 'log'" />
    <SettingsView v-else />
    <div v-if="app.notice" class="toast" role="status">{{ app.notice }}</div>
  </div>
</template>
