// The burner wallet: a throwaway ed25519 keypair made in the tab.
//
// This is NOT a wallet-extension key and the page has no code to connect one.
// The key exists so that an input batch is attributable to the player who
// pressed the keys. Anyone can read it out of localStorage, and anyone who
// has it can sign batches as that player. That is deliberate: the key only
// ever signs inputs, the relayer pays every fee, and the amounts involved are
// tiny. Do not put anything of value behind this key.
//
// Key material never leaves this module except as bytes going into and out of
// localStorage, and the page never renders it.

export const BURNER_STORAGE_KEY = "doom-play.burner.v1";
export const BURNER_SESSION_KEY = "doom-play.burner.session.v1";

export class BurnerError extends Error {
  constructor(message, detail = {}) {
    super(message);
    this.name = "BurnerError";
    Object.assign(this, detail);
  }
}

export function ed25519Available() {
  return Boolean(globalThis.crypto?.subtle) && typeof globalThis.crypto.subtle.generateKey === "function";
}

function base64ToBytes(text) {
  const binary = atob(text);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

function bytesToBase64(bytes) {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function toHex(bytes) {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

// A short, human-checkable fingerprint so a player can tell two burner keys
// apart without either of them being secret. First 8 bytes of SHA-256.
export async function fingerprint(publicKeyRaw) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", publicKeyRaw));
  return toHex(digest.subarray(0, 8));
}

function readStored(storage) {
  let text = null;
  try {
    text = storage.getItem(BURNER_STORAGE_KEY);
  } catch {
    return null;
  }
  if (!text) return null;
  try {
    const parsed = JSON.parse(text);
    if (typeof parsed?.pkcs8 !== "string" || typeof parsed?.publicKey !== "string") return null;
    return parsed;
  } catch {
    return null;
  }
}

export class BurnerWallet {
  constructor({privateKey, publicKey, publicKeyRaw, createdAt = null}) {
    this.privateKey = privateKey;
    this.publicKey = publicKey;
    this.publicKeyRaw = publicKeyRaw;
    this.createdAt = createdAt;
  }

  get publicKeyHex() {
    return toHex(this.publicKeyRaw);
  }

  get shortFingerprint() {
    return this.publicKeyHex.slice(0, 16);
  }

  // The handle the relayer sees. It identifies the player for input
  // attribution; it cannot be used to sign anything else.
  get handle() {
    return `burner:${this.publicKeyHex}`;
  }

  async sign(message) {
    const {signInput} = await import("./input.js");
    return signInput(this.privateKey, message);
  }
}

function hexToBytes(text) {
  if (typeof text !== "string" || text.length % 2) throw new BurnerError("stored public key is not hex");
  const out = new Uint8Array(text.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = parseInt(text.slice(i * 2, i * 2 + 2), 16);
  return out;
}

// Returns the stored burner, generating and persisting one on first use.
export async function loadOrCreateBurner({storage = globalThis.localStorage} = {}) {
  if (!ed25519Available()) {
    throw new BurnerError(
      "this browser has no WebCrypto Ed25519, so the page cannot make a signing key. " +
        "Use a current Chrome, Safari or Firefox, or open the page over https.",
    );
  }
  const stored = readStored(storage);
  if (stored) {
    try {
      const pkcs8 = base64ToBytes(stored.pkcs8);
      const publicKeyRaw = hexToBytes(stored.publicKey);
      if (publicKeyRaw.length !== 32) throw new BurnerError("stored public key is not 32 bytes");
      const privateKey = await crypto.subtle.importKey("pkcs8", pkcs8, {name: "Ed25519"}, false, ["sign"]);
      const publicKey = await crypto.subtle.importKey("raw", publicKeyRaw, {name: "Ed25519"}, true, ["verify"]);
      return {wallet: new BurnerWallet({privateKey, publicKey, publicKeyRaw, createdAt: stored.createdAt ?? null}), created: false};
    } catch {
      // A corrupt record is not worth an error page. Start a new burner.
    }
  }
  const pair = await crypto.subtle.generateKey({name: "Ed25519"}, true, ["sign", "verify"]);
  const publicKeyRaw = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  const pkcs8 = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
  const createdAt = new Date().toISOString();
  const record = {
    kind: "doom-play-burner",
    version: 1,
    note: "throwaway test key, signs inputs only, holds no funds",
    publicKey: toHex(publicKeyRaw),
    pkcs8: bytesToBase64(pkcs8),
    createdAt,
  };
  try {
    storage.setItem(BURNER_STORAGE_KEY, JSON.stringify(record));
  } catch {
    // Private mode with storage disabled: the key still works for this tab.
  }
  return {
    wallet: new BurnerWallet({privateKey: pair.privateKey, publicKey: pair.publicKey, publicKeyRaw, createdAt}),
    created: true,
  };
}

// The relayer remembers which burner is driving which session, so a reload
// re-attaches to the same player instead of orphaning a slot.
export function rememberSession(storage, session) {
  try {
    storage.setItem(BURNER_SESSION_KEY, JSON.stringify(session));
  } catch {
    /* storage disabled; the player can still start a new session */
  }
}

export function recallSession(storage) {
  try {
    const text = storage.getItem(BURNER_SESSION_KEY);
    return text ? JSON.parse(text) : null;
  } catch {
    return null;
  }
}

export function forgetSession(storage) {
  try {
    storage.removeItem(BURNER_SESSION_KEY);
  } catch {
    /* nothing to forget */
  }
}

export function forgetBurner(storage) {
  try {
    storage.removeItem(BURNER_STORAGE_KEY);
  } catch {
    /* nothing to forget */
  }
}
