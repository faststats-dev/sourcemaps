import { defineConfig } from "tsdown";

export default defineConfig({
	entry: {
		index: "src/index.ts",
		vite: "src/vite.ts",
		rollup: "src/rollup.ts",
		rolldown: "src/rolldown.ts",
		webpack: "src/webpack.ts",
		rspack: "src/rspack.ts",
		esbuild: "src/esbuild.ts",
		unloader: "src/unloader.ts",
		farm: "src/farm.ts",
		bun: "src/bun.ts",
	},
	format: ["esm"],
	clean: true,
	outDir: "dist",
});
