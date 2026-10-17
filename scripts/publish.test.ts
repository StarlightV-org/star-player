import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { publishVersion, releaseVersion } from "./publish";

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

function git(directory: string, ...args: string[]): string {
  const result = Bun.spawnSync(["git", ...args], { cwd: directory, stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) throw new Error(result.stderr.toString());
  return result.stdout.toString().trim();
}

function versionFiles(directory: string, manifestVersion: string, lockVersion = manifestVersion): void {
  writeFileSync(join(directory, "Cargo.toml"), `[package]\nname = "star-player"\nversion = "${manifestVersion}"\nedition = "2024"\n`);
  writeFileSync(join(directory, "Cargo.lock"), `# Generated lockfile\nversion = 4\n\n[[package]]\nname = "dependency"\nversion = "1.2.3"\nsource = "registry+https://example.invalid"\n\n[[package]]\nname = "star-player"\nversion = "${lockVersion}"\n`);
}

function fixture(tag = true) {
  const directory = mkdtempSync(join(tmpdir(), "star-player-publish-test-"));
  directories.push(directory);
  const repo = join(directory, "repo");
  const remote = join(directory, "remote.git");
  mkdirSync(repo);
  git(directory, "init", "--bare", remote);
  git(repo, "init", "-b", "main");
  git(repo, "config", "user.name", "Release Test");
  git(repo, "config", "user.email", "release-test@example.invalid");
  git(repo, "config", "commit.gpgsign", "false");
  git(repo, "config", "tag.gpgsign", "false");
  git(repo, "config", "core.autocrlf", "false");
  git(repo, "remote", "add", "origin", remote);
  versionFiles(repo, "1.2.3");
  git(repo, "add", "Cargo.toml", "Cargo.lock");
  git(repo, "commit", "-m", "Initial version");
  if (tag) git(repo, "tag", "v1.2.3");
  return { repo, remote };
}

test("selects a patch bump only for an unchanged released version", () => {
  expect(releaseVersion("1.2.3", "1.2.3")).toBe("1.2.4");
  expect(releaseVersion("1.3.0", "1.2.3")).toBe("1.3.0");
  expect(releaseVersion("1.2.3")).toBe("1.2.3");
  expect(() => releaseVersion("1.2.2", "1.2.3")).toThrow("older");
  expect(() => releaseVersion("1.2.3-beta.1")).toThrow("stable");
  expect(() => releaseVersion("1.2.18446744073709551615", "1.2.18446744073709551615")).toThrow("limit");
});

test("bumps, commits, tags and pushes an unchanged release", () => {
  const { repo, remote } = fixture();
  expect(publishVersion(repo, "origin", () => {})).toBe("1.2.4");
  expect(readFileSync(join(repo, "Cargo.toml"), "utf8")).toContain('version = "1.2.4"');
  const lock = readFileSync(join(repo, "Cargo.lock"), "utf8");
  expect(lock).toContain('name = "star-player"\nversion = "1.2.4"');
  expect(lock).toContain('name = "dependency"\nversion = "1.2.3"');
  expect(git(repo, "status", "--porcelain")).toBe("");
  expect(git(repo, "cat-file", "-t", "v1.2.4")).toBe("tag");
  expect(git(remote, "rev-parse", "refs/tags/v1.2.4^{}")).toBe(git(remote, "rev-parse", "refs/heads/main"));
});

test("uses a changed Cargo.lock value and synchronizes Cargo.toml", () => {
  const { repo, remote } = fixture();
  versionFiles(repo, "1.2.3", "1.3.0");
  expect(publishVersion(repo, "origin", () => {})).toBe("1.3.0");
  expect(readFileSync(join(repo, "Cargo.toml"), "utf8")).toContain('version = "1.3.0"');
  expect(git(remote, "show", "refs/tags/v1.3.0:Cargo.lock")).toContain('name = "star-player"\nversion = "1.3.0"');
});

test("uses a version change that was already committed", () => {
  const { repo } = fixture();
  versionFiles(repo, "2.0.0");
  git(repo, "add", "Cargo.toml", "Cargo.lock");
  git(repo, "commit", "-m", "Choose next version");
  const head = git(repo, "rev-parse", "HEAD");
  expect(publishVersion(repo, "origin", () => {})).toBe("2.0.0");
  expect(git(repo, "rev-parse", "HEAD")).toBe(head);
});

test("first release uses the current version without an unnecessary commit", () => {
  const { repo } = fixture(false);
  const head = git(repo, "rev-parse", "HEAD");
  expect(publishVersion(repo, "origin", () => {})).toBe("1.2.3");
  expect(git(repo, "rev-parse", "HEAD")).toBe(head);
});

test("refuses unrelated staged files and Cargo dependency edits", () => {
  const { repo } = fixture();
  writeFileSync(join(repo, "unrelated.txt"), "user work");
  git(repo, "add", "unrelated.txt");
  expect(() => publishVersion(repo, "origin", () => {})).toThrow("Commit other changes");
  expect(git(repo, "diff", "--cached", "--name-only")).toBe("unrelated.txt");
  const other = fixture();
  const path = join(other.repo, "Cargo.lock");
  writeFileSync(path, readFileSync(path, "utf8").replace('version = "1.2.3"', 'version = "1.2.9"'));
  expect(() => publishVersion(other.repo, "origin", () => {})).toThrow("non-version changes");
});

test("fetches release tags before selecting a version", () => {
  const { repo, remote } = fixture(false);
  git(repo, "tag", "v1.2.3");
  git(repo, "push", "origin", "refs/tags/v1.2.3");
  git(repo, "tag", "-d", "v1.2.3");
  expect(publishVersion(repo, "origin", () => {})).toBe("1.2.4");
  expect(git(remote, "tag", "--list", "v1.2.4")).toBe("v1.2.4");
});

test("refuses detached HEAD and version downgrades", () => {
  const { repo } = fixture();
  git(repo, "checkout", "--detach");
  expect(() => publishVersion(repo, "origin", () => {})).toThrow("symbolic-ref");
  const other = fixture();
  versionFiles(other.repo, "1.2.2");
  expect(() => publishVersion(other.repo, "origin", () => {})).toThrow("older");
  expect(git(other.repo, "tag", "--list")).toBe("v1.2.3");
});

test("a rejected atomic push retains local work and does not publish the tag", () => {
  const { repo, remote } = fixture();
  // Disallow updates of the remote main branch, so the atomic transaction fails.
  git(repo, "push", "origin", "HEAD:refs/heads/main");
  git(remote, "config", "receive.denyCurrentBranch", "refuse");
  git(remote, "config", "core.bare", "false");
  git(remote, "symbolic-ref", "HEAD", "refs/heads/main");
  expect(() => publishVersion(repo, "origin", () => {})).toThrow("Retry the push, not this script");
  expect(git(repo, "tag", "--list", "v1.2.4")).toBe("v1.2.4");
  expect(git(remote, "tag", "--list", "v1.2.4")).toBe("");
  expect(git(repo, "show", "HEAD:Cargo.toml")).toContain('version = "1.2.4"');
});
