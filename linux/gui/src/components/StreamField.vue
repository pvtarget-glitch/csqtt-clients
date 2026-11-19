<script setup lang="ts">
import { computed } from "vue";

/**
 * Поле потоков: одна ячейка — один поток туннеля, ряд — группа из десяти
 * (на группу приходится один набор учётных данных VK). Номера потоков
 * демон не сообщает, поэтому ячейки заполняются по счёту, слева направо.
 */
const props = defineProps<{
  total: number;
  active: number;
  mode: "idle" | "opening" | "live" | "fault";
}>();

const cells = computed(() => {
  const total = Math.max(5, Math.min(100, props.total));
  const lit = Math.min(total, props.active);
  return Array.from({ length: total }, (_, index) => index < lit);
});
</script>

<template>
  <div
    class="field-grid"
    :data-mode="mode"
    role="img"
    :aria-label="`Потоков активно: ${Math.min(active, cells.length)} из ${cells.length}`"
  >
    <span
      v-for="(lit, index) in cells"
      :key="index"
      class="cell"
      :class="{ 'cell--lit': lit }"
      :style="{ '--i': index % 10 }"
    />
  </div>
</template>

<style scoped>
.field-grid {
  display: grid;
  grid-template-columns: repeat(10, 1fr);
  gap: 6px;
}

.cell {
  height: 22px;
  border-radius: 4px;
  background: var(--cell);
  transition: background-color 0.35s ease;
}

.cell--lit {
  background: var(--line);
}

.field-grid[data-mode="fault"] .cell--lit,
.field-grid[data-mode="opening"] .cell--lit {
  background: var(--wait);
}

/* Пока линия поднимается, по рядам пробегает волна — единственное
   самостоятельное движение в интерфейсе. */
.field-grid[data-mode="opening"] .cell:not(.cell--lit) {
  animation: sweep 1.6s ease-in-out infinite;
  animation-delay: calc(var(--i) * 70ms);
}

@keyframes sweep {
  0%,
  60%,
  100% {
    opacity: 1;
  }
  30% {
    opacity: 0.35;
  }
}
</style>
