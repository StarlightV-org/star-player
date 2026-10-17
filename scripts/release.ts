/** Sign portable release metadata with the existing raw Ed25519 key format. */
import { TOML } from "bun";
import {
  createHash,
  createPrivateKey,
  createPublicKey,
  generateKeyPairSync,
  sign,
  verify,
  type KeyObject,
} from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdir, stat, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import { parseArgs } from "node:util";

// DER wrappers for the same 32-byte seed/public key used by the Rust updater.
const PKCS8_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");
const SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

function Print(message: string, error = false): void {
  (error ? process.stderr : process.stdout).write(`${message}\n`);
}

function decodeKey(value: string | undefined, name: string): Buffer {
  if (!value || !/^[A-Za-z0-9+/]{43}=$/.test(value)) {
    throw new Error(`${name} must be a base64-encoded 32-byte Ed25519 key`);
  }
  const bytes = Buffer.from(value, "base64");
  if (bytes.length !== 32 || bytes.toString("base64") !== value) {
    throw new Error(`${name} must be a base64-encoded 32-byte Ed25519 key`);
  }
  return bytes;
}

export function privateKeyFromSeed(seed: Buffer): KeyObject {
  if (seed.length !== 32) throw new Error("Expected a 32-byte Ed25519 seed");
  return createPrivateKey({
    key: Buffer.concat([PKCS8_PREFIX, seed]),
    format: "der",
    type: "pkcs8",
  });
}

export function rawPublicKey(key: KeyObject): Buffer {
  const bytes = createPublicKey(key).export({ format: "der", type: "spki" });
  if (bytes.length !== SPKI_PREFIX.length + 32 || !bytes.subarray(0, SPKI_PREFIX.length).equals(SPKI_PREFIX)) {
    throw new Error("Unexpected Ed25519 public key encoding");
  }
  return bytes.subarray(SPKI_PREFIX.length);
}

export async function packageVersion(): Promise<string> {
  const cargo = TOML.parse(await Bun.file(new URL("../Cargo.toml", import.meta.url)).text()) as {
    package?: { version?: unknown };
  };
  if (typeof cargo.package?.version !== "string") throw new Error("Missing Cargo.toml package.version");
  return cargo.package.version;
}

async function main(): Promise<void> {
  const { values } = parseArgs({
    args: Bun.argv.slice(2),
    options: {
      "generate-key": { type: "boolean" },
      artifact: { type: "string" },
      version: { type: "string" },
      output: { type: "string", default: "dist" },
    },
  });
  if (values["generate-key"]) {
    const { privateKey } = generateKeyPairSync("ed25519");
    const bytes = privateKey.export({ format: "der", type: "pkcs8" });
    if (bytes.length !== PKCS8_PREFIX.length + 32 || !bytes.subarray(0, PKCS8_PREFIX.length).equals(PKCS8_PREFIX)) {
      throw new Error("Unexpected Ed25519 private key encoding");
    }
    Print("Store this as the protected release environment secret STAR_PLAYER_UPDATE_PRIVATE_KEY:");
    Print(bytes.subarray(PKCS8_PREFIX.length).toString("base64"));
    Print("Store this as the repository variable STAR_PLAYER_UPDATE_PUBLIC_KEY:");
    Print(rawPublicKey(privateKey).toString("base64"));
    return;
  }
  if (!values.artifact || !values.version) {
    throw new Error("--artifact and --version are required when signing");
  }
  const version = await packageVersion();
  if (values.version !== version || version.includes("-")) {
    throw new Error("Release tag must match the stable Cargo.toml version");
  }
  const key = privateKeyFromSeed(decodeKey(process.env.STAR_PLAYER_UPDATE_PRIVATE_KEY, "STAR_PLAYER_UPDATE_PRIVATE_KEY"));
  const expectedPublic = decodeKey(process.env.STAR_PLAYER_UPDATE_PUBLIC_KEY?.trim(), "STAR_PLAYER_UPDATE_PUBLIC_KEY");
  if (!rawPublicKey(key).equals(expectedPublic)) {
    throw new Error("Signing key does not match the public key embedded in the executable");
  }
  const asset = basename(values.artifact);
  if (asset !== "star-player-windows-x86_64.exe") throw new Error("Unexpected release asset name");
  const info = await stat(values.artifact);
  if (!info.isFile() || info.size <= 0 || info.size > 512 * 1024 * 1024) {
    throw new Error("Executable exceeds updater size limits or is not a nonempty file");
  }
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(values.artifact)) hash.update(chunk);
  const digest = hash.digest("hex");
  // Alphabetical keys and no whitespace preserve the Python signer's bytes.
  const metadata = Buffer.from(JSON.stringify({
    asset,
    sha256: digest,
    size: info.size,
    target: "x86_64-pc-windows-msvc",
    version,
  }));
  const signature = sign(null, metadata, key);
  if (!verify(null, metadata, createPublicKey(key), signature)) throw new Error("Signature self-check failed");
  await mkdir(values.output, { recursive: true });
  await writeFile(join(values.output, "update.json"), metadata);
  await writeFile(join(values.output, "update.json.sig"), signature.toString("base64"));
  await writeFile(join(values.output, "SHA256SUMS"), `${digest}  ${asset}\n`);
}

if (import.meta.main) {
  try {
    await main();
  } catch (error) {
    Print(error instanceof Error ? error.message : String(error), true);
    process.exitCode = 1;
  }
}
