<script setup lang="ts">
import { computed } from "vue";
import { app, installUpdate, openReleasePage, restartApp } from "../store";

const manual = computed(() => !app.update?.asset || app.update.install === "manual");
const label = computed(() => {
  switch (app.updateStage) {
    case "download":
      return "Скачиваю и проверяю…";
    case "install":
      return "Устанавливаю…";
    default:
      return "Обновить";
  }
});
</script>

<template>
  <div v-if="app.updateStage === 'restart'" class="update" role="status">
    <span>Версия {{ app.update?.latest }} установлена. Перезапусти окно, чтобы перейти на неё.</span>
    <button class="button" type="button" @click="restartApp">Перезапустить</button>
  </div>
  <div v-else-if="app.update" class="update" role="status">
    <span>
      Доступна версия {{ app.update.latest }}.
      <button class="link" type="button" @click="openReleasePage(app.update.page)">
        Что нового
      </button>
    </span>
    <button
      v-if="manual"
      class="button"
      type="button"
      @click="openReleasePage(app.update.page)"
    >
      Скачать
    </button>
    <button
      v-else
      class="button"
      type="button"
      :disabled="app.updateStage !== ''"
      @click="installUpdate"
    >
      {{ label }}
    </button>
    <button
      class="update__close"
      type="button"
      aria-label="Скрыть"
      :disabled="app.updateStage !== ''"
      @click="app.update = null"
    >
      ×
    </button>
  </div>
</template>

<style scoped>
.update {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 12px;
  padding: 8px 16px;
  border-bottom: 1px solid var(--rule);
}

.update span {
  flex: 1 1 auto;
}

.update__close {
  border: 0;
  background: none;
  color: var(--muted);
  font-size: 1.2em;
  line-height: 1;
  cursor: pointer;
}
</style>
