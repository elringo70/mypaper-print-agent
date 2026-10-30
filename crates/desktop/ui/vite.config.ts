import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Tauri expects a fixed port and does not want Vite to clear its output.
export default defineConfig({
	plugins: [svelte()],
	clearScreen: false,
	server: {
		port: 1420,
		strictPort: true,
		host: '127.0.0.1',
		// Rust artifacts are not frontend inputs: rebuilding Vite for them only
		// steals the dev loop from `tauri dev`, which restarts the window itself.
		watch: { ignored: ['**/target/**', '**/src-tauri/**'] }
	},
	build: { outDir: 'dist', emptyOutDir: true, target: 'chrome105' }
});
