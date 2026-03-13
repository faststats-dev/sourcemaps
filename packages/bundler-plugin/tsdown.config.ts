import { defineConfig } from "tsdown";

export default defineConfig({
	entry: {
		index: "src/index.ts",
		"bundler/vite": "src/bundler/vite.ts",
		"bundler/rollup": "src/bundler/rollup.ts",
		"bundler/rolldown": "src/bundler/rolldown.ts",
		"bundler/webpack": "src/bundler/webpack.ts",
		"bundler/rspack": "src/bundler/rspack.ts",
		"bundler/esbuild": "src/bundler/esbuild.ts",
		"bundler/unloader": "src/bundler/unloader.ts",
		"bundler/farm": "src/bundler/farm.ts",
		"bundler/bun": "src/bundler/bun.ts",
	},
	format: ["esm"],
	clean: true,
	outDir: "dist",
});
