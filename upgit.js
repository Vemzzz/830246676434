#!/usr/bin/env node

const { spawnSync } = require("node:child_process");
const { createInterface } = require("node:readline/promises");
const { stdin, stdout, exitCode } = require("node:process");
const fs = require("node:fs");
const path = require("node:path");

const prompt = createInterface({ input: stdin, output: stdout });
const remote = "origin";

// Isi default .gitignore untuk mencegah file rahasia/log ter-commit
const DEFAULT_GITIGNORE = `
# Environment variables & Secrets
.env
.env.*
*.pem
*.key
*.p12

# Dependencies
node_modules/

# Logs & Debug
*.log
npm-debug.log*

# OS Files
.DS_Store
Thumbs.db
`;

async function ask(label, defaultValue = "") {
  const displayDefault = defaultValue ? ` (${defaultValue})` : "";
  const answer = await prompt.question(`${label}${displayDefault}: `);
  return answer.trim() || defaultValue;
}

function runGit(args, options = {}) {
  const result = spawnSync("git", args, { stdio: "inherit", ...options });
  if (result.error) {
    throw new Error(`Could not run git: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} failed with exit code ${result.status ?? "unknown"}`);
  }
  return result;
}

function ensureGitignore() {
  const gitignorePath = path.join(process.cwd(), ".gitignore");
  if (!fs.existsSync(gitignorePath)) {
    fs.writeFileSync(gitignorePath, DEFAULT_GITIGNORE.trim() + "\n");
    console.log("Dibuat file .gitignore baru.");
  } else {
    let content = fs.readFileSync(gitignorePath, "utf8");
    if (!content.includes(".env")) {
      content += "\n.env\n.env.*\n*.pem\n*.key\n";
      fs.writeFileSync(gitignorePath, content);
      console.log("Diperbarui file .gitignore untuk mengabaikan file rahasia.");
    }
  }
}

async function main() {
  try {
    let remoteUrl = await ask(
      "GitHub repository URL",
      "https://github.com/bebasdah172-prog/973743735748.git",
    );

    const token = await ask("GitHub Personal Access Token (PAT / kosongkan jika ingin pakai login terminal)", "");

    if (token) {
      remoteUrl = remoteUrl.replace(/^https:\/\//, `https://${token}@`);
    }

    const commitMessage = await ask("Commit message", "first commit");
    const branch = await ask("Branch name", "main");

    // 1. Pastikan .gitignore terpasang
    ensureGitignore();

    // 2. Bersihkan riwayat commit lama yang mengandung rahasia (Reset ke 1 commit bersih)
    console.log("Mereset riwayat Git untuk menghapus jejak rahasia dari commit lama...");
    runGit(["checkout", "--orphan", "temp_clean_branch"]);
    runGit(["add", "."]);
    runGit(["commit", "-m", commitMessage]);
    runGit(["branch", "-D", branch], { stdio: "ignore" }); // Hapus branch lama jika ada
    runGit(["branch", "-m", branch]); // Ubah nama branch temp menjadi main

    // 3. Atur Remote URL
    const remotes = runGit(["remote"], { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] })
      .stdout
      .trim()
      .split(/\r?\n/);

    if (remotes.includes(remote)) {
      runGit(["remote", "set-url", remote, remoteUrl]);
    } else {
      runGit(["remote", "add", remote, remoteUrl]);
    }

    console.log(`Pushing branch "${branch}" to ${remoteUrl.replace(/\/\/[^@]+@/, "//***@")}...`);

    const pushArgs = token
      ? ["push", "-f", "-u", remote, branch]
      : ["-c", "credential.helper=", "push", "-f", "-u", remote, branch];

    runGit(pushArgs);
    console.log("Berhasil push kode tanpa riwayat rahasia!");
  } finally {
    prompt.close();
  }
}

main().catch((error) => {
  console.error(`upgit: ${error.message}`);
  process.exitCode = 1;
});