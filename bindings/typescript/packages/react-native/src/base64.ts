// Minimal base64 encode/decode for the React Native binding.
// The native modules take binary data as base64 strings (the
// TurboModule bridge has no byte-array type), so the JS side
// needs this with no extra dependency. Standard alphabet with
// padding, no line breaks.

const ALPHABET =
  "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const LOOKUP = new Map<string, number>(
  [...ALPHABET].map((char, index) => [char, index]),
);

/** Encode bytes as a base64 string. */
export function encodeBase64(data: Uint8Array): string {
  let out = "";
  for (let i = 0; i < data.length; i += 3) {
    const b0 = data[i] as number;
    const b1 = i + 1 < data.length ? (data[i + 1] as number) : 0;
    const b2 = i + 2 < data.length ? (data[i + 2] as number) : 0;
    out += ALPHABET[b0 >> 2] as string;
    out += ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as string;
    out +=
      i + 1 < data.length
        ? (ALPHABET[((b1 & 0x0f) << 2) | (b2 >> 6)] as string)
        : "=";
    out += i + 2 < data.length ? (ALPHABET[b2 & 0x3f] as string) : "=";
  }
  return out;
}

/** Decode a base64 string to bytes. Throws on bad input. */
export function decodeBase64(text: string): Uint8Array {
  const clean = text.replace(/[\r\n ]/g, "");
  if (clean.length % 4 !== 0) {
    throw new Error("invalid base64: length is not a multiple of 4");
  }
  const padding = clean.endsWith("==") ? 2 : clean.endsWith("=") ? 1 : 0;
  const out = new Uint8Array((clean.length / 4) * 3 - padding);
  let offset = 0;
  for (let i = 0; i < clean.length; i += 4) {
    const quad = [0, 1, 2, 3].map((k) => {
      const char = clean[i + k] as string;
      if (char === "=") {
        return 0;
      }
      const value = LOOKUP.get(char);
      if (value === undefined) {
        throw new Error(`invalid base64 character: ${char}`);
      }
      return value;
    });
    out[offset] = ((quad[0] as number) << 2) | ((quad[1] as number) >> 4);
    offset += 1;
    if (clean[i + 2] !== "=") {
      out[offset] =
        (((quad[1] as number) & 0x0f) << 4) | ((quad[2] as number) >> 2);
      offset += 1;
    }
    if (clean[i + 3] !== "=") {
      out[offset] = (((quad[2] as number) & 0x03) << 6) | (quad[3] as number);
      offset += 1;
    }
  }
  return out;
}
