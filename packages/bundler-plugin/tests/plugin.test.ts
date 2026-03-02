import { cp, mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { rspack } from "@rspack/core";
import { build as esbuildBuild } from "esbuild";
import { rollup as createRollupBundle } from "rollup";
import { build as viteBuild } from "vite";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import webpack from "webpack";
import bunPlugin from "../src/bun";
import esbuildPlugin from "../src/esbuild";
import farmPlugin from "../src/farm";
import sourcemapsPlugin from "../src/index";
import rolldownPlugin from "../src/rolldown";
import rollupPlugin from "../src/rollup";
import rspackPlugin from "../src/rspack";
import unloaderPlugin from "../src/unloader";
import vitePlugin from "../src/vite";
import webpackPlugin from "../src/webpack";

type UploadPayload = {
	buildId: string;
	bundler: string;
	uploadedAt: string;
	sourcemaps: Array<{ fileName: string; sourcemap: string }>;
};

const fixturesDir = resolve("tests/fixtures");

const listFiles = async (rootDir: string): Promise<string[]> => {
	const entries = await readdir(rootDir, { withFileTypes: true });
	const nested = await Promise.all(
		entries.map(async (entry) => {
			const fullPath = join(rootDir, entry.name);
			if (entry.isDirectory()) {
				return listFiles(fullPath);
			}
			return [fullPath];
		}),
	);

	return nested.flat();
};

const findFirst = async (
	rootDir: string,
	suffix: string,
): Promise<string | undefined> => {
	const files = await listFiles(rootDir);
	return files.find((filePath) => filePath.endsWith(suffix));
};

const firstPlugin = <T>(plugin: T | T[]): T =>
	Array.isArray(plugin) ? plugin[0]! : plugin;

const runWebpack = (config: webpack.Configuration): Promise<webpack.Stats> =>
	new Promise((resolvePromise, rejectPromise) => {
		webpack(config, (error, stats) => {
			if (error) {
				rejectPromise(error);
				return;
			}

			if (!stats || stats.hasErrors()) {
				rejectPromise(
					new Error(
						stats?.toString({ all: false, errors: true }) ??
							"Webpack build failed",
					),
				);
				return;
			}

			resolvePromise(stats);
		});
	});

const runRspack = (config: Parameters<typeof rspack>[0]): Promise<any> =>
	new Promise((resolvePromise, rejectPromise) => {
		rspack(config, (error, stats) => {
			if (error) {
				rejectPromise(error);
				return;
			}

			if (!stats || stats.hasErrors()) {
				rejectPromise(
					new Error(
						stats?.toString({ all: false, errors: true }) ??
							"Rspack build failed",
					),
				);
				return;
			}

			resolvePromise(stats);
		});
	});

describe("sourcemaps bundler plugin", () => {
	let uploads: UploadPayload[] = [];
	let server: ReturnType<typeof createServer>;
	let endpoint = "";

	beforeEach(async () => {
		uploads = [];
		server = createServer(async (request, response) => {
			if (request.method !== "POST" || request.url !== "/api/sourcemaps") {
				response.writeHead(404).end();
				return;
			}

			const chunks: Buffer[] = [];
			for await (const chunk of request) {
				chunks.push(Buffer.from(chunk));
			}

			const payload = JSON.parse(
				Buffer.concat(chunks).toString("utf8"),
			) as UploadPayload;
			uploads.push(payload);
			response.writeHead(200, { "content-type": "application/json" });
			response.end(JSON.stringify({ ok: true }));
		});

		await new Promise<void>((resolvePromise) => {
			server.listen(0, "127.0.0.1", () => resolvePromise());
		});

		const address = server.address();
		if (!address || typeof address === "string") {
			throw new Error("Could not resolve test server address");
		}

		endpoint = `http://127.0.0.1:${address.port}/api/sourcemaps`;
	});

	afterEach(async () => {
		await new Promise<void>((resolvePromise, rejectPromise) => {
			server.close((error) => {
				if (error) {
					rejectPromise(error);
					return;
				}
				resolvePromise();
			});
		});
	});

	it("exposes all unplugin adapter entrypoints", () => {
		const endpointOption = { endpoint: "http://localhost/example" };

		expect(typeof vitePlugin).toBe("function");
		expect(typeof sourcemapsPlugin.vite).toBe("function");
		expect(firstPlugin(vitePlugin(endpointOption)).name).toBe(
			firstPlugin(sourcemapsPlugin.vite(endpointOption)).name,
		);

		expect(typeof rollupPlugin).toBe("function");
		expect(typeof sourcemapsPlugin.rollup).toBe("function");
		expect(firstPlugin(rollupPlugin(endpointOption)).name).toBe(
			firstPlugin(sourcemapsPlugin.rollup(endpointOption)).name,
		);

		expect(typeof rolldownPlugin).toBe("function");
		expect(typeof sourcemapsPlugin.rolldown).toBe("function");
		expect(firstPlugin(rolldownPlugin(endpointOption)).name).toBe(
			firstPlugin(sourcemapsPlugin.rolldown(endpointOption)).name,
		);

		expect(typeof webpackPlugin).toBe("function");
		expect(typeof sourcemapsPlugin.webpack).toBe("function");
		expect(webpackPlugin(endpointOption).apply).toBeDefined();
		expect(sourcemapsPlugin.webpack(endpointOption).apply).toBeDefined();

		expect(typeof rspackPlugin).toBe("function");
		expect(typeof sourcemapsPlugin.rspack).toBe("function");
		expect(rspackPlugin(endpointOption).apply).toBeDefined();
		expect(sourcemapsPlugin.rspack(endpointOption).apply).toBeDefined();

		expect(typeof esbuildPlugin).toBe("function");
		expect(typeof sourcemapsPlugin.esbuild).toBe("function");
		expect(esbuildPlugin(endpointOption).name).toBe(
			sourcemapsPlugin.esbuild(endpointOption).name,
		);

		expect(typeof unloaderPlugin).toBe("function");
		expect(typeof sourcemapsPlugin.unloader).toBe("function");
		expect(firstPlugin(unloaderPlugin(endpointOption)).name).toBe(
			firstPlugin(sourcemapsPlugin.unloader(endpointOption)).name,
		);

		expect(typeof farmPlugin).toBe("function");
		expect(typeof sourcemapsPlugin.farm).toBe("function");
		expect(farmPlugin(endpointOption).name).toBe(
			sourcemapsPlugin.farm(endpointOption).name,
		);

		expect(typeof bunPlugin).toBe("function");
		expect(typeof sourcemapsPlugin.bun).toBe("function");
	});

	it("uploads and injects build metadata for vite", async () => {
		const cwd = await mkdtemp(join(tmpdir(), "sourcemaps-vite-"));
		await cp(join(fixturesDir, "vite"), cwd, { recursive: true });
		const buildId = "vite-build-id";
		const outDir = join(cwd, "dist");

		await viteBuild({
			configFile: false,
			root: cwd,
			build: {
				outDir,
				sourcemap: true,
				emptyOutDir: true,
			},
			plugins: [
				vitePlugin({
					endpoint,
					buildId,
					deleteAfterUpload: true,
				}),
			],
		});

		expect(uploads).toHaveLength(1);
		expect(uploads[0]?.bundler).toBe("vite");
		expect(uploads[0]?.buildId).toBe(buildId);
		expect(uploads[0]?.sourcemaps.length).toBeGreaterThan(0);
		const jsBundle = await findFirst(outDir, ".js");
		expect(jsBundle).toBeTruthy();
		const content = await readFile(jsBundle!, "utf8");
		expect(content.includes(`buildId:"${buildId}"`)).toBe(true);
		const mapBundle = await findFirst(outDir, ".map");
		expect(mapBundle).toBeUndefined();
		await rm(cwd, { recursive: true, force: true });
	});

	it("does nothing when enabled is false", async () => {
		const cwd = await mkdtemp(join(tmpdir(), "sourcemaps-vite-disabled-"));
		await cp(join(fixturesDir, "vite"), cwd, { recursive: true });
		const outDir = join(cwd, "dist");

		await viteBuild({
			configFile: false,
			root: cwd,
			build: {
				outDir,
				sourcemap: true,
				emptyOutDir: true,
			},
			plugins: [
				vitePlugin({
					endpoint,
					buildId: "disabled-build-id",
					deleteAfterUpload: true,
					enabled: false,
				}),
			],
		});

		expect(uploads).toHaveLength(0);
		const jsBundle = await findFirst(outDir, ".js");
		expect(jsBundle).toBeTruthy();
		if (!jsBundle) {
			throw new Error("Expected Vite output bundle to exist");
		}
		const content = await readFile(jsBundle, "utf8");
		expect(content.includes('buildId:"disabled-build-id"')).toBe(false);
		const mapBundle = await findFirst(outDir, ".map");
		expect(mapBundle).toBeTruthy();
		await rm(cwd, { recursive: true, force: true });
	});

	it("prefers webpack native build hash when buildId is not provided", async () => {
		const cwd = await mkdtemp(join(tmpdir(), "sourcemaps-webpack-native-id-"));
		await cp(join(fixturesDir, "webpack"), cwd, { recursive: true });
		const outDir = join(cwd, "dist");

		const stats = await runWebpack({
			mode: "production",
			context: cwd,
			entry: "./src/index.js",
			output: {
				path: outDir,
				filename: "bundle.js",
				clean: true,
			},
			devtool: "source-map",
			plugins: [
				webpackPlugin({
					endpoint,
				}) as webpack.WebpackPluginInstance,
			],
		});

		const nativeHash =
			stats.compilation.hash ??
			stats.compilation.fullHash ??
			stats.toJson({ all: false, hash: true }).hash;
		expect(typeof nativeHash).toBe("string");
		expect(nativeHash).toBeTruthy();
		expect(uploads).toHaveLength(1);
		expect(uploads[0]?.buildId).toBe(nativeHash);
		const content = await readFile(join(outDir, "bundle.js"), "utf8");
		expect(content.includes(`buildId:"${nativeHash}"`)).toBe(true);
		await rm(cwd, { recursive: true, force: true });
	});

	it("uploads and injects build metadata for rollup", async () => {
		const cwd = await mkdtemp(join(tmpdir(), "sourcemaps-rollup-"));
		await cp(join(fixturesDir, "rollup"), cwd, { recursive: true });
		const buildId = "rollup-build-id";
		const outDir = join(cwd, "dist");

		const bundle = await createRollupBundle({
			input: join(cwd, "src/main.js"),
			plugins: [
				rollupPlugin({
					endpoint,
					buildId,
					deleteAfterUpload: true,
				}),
			],
		});

		await bundle.write({
			dir: outDir,
			format: "esm",
			sourcemap: true,
		});
		await bundle.close();

		expect(uploads).toHaveLength(1);
		expect(uploads[0]?.bundler).toBe("rollup");
		expect(uploads[0]?.buildId).toBe(buildId);
		expect(uploads[0]?.sourcemaps.length).toBeGreaterThan(0);
		const jsBundle = await findFirst(outDir, ".js");
		expect(jsBundle).toBeTruthy();
		const content = await readFile(jsBundle!, "utf8");
		expect(content.includes(`buildId:"${buildId}"`)).toBe(true);
		const mapBundle = await findFirst(outDir, ".map");
		expect(mapBundle).toBeUndefined();
		await rm(cwd, { recursive: true, force: true });
	});

	it("prefers rspack native build hash when buildId is not provided", async () => {
		const cwd = await mkdtemp(join(tmpdir(), "sourcemaps-rspack-native-id-"));
		await cp(join(fixturesDir, "rspack"), cwd, { recursive: true });
		const outDir = join(cwd, "dist");

		const stats = await runRspack({
			mode: "production",
			context: cwd,
			entry: {
				main: "./src/index.js",
			},
			output: {
				path: outDir,
				filename: "bundle.js",
				clean: true,
			},
			devtool: "source-map",
			plugins: [
				rspackPlugin({
					endpoint,
				}),
			],
		});

		const nativeHash = stats.toJson({ all: false, hash: true }).hash;
		expect(typeof nativeHash).toBe("string");
		expect(nativeHash).toBeTruthy();
		expect(uploads).toHaveLength(1);
		expect(uploads[0]?.buildId).toBe(nativeHash);
		const content = await readFile(join(outDir, "bundle.js"), "utf8");
		expect(content.includes(`buildId:"${nativeHash}"`)).toBe(true);
		await rm(cwd, { recursive: true, force: true });
	});

	it("uploads and injects build metadata for webpack", async () => {
		const cwd = await mkdtemp(join(tmpdir(), "sourcemaps-webpack-"));
		await cp(join(fixturesDir, "webpack"), cwd, { recursive: true });
		const buildId = "webpack-build-id";
		const outDir = join(cwd, "dist");

		await runWebpack({
			mode: "production",
			context: cwd,
			entry: "./src/index.js",
			output: {
				path: outDir,
				filename: "bundle.js",
				clean: true,
			},
			devtool: "source-map",
			plugins: [
				webpackPlugin({
					endpoint,
					buildId,
					deleteAfterUpload: true,
				}) as webpack.WebpackPluginInstance,
			],
		});

		expect(uploads).toHaveLength(1);
		expect(uploads[0]?.bundler).toBe("webpack");
		expect(uploads[0]?.buildId).toBe(buildId);
		expect(uploads[0]?.sourcemaps.length).toBeGreaterThan(0);
		const jsBundle = join(outDir, "bundle.js");
		const content = await readFile(jsBundle, "utf8");
		expect(content.includes(`buildId:"${buildId}"`)).toBe(true);
		const mapBundle = await findFirst(outDir, ".map");
		expect(mapBundle).toBeUndefined();
		await rm(cwd, { recursive: true, force: true });
	});

	it("uploads and injects build metadata for rspack", async () => {
		const cwd = await mkdtemp(join(tmpdir(), "sourcemaps-rspack-"));
		await cp(join(fixturesDir, "rspack"), cwd, { recursive: true });
		const buildId = "rspack-build-id";
		const outDir = join(cwd, "dist");

		await runRspack({
			mode: "production",
			context: cwd,
			entry: {
				main: "./src/index.js",
			},
			output: {
				path: outDir,
				filename: "bundle.js",
				clean: true,
			},
			devtool: "source-map",
			plugins: [
				rspackPlugin({
					endpoint,
					buildId,
					deleteAfterUpload: true,
				}),
			],
		});

		expect(uploads).toHaveLength(1);
		expect(uploads[0]?.bundler).toBe("rspack");
		expect(uploads[0]?.buildId).toBe(buildId);
		expect(uploads[0]?.sourcemaps.length).toBeGreaterThan(0);
		const jsBundle = join(outDir, "bundle.js");
		const content = await readFile(jsBundle, "utf8");
		expect(content.includes(`buildId:"${buildId}"`)).toBe(true);
		const mapBundle = await findFirst(outDir, ".map");
		expect(mapBundle).toBeUndefined();
		await rm(cwd, { recursive: true, force: true });
	});

	it("uploads and injects build metadata for esbuild", async () => {
		const cwd = await mkdtemp(join(tmpdir(), "sourcemaps-esbuild-"));
		await cp(join(fixturesDir, "esbuild"), cwd, { recursive: true });
		const buildId = "esbuild-build-id";
		const outDir = join(cwd, "dist");

		await esbuildBuild({
			entryPoints: [join(cwd, "src/index.js")],
			bundle: true,
			format: "esm",
			outfile: join(outDir, "bundle.js"),
			sourcemap: true,
			plugins: [
				esbuildPlugin({
					endpoint,
					buildId,
					deleteAfterUpload: true,
				}),
			],
		});

		expect(uploads).toHaveLength(1);
		expect(uploads[0]?.bundler).toBe("esbuild");
		expect(uploads[0]?.buildId).toBe(buildId);
		expect(uploads[0]?.sourcemaps.length).toBeGreaterThan(0);
		const jsBundle = join(outDir, "bundle.js");
		const content = await readFile(jsBundle, "utf8");
		expect(content.includes(`buildId:"${buildId}"`)).toBe(true);
		const mapBundle = await findFirst(outDir, ".map");
		expect(mapBundle).toBeUndefined();
		await rm(cwd, { recursive: true, force: true });
	});
});
