// WebAuthn glue: the server sends options in the JSON form of
// PublicKeyCredential.parse{Creation,Request}OptionsFromJSON() and takes
// credential.toJSON() back. Browsers without those (older Safari) get the
// same conversion by hand.

export function b64urlToBytes(s: string): Uint8Array<ArrayBuffer> {
  const pad = "=".repeat((4 - (s.length % 4)) % 4);
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + pad);
  const out = new Uint8Array(new ArrayBuffer(bin.length));
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function bytesToB64url(b: ArrayBuffer | Uint8Array): string {
  const bytes = b instanceof Uint8Array ? b : new Uint8Array(b);
  let bin = "";
  for (const x of bytes) bin += String.fromCharCode(x);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

type Json = Record<string, unknown>;
type Desc = { id: string; type: string; transports?: string[] };

const descs = (l: unknown) =>
  ((l as Desc[] | undefined) ?? []).map((d) => ({
    ...d,
    id: b64urlToBytes(d.id),
    transports: d.transports as AuthenticatorTransport[] | undefined,
  })) as PublicKeyCredentialDescriptor[];

interface PKCStatic {
  parseCreationOptionsFromJSON?: (o: Json) => PublicKeyCredentialCreationOptions;
  parseRequestOptionsFromJSON?: (o: Json) => PublicKeyCredentialRequestOptions;
}
const PKC = () => (window.PublicKeyCredential as unknown as PKCStatic | undefined) ?? {};

export function creationOptions(o: Json): PublicKeyCredentialCreationOptions {
  const parse = PKC().parseCreationOptionsFromJSON;
  if (parse) return parse(o);
  const user = o.user as { id: string; name: string; displayName: string };
  return {
    ...(o as unknown as PublicKeyCredentialCreationOptions),
    challenge: b64urlToBytes(o.challenge as string),
    user: { ...user, id: b64urlToBytes(user.id) },
    excludeCredentials: descs(o.excludeCredentials),
  };
}

export function requestOptions(o: Json): PublicKeyCredentialRequestOptions {
  const parse = PKC().parseRequestOptionsFromJSON;
  if (parse) return parse(o);
  return {
    ...(o as unknown as PublicKeyCredentialRequestOptions),
    challenge: b64urlToBytes(o.challenge as string),
    allowCredentials: descs(o.allowCredentials),
  };
}

/** `credential.toJSON()`, or the same by hand. */
export function credentialJSON(c: PublicKeyCredential): Json {
  const withJSON = c as PublicKeyCredential & { toJSON?: () => Json };
  if (typeof withJSON.toJSON === "function") {
    try {
      return withJSON.toJSON();
    } catch {
      // fall through: some password managers' credentials throw here
    }
  }
  const r = c.response as AuthenticatorResponse & {
    attestationObject?: ArrayBuffer;
    authenticatorData?: ArrayBuffer;
    signature?: ArrayBuffer;
    userHandle?: ArrayBuffer | null;
    getTransports?: () => string[];
  };
  const response: Json = { clientDataJSON: bytesToB64url(r.clientDataJSON) };
  if (r.attestationObject) {
    response.attestationObject = bytesToB64url(r.attestationObject);
    response.transports = r.getTransports?.() ?? [];
  }
  if (r.authenticatorData) response.authenticatorData = bytesToB64url(r.authenticatorData);
  if (r.signature) response.signature = bytesToB64url(r.signature);
  if (r.userHandle) response.userHandle = bytesToB64url(r.userHandle);
  return { id: c.id, rawId: bytesToB64url(c.rawId), type: c.type, response };
}

export const passkeysSupported = () =>
  typeof window !== "undefined" && typeof window.PublicKeyCredential === "function";

/** Plain words for a failed browser ceremony, or null when the user just cancelled. */
export function webauthnErrorMessage(e: unknown): string | null {
  const name = (e as { name?: string })?.name;
  if (name === "NotAllowedError" || name === "AbortError") return null;
  if (name === "InvalidStateError") return "This passkey is already registered here.";
  if (name === "SecurityError")
    return "This browser refused the passkey: the page's address does not match the server's public URL.";
  if (name === "NotSupportedError") return "This browser or device does not support passkeys.";
  return (e as Error)?.message || "The passkey prompt failed.";
}
