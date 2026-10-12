import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: 'dist',
  },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
    passWithNoTests: true,
    // Vitest disables CSS processing by default and its `?raw` detection is
    // not query-aware, so `tokens.css?raw` gets emptied out before Vite's
    // native raw loader runs. Enabling `css` restores the native loader.
    css: true,
  },
});
