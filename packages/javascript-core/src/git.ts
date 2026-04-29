import { execSync } from "node:child_process";

const envVariables = [
	"GITHUB_SHA",
	"CI_COMMIT_SHA",
	"BITBUCKET_COMMIT",
	"BUILDKITE_COMMIT",
	"CIRCLE_SHA1",
	"VERCEL_GIT_COMMIT_SHA",
	"COMMIT_REF",
	"WORKERS_CI_COMMIT_SHA",
	"RAILWAY_GIT_COMMIT_SHA",
	"AWS_COMMIT_ID",
	"CF_PAGES_COMMIT_SHA",
	"RENDER_GIT_COMMIT",
	"KOYEB_GIT_SHA",
	"SVL_DEPLOYMENT_COMMIT_SHA",
	"SOURCE_COMMIT",
];

export const getGitCommitHashSync = (): string | undefined => {
	for (const envVariable of envVariables) {
		const value = process.env[envVariable]?.trim();
		if (value) {
			return value;
		}
	}

	try {
		return execSync("git rev-parse HEAD", { encoding: "utf8" }).trim();
	} catch {
		return undefined;
	}
};
