<script setup lang="ts">
import { nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { app, init, type View } from "./store";
import TunnelView from "./views/TunnelView.vue";
import ProfilesView from "./views/ProfilesView.vue";
import LogView from "./views/LogView.vue";
import AppsView from "./views/AppsView.vue";
import SettingsView from "./views/SettingsView.vue";
import UpdateBanner from "./components/UpdateBanner.vue";

// «Настройки» — значком справа: при крупном системном шрифте и узком окне
// текстовые вкладки прокручиваются, а настройки остаются видны всегда.
const tabs: { id: View; label: string }[] = [
  { id: "tunnel", label: "Туннель" },
  { id: "profiles", label: "Серверы" },
  { id: "apps", label: "Программы" },
  { id: "log", label: "Журнал" },
];

const strip = ref<HTMLElement | null>(null);
// Есть ли вкладки за краем полосы — тогда край затеняется.
const hiddenStart = ref(false);
const hiddenEnd = ref(false);

function updateEdges() {
  const element = strip.value;
  if (!element) return;
  hiddenStart.value = element.scrollLeft > 1;
  hiddenEnd.value = element.scrollLeft + element.clientWidth < element.scrollWidth - 1;
}

let resize: ResizeObserver | undefined;
onMounted(() => {
  if (!strip.value) return;
  resize = new ResizeObserver(updateEdges);
  resize.observe(strip.value);
  updateEdges();
});
onUnmounted(() => resize?.disconnect());

// Выбранная вкладка всегда видна целиком, даже если полоса прокручена.
watch(
  () => app.view,
  async () => {
    await nextTick();
    strip.value
      ?.querySelector<HTMLElement>('[aria-current="page"]')
      ?.scrollIntoView({ block: "nearest", inline: "nearest" });
  },
);

// Колесо мыши прокручивает полосу вкладок по горизонтали.
function scrollTabs(event: WheelEvent) {
  const element = strip.value;
  if (!element || element.scrollWidth <= element.clientWidth) return;
  if (Math.abs(event.deltaY) > Math.abs(event.deltaX)) {
    element.scrollLeft += event.deltaY;
    event.preventDefault();
  }
}

onMounted(() => void init());
</script>

<template>
  <div class="shell">
    <nav class="tabs" aria-label="Разделы">
      <div
        ref="strip"
        class="tab-strip"
        :class="{ 'fade-start': hiddenStart, 'fade-end': hiddenEnd }"
        @wheel="scrollTabs"
        @scroll="updateEdges"
      >
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
      </div>
      <button
        class="tab tab--icon"
        type="button"
        title="Настройки"
        aria-label="Настройки"
        :aria-current="app.view === 'settings' ? 'page' : undefined"
        @click="app.view = 'settings'"
      >
        <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
          <path
            fill="none"
            stroke="currentColor"
            stroke-width="1.7"
            stroke-linecap="round"
            stroke-linejoin="round"
            d="M12 15.2a3.2 3.2 0 1 0 0-6.4 3.2 3.2 0 0 0 0 6.4Z M19.4 13.5l1.6 1.2-1.8 3.1-1.9-.7a7.6 7.6 0 0 1-1.9 1.1L15.1 20h-3.6l-.3-1.8a7.6 7.6 0 0 1-1.9-1.1l-1.9.7-1.8-3.1 1.6-1.2a7.7 7.7 0 0 1 0-2.2L5.6 10l1.8-3.1 1.9.7a7.6 7.6 0 0 1 1.9-1.1l.3-2h3.6l.3 2a7.6 7.6 0 0 1 1.9 1.1l1.9-.7 1.8 3.1-1.6 1.3a7.7 7.7 0 0 1 0 2.2Z"
          />
        </svg>
      </button>
    </nav>
    <UpdateBanner />
    <TunnelView v-if="app.view === 'tunnel'" />
    <ProfilesView v-else-if="app.view === 'profiles'" />
    <AppsView v-else-if="app.view === 'apps'" />
    <LogView v-else-if="app.view === 'log'" />
    <SettingsView v-else />
    <div v-if="app.notice" class="toast" role="status">{{ app.notice }}</div>
  </div>
</template>
