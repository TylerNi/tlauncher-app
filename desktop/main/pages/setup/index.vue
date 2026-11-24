<template>
  <div
    class="mx-auto flex flex-col items-center gap-y-4 max-w-2xl py-32 sm:py-48 lg:py-56"
  >
    <div>
      <Wordmark />
    </div>
    <div class="text-center">
      <h1
        class="text-balance text-4xl font-bold font-display tracking-tight text-zinc-100 sm:text-6xl"
      >
        Let's get you set up
      </h1>
      <div class="mt-10 flex items-center justify-center gap-x-6">
        <LoadingButton :loading="loading" @click="connect">
          Get started ->
        </LoadingButton>
      </div>
      <p v-if="error" class="mt-6 text-sm text-red-600">{{ error }}</p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";

definePageMeta({
  layout: "mini",
});

const loading = ref(false);
const error = ref(undefined);
const router = useRouter();

async function connect() {
  loading.value = true;
  error.value = undefined;
  try {
    await invoke("use_remote", { url: "https://tlauncher.srvnt.ca" });
    router.push("/auth");
  } catch (e) {
    error.value = e;
  } finally {
    loading.value = false;
  }
}
</script>
