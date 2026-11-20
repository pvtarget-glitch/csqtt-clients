<script setup lang="ts">
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import ProfileEditor from "../components/ProfileEditor.vue";
import ServerDeploy from "../components/ServerDeploy.vue";
import { app, fail, loadProfiles, notify } from "../store";
import type { Profile } from "../types";

const link = ref("");
const deploying = ref(false);
const busy = ref(false);

function source(profile: Profile): string {
  if (profile.vk_token) {
    return profile.vk_hash_mode === "auto_api" ? "вход в VK, Auto API" : "вход в VK, Auto JS";
  }
  const count = profile.hashes.length;
  if (count === 0) return "хешей нет";
  const mod10 = count % 10;
  const word = count === 1 ? "хеш" : mod10 >= 2 && mod10 <= 4 ? "хеша" : "хешей";
  return `${count} ${word}`;
}

async function addFromLink() {
  if (!link.value.trim()) return;
  busy.value = true;
  app.error = "";
  try {
    const saved = await invoke<Profile>("import_link", { link: link.value, name: null });
    link.value = "";
    await loadProfiles();
    notify(`Сервер «${saved.name}» добавлен`);
  } catch (error) {
    fail(error);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <ProfileEditor v-if="app.editing !== null" :name="app.editing" @close="app.editing = null" />
  <ServerDeploy v-else-if="deploying" @close="deploying = false" />
  <main v-else class="view">
    <div class="stack">
      <form class="field" @submit.prevent="addFromLink">
        <span>Добавить сервер по ссылке</span>
        <div class="row">
          <input
            v-model="link"
            class="input grow"
            placeholder="csqtt://…"
            aria-label="Ссылка на сервер"
            spellcheck="false"
          />
          <button class="button" type="submit" :disabled="busy || !link.trim()">Добавить</button>
        </div>
      </form>
      <div v-if="app.error" class="note note--fault selectable">{{ app.error }}</div>

      <ul v-if="app.profiles.length" class="servers">
        <li v-for="profile in app.profiles" :key="profile.name">
          <button class="server" type="button" @click="app.editing = profile.name">
            <span class="server__name">{{ profile.name }}</span>
            <span class="server__meta quiet small">
              {{ profile.host }}:{{ profile.port }}, {{ source(profile) }}
            </span>
            <span v-if="profile.name === app.defaultProfile" class="server__mark small">
              основной
            </span>
          </button>
        </li>
      </ul>
      <p v-else class="quiet">
        Серверов пока нет. Ссылку <code>csqtt://</code> выдаёт панель сервера или приложение
        на телефоне.
      </p>

      <p class="actions">
        <button class="link" type="button" @click="app.editing = ''">Ввести данные вручную</button>
        <button class="link" type="button" @click="deploying = true">Установить сервер на свой VPS</button>
      </p>
    </div>
  </main>
</template>

<style scoped>
.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px 20px;
}

.servers {
  border-top: 1px solid var(--rule);
}

.server {
  appearance: none;
  width: 100%;
  border: 0;
  border-bottom: 1px solid var(--rule);
  background: none;
  padding: 14px 2px;
  display: grid;
  grid-template-columns: 1fr auto;
  gap: 2px 12px;
  text-align: left;
  cursor: pointer;
}

.server:hover .server__name {
  text-decoration: underline;
  text-underline-offset: 3px;
}

.server__name {
  font-weight: 600;
}

.server__meta {
  grid-column: 1;
}

.server__mark {
  grid-column: 2;
  grid-row: 1 / span 2;
  align-self: center;
  color: var(--line);
  font-weight: 600;
}
</style>
