import { execSync } from "node:child_process";

const envVariables = [
	// Git Providers
	"GITHUB_SHA",
	"CI_COMMIT_SHA", // GitLab
	"BITBUCKET_COMMIT",

	// CI Providers
	"BUILDKITE_COMMIT",
	"CIRCLE_SHA1",

	// Cloud Providers
	"VERCEL_GIT_COMMIT_SHA",
	"COMMIT_REF", // Netlify
	"WORKERS_CI_COMMIT_SHA",
	"RAILWAY_GIT_COMMIT_SHA",
	"AWS_COMMIT_ID",
	"CF_PAGES_COMMIT_SHA",
	"RENDER_GIT_COMMIT",
	"KOYEB_GIT_SHA",
	"SVL_DEPLOYMENT_COMMIT_SHA", // Sevalla
	"SOURCE_COMMIT", // Coolify
];

export const getGitCommitHashSync = (): string | undefined => {
	for (const envVariable of envVariables) {
		const value = process.env[envVariable]?.trim();
		if (value) return value;
	}
	try {
		return execSync("git rev-parse HEAD", { encoding: "utf8" }).trim();
	} catch {
		return undefined;
	}
};
