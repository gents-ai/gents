import type {
  AttachmentView,
  RequestAttachment,
} from "@source-inc/gents-desktop-client";

export async function attachmentMetadata(
  content: RequestAttachment,
): Promise<AttachmentView> {
  const bytes = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(content.data),
  );
  return {
    name: content.name,
    mediaType: content.mediaType,
    digest: Array.from(new Uint8Array(bytes), (byte) =>
      byte.toString(16).padStart(2, "0"),
    ).join(""),
    size:
      (content.data.length / 4) * 3 -
      (content.data.endsWith("==") ? 2 : content.data.endsWith("=") ? 1 : 0),
  };
}
