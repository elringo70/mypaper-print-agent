import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

// Needed for `lang="ts"` in components and for svelte-check to find the setup.
export default {
	preprocess: vitePreprocess()
};
