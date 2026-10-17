/** Select a release version, commit version files if needed, then tag and push. */

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { TOML } from "bun";
import "@starlightv-org/print";

function git(directory: string, ...args: string[]): string {
  const result = Bun.spawnSync(["git", ...args], {
    cwd: directory,
    env: { ...process.env, GIT_TERMINAL_PROMPT: "0" },
    stdout: "pipe",
    stderr: "pipe",
  });
  if (result.exitCode !== 0) {
    throw new Error(
      `git ${args.join(" ")} failed: ${result.stderr.toString().trim() || result.stdout.toString().trim()}`,
    );
  }
  return result.stdout.toString();
}

function parts(version: string): [bigint, bigint, bigint] {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) {
    throw new Error(
      `Expected a stable major.minor.patch version, got ${version}`,
    );
  }
  const [major, minor, patch] = version.split(".");
  if (major === undefined || minor === undefined || patch === undefined) {
    throw new Error("Expected three version components");
  }
  const values: [bigint, bigint, bigint] = [
    BigInt(major),
    BigInt(minor),
    BigInt(patch),
  ];
  if (values.some((value) => value > 18446744073709551615n))
    throw new Error("Version component exceeds Cargo's limit");
  return values;
}

function compare(left: string, right: string): number {
  const [leftMajor, leftMinor, leftPatch] = parts(left);
  const [rightMajor, rightMinor, rightPatch] = parts(right);
  for (const difference of [
    leftMajor - rightMajor,
    leftMinor - rightMinor,
    leftPatch - rightPatch,
  ]) {
    if (difference < 0n) return -1;
    if (difference > 0n) return 1;
  }
  return 0;
}

export function releaseVersion(current: string, previous?: string): string {
  const [major, minor, patch] = parts(current);
  if (previous === undefined) return current;
  const order = compare(current, previous);
  if (order < 0)
    throw new Error(
      `Cargo.lock version ${current} is older than the last release ${previous}`,
    );
  const next = order === 0 ? `${major}.${minor}.${patch + 1n}` : current;
  parts(next);
  return next;
}

function rootPackage(manifest: string): { name: string; version: string } {
  const data = TOML.parse(manifest) as {
    package?: { name?: unknown; version?: unknown };
  };
  if (
    typeof data.package?.name !== "string" ||
    typeof data.package.version !== "string"
  ) {
    throw new Error("Cargo.toml must contain package.name and package.version");
  }
  return { name: data.package.name, version: data.package.version };
}

function lockedVersion(lock: string, name: string): string {
  const data = TOML.parse(lock) as {
    package?: { name: string; version: string; source?: string }[];
  };
  const matches =
    data.package?.filter((entry) => entry.name === name && !entry.source) ?? [];
  if (matches.length !== 1 || typeof matches[0]?.version !== "string") {
    throw new Error(
      `Cargo.lock must contain exactly one local package named ${name}`,
    );
  }
  return matches[0].version;
}

function replaceVersion(section: string, version: string): string {
  const pattern = /^([ \t]*version\s*=\s*)"[^"\r\n]*"/m;
  if (!pattern.test(section))
    throw new Error("Cannot locate the version field");
  return section.replace(pattern, `$1"${version}"`);
}

