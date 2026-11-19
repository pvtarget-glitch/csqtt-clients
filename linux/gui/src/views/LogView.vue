<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { app, copyText } from "../store";

const VISIBLE = 1500;
const filter = ref("");
const follow = ref(true);
const list = ref<HTMLElement | null>(null);

const filtered = computed(() => {
  const needle = filter.value.trim().toLowerCase();
  const lines = needle
    ? app.logs.filter((entry) => entry.line.toLowerCase().includes(needle))
    : app.logs;
  return lines.slice(-VISIBLE);
});

const hidden = computed(() => {
  const needle = filter.value.trim();
  return needle ? 0 : Math.max(0, app.logs.length - VISIBLE);
});

function time(ts: number): string {
  return new Date(ts).toLocaleTimeString("ru-RU", { hour12: false });
}

function tone(line: string): string {
  if (/FATAL|ошибк|не состоял|таймаут|не удалось|timeout|refused/i.test(line)) return "fault";
  if (line.startsWith("[csqttd]")) return "host";
  return "";
}

watch(
  () => [filtered.value.length, app.view],
  async () => {
    if (!follow.value) return;
    await nextTick();
    if (list.value) list.value.scrollTop = list.value.scrollHeight;
  },
  { immediate: true },
);

function copyAll() {
  const text = filtered.value.map((entry) => `${time(entry.ts)} ${entry.line}`).join("\n");
  void copyText(text, "Журнал скопирован");
}
</script>

<template>
  <main class="view view--flush">
    <div class="bar">
      <input
        v-model="filter"
        class="input grow"
        placeholder="Найти в журнале"
        aria-label="Найти в журнале"
        spellcheck="false"
      />
      <button class="button" type="button" :disabled="!filtered.length" @click="copyAll">
        Скопировать
      </button>
    </div>
    <label class="check follow small">
      <input v-model="follow" type="checkbox" />
      <span>Показывать новые строки</span>
    </label>
    <div ref="list" class="lines selectable">
      <p v-if="!filtered.length" class="empty quiet">
        {{
          filter
            ? "Ничего не найдено."
            : "Журнал пуст. Он заполняется с момента подключения."
        }}
      </p>
      <p v-if="hidden" class="empty quiet">Показаны последние {{ VISIBLE }} строк из {{ app.logs.length }}.</p>
      <div v-for="entry in filtered" :key="entry.seq + ':' + entry.ts" class="line" :data-tone="tone(entry.line)">
        <time>{{ time(entry.ts) }}</time>
        <span>{{ entry.line }}</span>
      </div>
    </div>
  </main>
</template>

<style scoped>
.bar {
  display: flex;
  gap: 10px;
  padding: 14px var(--gap) 8px;
}

.follow {
  padding: 0 var(--gap) 10px;
  color: var(--muted);
}

.lines {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  border-top: 1px solid var(--rule);
  background: var(--raised);
  padding: 10px var(--gap) 16px;
  font-family: var(--mono);
  font-size: 0.78rem;
  line-height: 1.55;
}

.line {
  display: grid;
  grid-template-columns: auto 1fr;
  gap: 10px;
}

.line time {
  color: var(--muted);
}

.line span {
  overflow-wrap: anywhere;
}

.line[data-tone="fault"] span {
  color: var(--fault);
}

.line[data-tone="host"] span {
  color: var(--line);
}

.empty {
  font-family: var(--font);
  font-size: var(--step-0);
  padding: 6px 0 10px;
}
</style>
