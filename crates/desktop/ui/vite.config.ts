import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Tauri expects a fixed port and does not want Vite to clear its output.
export default defineConfig({
	plugins: [svelte()],
	clearScreen: false,
	server: { port: 1420, strictPort: true, host: '127.0.0.1' },
	build: { outDir: 'dist', emptyOutDir: true, target: 'chrome105' }
});
