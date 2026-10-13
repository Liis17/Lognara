// Преобразование options webauthn-rs (base64url) для navigator.credentials и обратно.

function decode(value: string): ArrayBuffer {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(base64 + "=".repeat((4 - (base64.length % 4)) % 4));
  return Uint8Array.from(binary, (c) => c.charCodeAt(0)).buffer;
}

function encode(buffer: ArrayBuffer | null): string | null {
  if (buffer === null) return null;
  const binary = String.fromCharCode(...new Uint8Array(buffer));
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

interface Descriptor {
  id: string;
  type: string;
  transports?: string[];
}

function descriptors(list: Descriptor[] | undefined): PublicKeyCredentialDescriptor[] | undefined {
  return list?.map((item) => ({ ...item, id: decode(item.id) }) as PublicKeyCredentialDescriptor);
}

type Json = any;

export async function createPasskey(options: Json): Promise<Json> {
  const publicKey = options.publicKey;
  const credential = (await navigator.credentials.create({
    publicKey: {
      ...publicKey,
      challenge: decode(publicKey.challenge),
      user: { ...publicKey.user, id: decode(publicKey.user.id) },
      excludeCredentials: descriptors(publicKey.excludeCredentials),
    },
  })) as PublicKeyCredential;
  const response = credential.response as AuthenticatorAttestationResponse;
  return {
    id: credential.id,
    rawId: encode(credential.rawId),
    type: credential.type,
    response: {
      attestationObject: encode(response.attestationObject),
      clientDataJSON: encode(response.clientDataJSON),
      transports: response.getTransports?.(),
    },
    extensions: {},
  };
}

export async function getPasskey(options: Json): Promise<Json> {
  const publicKey = options.publicKey;
  const credential = (await navigator.credentials.get({
    publicKey: {
      ...publicKey,
      challenge: decode(publicKey.challenge),
      allowCredentials: descriptors(publicKey.allowCredentials),
    },
  })) as PublicKeyCredential;
  const response = credential.response as AuthenticatorAssertionResponse;
  return {
    id: credential.id,
    rawId: encode(credential.rawId),
    type: credential.type,
    response: {
      authenticatorData: encode(response.authenticatorData),
      clientDataJSON: encode(response.clientDataJSON),
      signature: encode(response.signature),
      userHandle: encode(response.userHandle),
    },
    extensions: {},
  };
}
