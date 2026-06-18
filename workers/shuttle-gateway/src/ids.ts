import { badRequest } from "./errors.js";

export function newId(): string {
  return crypto.randomUUID();
}

export function nowIso(): string {
  return new Date().toISOString();
}

const SLUG_RE = /^[a-z0-9][a-z0-9-]*$/;

export function normalizeSlug(input: string): string {
  const slug = input.trim().toLowerCase();
  if (!SLUG_RE.test(slug)) {
    throw badRequest(
      "slug must be lowercase alphanumeric with hyphens and start with a letter or digit",
    );
  }
  return slug;
}

export async function sha256Hex(value: string): Promise<string> {
  const data = new TextEncoder().encode(value);
  const digest = await crypto.subtle.digest("SHA-256", data);
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Mint a random personal access token with a recognizable prefix. */
export function mintToken(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  const base64 = btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
  return `stl_${base64}`;
}
