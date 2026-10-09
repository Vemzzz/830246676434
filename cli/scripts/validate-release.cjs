#!/usr/bin/env node

const fs = require("fs");
const { execFileSync } = require("child_process");

const [refName, mode] = process.argv.slice(2);
const requireAnnotatedTag = mode !== "--pretag";
if (!refName) throw new Error("Usage: validate-release.cjs <tag>");

const cliVersion = JSON.parse(fs.readFileSync("cli/package.json", "utf8")).version;
const appVersion = JSON.parse(fs.readFileSync("package.json", "utf8")).version;
const tagVersion = refName.replace(/^v/, "");
const changelog = fs.readFileSync("CHANGELOG.md", "utf8");

if (cliVersion !== appVersion) {
  throw new Error(`Package version mismatch: cli=${cliVersion}, app=${appVersion}`);
}
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(tagVersion)) {
  throw new Error(`Release version is not SemVer: ${tagVersion}`);
}
if (tagVersion !== cliVersion) {
  throw new Error(`Tag version mismatch: tag=${tagVersion}, package=${cliVersion}`);
}
if (!changelog.startsWith(`# v${tagVersion} `)) {
  throw new Error(`CHANGELOG.md must start with release heading # v${tagVersion}`);
}

const latestChangelogCommit = execFileSync(
  "git",
  ["log", "-1", "--format=%H", "--", "CHANGELOG.md"],
  { encoding: "utf8" },
).trim();
const headCommit = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
if (latestChangelogCommit !== headCommit) {
  throw new Error(
    `Release tag must point to the commit that last changed CHANGELOG.md: ${latestChangelogCommit} !== ${headCommit}`,
  );
}
const latestCommitFiles = execFileSync(
  "git",
  ["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"],
  { encoding: "utf8" },
).trim().split("\n").filter(Boolean);
if (latestCommitFiles.length !== 1 || latestCommitFiles[0] !== "CHANGELOG.md") {
  throw new Error("The commit immediately before the release tag must change only CHANGELOG.md");
}
if (requireAnnotatedTag) {
  const tagRef = process.env.RELEASE_TAG_REF || `refs/tags/${refName}`;
  try {
    execFileSync("git", ["rev-parse", "--verify", `${tagRef}^{tag}`], { encoding: "utf8" });
  } catch {
    throw new Error(`Release tag must be annotated: ${refName}`);
  }
}

if (process.env.GITHUB_OUTPUT) {
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `version=${tagVersion}\n`);
}
console.log(`Validated release ${tagVersion}`);