function updateManifest(manifest: string, version: string): string {
  const sections = manifest.split(/(?=^\[)/m);
  let found = false;
  const result = sections
    .map((section) => {
      if (!/^\[package\][ \t]*(?:#[^\r\n]*)?(?:\r?\n|$)/.test(section))
        return section;
      found = true;
      return replaceVersion(section, version);
    })
    .join("");
  if (!found) throw new Error("Cannot locate Cargo.toml [package]");
  return result;
}

function updateLock(lock: string, name: string, version: string): string {
  const sections = lock.split(/(?=^\[\[package\]\])/m);
  let found = false;
  const result = sections
    .map((section) => {
      if (!/^\[\[package\]\]/.test(section)) return section;
      const parsed = TOML.parse(section) as {
        package: { name: string; source?: string }[];
      };
      if (parsed.package[0]?.name !== name || parsed.package[0]?.source)
        return section;
      found = true;
      return replaceVersion(section, version);
    })
    .join("");
  if (!found) throw new Error(`Cannot locate ${name} in Cargo.lock`);
  return result;
}

export function publishVersion(
  directory: string,
  remote = "origin",
  output: (message: string) => void = Print,
): string {
  if (!/^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(remote))
    throw new Error("Expected a configured Git remote name");
  const branch = git(
    directory,
    "symbolic-ref",
    "--quiet",
    "--short",
    "HEAD",
  ).trim();
  git(directory, "remote", "get-url", remote);
  const changes = git(
    directory,
    "status",
    "--porcelain=v1",
    "-z",
    "--untracked-files=all",
  )
    .split("\0")
    .filter(Boolean);
  if (
    changes.some((entry) => !/^[ M]{2} (Cargo\.toml|Cargo\.lock)$/.test(entry))
  ) {
    throw new Error(
      "Commit other changes before publishing. Only Cargo version edits may be uncommitted.",
    );
  }
  const manifestPath = join(directory, "Cargo.toml");
  const lockPath = join(directory, "Cargo.lock");
  const manifest = readFileSync(manifestPath, "utf8");
  const lock = readFileSync(lockPath, "utf8");
  const { name } = rootPackage(manifest);
  const current = lockedVersion(lock, name);
  parts(current);
  // Never include dependency edits or other Cargo changes in an automatic commit.
  const committedManifest = git(directory, "show", "HEAD:Cargo.toml");
  const committedLock = git(directory, "show", "HEAD:Cargo.lock");
  const normalize = (text: string) => text.replaceAll("\r\n", "\n");
  if (
    normalize(
      updateManifest(manifest, rootPackage(committedManifest).version),
    ) !== normalize(committedManifest) ||
    normalize(updateLock(lock, name, lockedVersion(committedLock, name))) !==
      normalize(committedLock)
  ) {
    throw new Error(
      "Commit non-version changes in Cargo.toml and Cargo.lock before publishing",
    );
  }
  output(`Fetching release tags from ${remote}...`);
  git(directory, "fetch", "--tags", remote);
  const tags = git(directory, "tag", "--list", "v*")
    .trim()
    .split(/\r?\n/)
    .filter((tag) => /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(tag))
    .sort((a, b) => compare(a.slice(1), b.slice(1)));
  const latest = tags.at(-1);
  let previous: string | undefined;
  if (latest) {
    git(directory, "merge-base", "--is-ancestor", latest, "HEAD");
    previous = lockedVersion(
      git(directory, "show", `${latest}:Cargo.lock`),
      name,
    );
    if (previous !== latest.slice(1))
      throw new Error(`${latest} does not match its Cargo.lock version`);
  }
  const version = releaseVersion(current, previous);
  const tag = `v${version}`;
  const nextManifest = updateManifest(manifest, version);
  const nextLock = updateLock(lock, name, version);
  output(
    `Publishing ${tag}${latest ? ` after ${latest}` : " as the first release"}`,
  );
  if (nextManifest !== manifest) writeFileSync(manifestPath, nextManifest);
  if (nextLock !== lock) writeFileSync(lockPath, nextLock);
  if (
    git(
      directory,
      "status",
      "--porcelain=v1",
      "--",
      "Cargo.toml",
      "Cargo.lock",
    ).trim()
  ) {
    git(
      directory,
      "commit",
      "--only",
      "-m",
      `chore: Bump version to ${version}`,
      "-m",
      `Keep Cargo.toml and Cargo.lock aligned for the ${tag} release.`,
      "--",
      "Cargo.toml",
      "Cargo.lock",
    );
  }
  // Verify the tag will point at the version consumed by the release workflow.
  if (
    rootPackage(git(directory, "show", "HEAD:Cargo.toml")).version !==
      version ||
    lockedVersion(git(directory, "show", "HEAD:Cargo.lock"), name) !== version
  ) {
    throw new Error(
      "Committed Cargo versions do not match the selected release",
    );
  }
  git(directory, "tag", "-a", tag, "-m", `Release ${tag}`);
  try {
    git(
      directory,
      "push",
      "--atomic",
      remote,
      `HEAD:refs/heads/${branch}`,
      `refs/tags/${tag}`,
    );
  } catch (error) {
    throw new Error(
      `${error instanceof Error ? error.message : error}\nLocal commit and tag retained. Retry the push, not this script:\ngit push --atomic ${remote} HEAD:refs/heads/${branch} refs/tags/${tag}`,
    );
  }
  output(`Pushed ${tag}. GitHub Actions will build and publish the release.`);
  return version;
}

if (import.meta.main) {
  try {
    const { values } = parseArgs({
      args: Bun.argv.slice(2),
      options: { remote: { type: "string", default: "origin" } },
    });
    publishVersion(join(import.meta.dir, ".."), values.remote);
  } catch (error) {
    Print(error instanceof Error ? error.message : String(error), true);
    process.exitCode = 1;
  }
}
