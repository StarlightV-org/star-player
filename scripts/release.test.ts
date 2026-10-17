/** Uses fixed test-only keys, never production secrets. */
import { afterEach, expect, test } from "bun:test";
import { createHash, createPublicKey, sign, verify } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { packageVersion, privateKeyFromSeed, rawPublicKey } from "./release";

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

async function fixture() {
  const directory = mkdtempSync(join(tmpdir(), "star-player-signing-test-"));
  directories.push(directory);
  const artifact = join(directory, "star-player-windows-x86_64.exe");
  writeFileSync(artifact, "test executable");
  const seed = Buffer.alloc(32, 7);
  const key = privateKeyFromSeed(seed);
  const env = {
    ...process.env,
    STAR_PLAYER_UPDATE_PRIVATE_KEY: seed.toString("base64"),
    STAR_PLAYER_UPDATE_PUBLIC_KEY: rawPublicKey(key).toString("base64"),
  };
  const version = await packageVersion();
  const command = [process.execPath, join(import.meta.dir, "release.ts"),
    "--artifact", artifact, "--version", version, "--output", directory];
  return { directory, artifact, key, env, version, command };
}

test("metadata, checksums and raw signature preserve the release contract", async () => {
  const { directory, key, env, version, command } = await fixture();
  const result = Bun.spawnSync(command, { env });
  expect(result.stderr.toString()).toBe("");
  expect(result.exitCode).toBe(0);
  const metadata = readFileSync(join(directory, "update.json"));
  const signature = Buffer.from(readFileSync(join(directory, "update.json.sig"), "utf8"), "base64");
  const digest = createHash("sha256").update("test executable").digest("hex");
  expect(metadata.toString()).toBe(JSON.stringify({
    asset: "star-player-windows-x86_64.exe",
    sha256: digest,
    size: Buffer.byteLength("test executable"),
    target: "x86_64-pc-windows-msvc",
    version,
  }));
  expect(signature.length).toBe(64);
  expect(verify(null, metadata, createPublicKey(key), signature)).toBe(true);
  expect(verify(null, Buffer.concat([metadata, Buffer.from(" ")]), createPublicKey(key), signature)).toBe(false);
  expect(readFileSync(join(directory, "SHA256SUMS"), "utf8")).toBe(`${digest}  star-player-windows-x86_64.exe\n`);
});

test("rejects mismatched public keys before writing metadata", async () => {
  const { directory, env, command } = await fixture();
  env.STAR_PLAYER_UPDATE_PUBLIC_KEY = Buffer.alloc(32, 8).toString("base64");
  const result = Bun.spawnSync(command, { env });
  expect(result.exitCode).not.toBe(0);
  expect(result.stderr.toString()).toContain("does not match");
  expect(await Bun.file(join(directory, "update.json")).exists()).toBe(false);
});

test("rejects malformed or wrong-length private keys", async () => {
  const { env, command } = await fixture();
  for (const value of ["not base64!", Buffer.alloc(31).toString("base64"), ""]) {
    const result = Bun.spawnSync(command, { env: { ...env, STAR_PLAYER_UPDATE_PRIVATE_KEY: value } });
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr.toString()).toContain("32-byte Ed25519 key");
  }
});

test("rejects wrong versions, asset names and empty files", async () => {
  const { artifact, env, command } = await fixture();
  const wrongVersion = [...command];
  wrongVersion[5] = "0.0.0-invalid";
  expect(Bun.spawnSync(wrongVersion, { env }).stderr.toString()).toContain("stable Cargo.toml version");
  const wrongAsset = join(artifact, "..", "wrong.exe");
  writeFileSync(wrongAsset, "test executable");
  const wrongName = [...command];
  wrongName[3] = wrongAsset;
  expect(Bun.spawnSync(wrongName, { env }).stderr.toString()).toContain("Unexpected release asset name");
  writeFileSync(artifact, "");
  expect(Bun.spawnSync(command, { env }).stderr.toString()).toContain("size limits");
});

test("raw seed encoding matches the RFC 8032 Ed25519 test vector", () => {
  const seed = Buffer.from("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "hex");
  const key = privateKeyFromSeed(seed);
  expect(rawPublicKey(key).toString("hex")).toBe("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
  expect(sign(null, Buffer.alloc(0), key).toString("hex")).toBe(
    "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155" +
    "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
  );
});

test("key generation emits the existing base64 raw-key format", () => {
  const result = Bun.spawnSync([process.execPath, join(import.meta.dir, "release.ts"), "--generate-key"]);
  expect(result.exitCode).toBe(0);
  const lines = result.stdout.toString().trim().split(/\r?\n/);
  const seedLine = lines[1];
  const publicKeyLine = lines[3];
  if (seedLine === undefined || publicKeyLine === undefined) {
    throw new Error("Key generation did not emit both key values");
  }
  const seed = Buffer.from(seedLine, "base64");
  const publicKey = Buffer.from(publicKeyLine, "base64");
  expect(seed.length).toBe(32);
  expect(publicKey.length).toBe(32);
  expect(rawPublicKey(privateKeyFromSeed(seed)).equals(publicKey)).toBe(true);
});
