import { useEffect, useMemo, useState } from "react";
import { api } from "../store";
import type { Attachment, AttachmentDraft } from "../types";

type TileAttachment = Attachment | AttachmentDraft;

interface Props {
  attachment: TileAttachment;
  onOpen?: (id: number) => void;
  onRemove?: () => void;
  draft?: boolean;
  getDragPaths?: () => string[];
  onDragStateChange?: (dragging: boolean) => void;
  selected?: boolean;
  onSelect?: (event: React.MouseEvent, id: number) => void;
}

function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(bytes < 10 * 1024 * 1024 ? 1 : 0)} MB`;
}

function extension(name: string) {
  const value = name.split(".").pop();
  return value && value !== name ? value.slice(0, 5).toUpperCase() : "FILE";
}

function fileUrl(path: string) {
  const normalized = path.replace(/\\/g, "/");
  return new URL(`file://${normalized.startsWith("/") ? "" : "/"}${normalized}`).href;
}

const PNG_SIGNATURE_BYTES = 8;
const PNG_DENSITY = "pHYs";
const DRAG_ICON_CACHE = new Map<string, string>();
const CRC_TABLE = new Uint32Array(256).map((_, index) => {
  let value = index;
  for (let bit = 0; bit < 8; bit += 1) {
    value = (value & 1) !== 0 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
  }
  return value >>> 0;
});

function crc32(bytes: Uint8Array) {
  let value = 0xffffffff;
  for (const byte of bytes) value = CRC_TABLE[(value ^ byte) & 0xff] ^ (value >>> 8);
  return (value ^ 0xffffffff) >>> 0;
}

function pngChunkType(bytes: Uint8Array, offset: number) {
  return String.fromCharCode(...bytes.subarray(offset + 4, offset + 8));
}

/**
 * Canvas PNGs do not advertise their backing scale. AppKit otherwise treats a
 * 2× preview as twice as large instead of as a Retina image, so add the PNG
 * density metadata that preserves its logical drag size.
 */
function withPngDensity(dataUrl: string, dpi: number) {
  try {
    const encoded = dataUrl.slice(dataUrl.indexOf(",") + 1);
    const binary = atob(encoded);
    const source = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    const pixelsPerMeter = Math.round(dpi / 0.0254);
    const density = new Uint8Array(21);
    const densityView = new DataView(density.buffer);
    densityView.setUint32(0, 9, false);
    density.set([0x70, 0x48, 0x59, 0x73], 4);
    densityView.setUint32(8, pixelsPerMeter, false);
    densityView.setUint32(12, pixelsPerMeter, false);
    density[16] = 1;
    densityView.setUint32(17, crc32(density.subarray(4, 17)), false);

    const chunks: Uint8Array[] = [source.subarray(0, PNG_SIGNATURE_BYTES)];
    let offset = PNG_SIGNATURE_BYTES;
    while (offset + 12 <= source.length) {
      const length = new DataView(source.buffer, source.byteOffset + offset, 4).getUint32(0, false);
      const end = offset + 12 + length;
      if (end > source.length) return dataUrl;
      const type = pngChunkType(source, offset);
      if (type !== PNG_DENSITY) chunks.push(source.subarray(offset, end));
      if (type === "IHDR") chunks.push(density);
      offset = end;
    }

    const outputLength = chunks.reduce((total, chunk) => total + chunk.length, 0);
    const output = new Uint8Array(outputLength);
    let cursor = 0;
    for (const chunk of chunks) {
      output.set(chunk, cursor);
      cursor += chunk.length;
    }
    let outputBinary = "";
    for (let index = 0; index < output.length; index += 0x8000) {
      outputBinary += String.fromCharCode(...output.subarray(index, index + 0x8000));
    }
    return `data:image/png;base64,${btoa(outputBinary)}`;
  } catch {
    return dataUrl;
  }
}

export function makeDragIcon(
  label: string,
  count = 1,
  kind: "prompt" | "files" = "files",
) {
  const logicalWidth = kind === "prompt" ? 184 : 126;
  const logicalHeight = 76;
  const retinaScale = navigator.userAgent.includes("Mac")
    ? Math.min(2, Math.max(1, window.devicePixelRatio || 1))
    : 1;
  const normalizedLabel = label.replace(/\s+/g, " ").trim();
  const displayLabel =
    kind === "prompt"
      ? normalizedLabel.length > 25
        ? `${normalizedLabel.slice(0, 24)}…`
        : normalizedLabel
      : normalizedLabel.length > 14
        ? `${normalizedLabel.slice(0, 13)}…`
        : normalizedLabel;
  const cacheKey = `${kind}|${count}|${retinaScale}|${displayLabel}`;
  const cached = DRAG_ICON_CACHE.get(cacheKey);
  if (cached) return cached;
  const canvas = document.createElement("canvas");
  canvas.width = Math.round(logicalWidth * retinaScale);
  canvas.height = Math.round(logicalHeight * retinaScale);
  const context = canvas.getContext("2d");
  if (!context) return "";

  context.scale(retinaScale, retinaScale);
  context.clearRect(0, 0, logicalWidth, logicalHeight);
  context.shadowColor = "rgba(30, 38, 34, 0.16)";
  context.shadowBlur = 12;
  context.shadowOffsetY = 4;
  const finish = () => {
    const image = canvas.toDataURL("image/png");
    const result = retinaScale > 1 ? withPngDensity(image, 72 * retinaScale) : image;
    if (DRAG_ICON_CACHE.size >= 32) {
      const oldest = DRAG_ICON_CACHE.keys().next().value;
      if (oldest) DRAG_ICON_CACHE.delete(oldest);
    }
    DRAG_ICON_CACHE.set(cacheKey, result);
    return result;
  };

  if (kind === "prompt") {
    context.beginPath();
    context.roundRect(8, 7, 168, 58, 12);
    context.fillStyle = "#fcfcfa";
    context.fill();
    context.shadowColor = "transparent";
    context.strokeStyle = "rgba(31, 35, 32, 0.14)";
    context.lineWidth = 1;
    context.stroke();
    context.beginPath();
    context.arc(24, 26, 6, 0, Math.PI * 2);
    context.strokeStyle = "rgba(65, 72, 68, 0.66)";
    context.lineWidth = 1.5;
    context.stroke();
    context.fillStyle = "#242724";
    context.font = "500 11px -apple-system, BlinkMacSystemFont, sans-serif";
    context.fillText(displayLabel, 38, 30, 126);
    context.fillStyle = "#7b817d";
    context.font = "500 9px -apple-system, BlinkMacSystemFont, sans-serif";
    context.fillText("Prompt", 38, 46);
    return finish();
  }

  const cards = Math.min(count, 3);
  for (let index = cards - 1; index >= 0; index -= 1) {
    const offset = index * 4;
    context.beginPath();
    context.roundRect(10 + offset, 7 + offset, 88, 58, 10);
    context.fillStyle = index === 0 ? "#fcfcfa" : "#e5e9e6";
    context.fill();
    context.shadowColor = "transparent";
    context.strokeStyle = "rgba(31, 35, 32, 0.14)";
    context.lineWidth = 1;
    context.stroke();
  }
  context.fillStyle = "#20211f";
  context.font = "600 9px -apple-system, BlinkMacSystemFont, sans-serif";
  context.fillText(displayLabel, 19, 40, 68);

  if (count > 1) {
    context.beginPath();
    context.arc(101, 18, 12, 0, Math.PI * 2);
    context.fillStyle = "#e5484d";
    context.fill();
    context.fillStyle = "#fff";
    context.font = "700 11px -apple-system, BlinkMacSystemFont, sans-serif";
    context.textAlign = "center";
    context.textBaseline = "middle";
    context.fillText(String(count), 101, 18);
  }
  return finish();
}

export default function AttachmentTile({
  attachment,
  onOpen,
  onRemove,
  draft,
  getDragPaths,
  onDragStateChange,
  selected = false,
  onSelect,
}: Props) {
  const storedId = "id" in attachment ? attachment.id : null;
  const suppliedPreview = "preview" in attachment ? attachment.preview : null;
  const [preview, setPreview] = useState<string | null>(suppliedPreview);
  const image = attachment.mediaType.startsWith("image/");
  const detail = useMemo(
    () => `${attachment.name} · ${formatBytes(attachment.size)}`,
    [attachment.name, attachment.size],
  );

  useEffect(() => {
    setPreview(suppliedPreview);
    if (!image || storedId === null || suppliedPreview) return;
    let disposed = false;
    void api
      .getAttachmentPreview(storedId)
      .then((value) => {
        if (!disposed) setPreview(value);
      })
      .catch(() => {
        if (!disposed) setPreview(null);
      });
    return () => {
      disposed = true;
    };
  }, [image, storedId, suppliedPreview]);

  const content = (
    <>
      {preview ? (
        <img src={preview} alt="" draggable={false} />
      ) : (
        <span className="attachment-file-icon" aria-hidden>
          <svg viewBox="0 0 24 28">
            <path d="M4 1.5h10l6 6V26.5H4z" />
            <path d="M14 1.5v6h6" />
          </svg>
          <span>{extension(attachment.name)}</span>
        </span>
      )}
      <span className="attachment-label">
        <strong>{attachment.name}</strong>
        <small>{formatBytes(attachment.size)}</small>
      </span>
    </>
  );

  return (
    <div
      className={`attachment-tile${draft ? " draft" : ""}${preview ? " has-preview" : ""}${selected ? " selected" : ""}`}
    >
      {storedId !== null && onOpen ? (
        <button
          className="attachment-open"
          title={onSelect ? `Select ${detail} · Double-click to open` : `Open ${detail}`}
          aria-label={`${selected ? "Selected" : "Select"} attachment ${detail}`}
          aria-pressed={onSelect ? selected : undefined}
          draggable={!!getDragPaths}
          onDragStart={(event) => {
            const dragPaths = getDragPaths?.() ?? [];
            if (!dragPaths?.length) return;
            const urls = dragPaths.map(fileUrl);
            event.dataTransfer.effectAllowed = "copy";
            event.dataTransfer.setData("text/uri-list", urls.join("\r\n"));
            event.dataTransfer.setData("text/plain", dragPaths.join("\n"));
            if (dragPaths.length === 1) {
              event.dataTransfer.setData(
                "DownloadURL",
                `${attachment.mediaType}:${attachment.name}:${urls[0]}`,
              );
            }
            event.stopPropagation();
            event.preventDefault();
            onDragStateChange?.(true);
            void api
              .startFileDrag(
                dragPaths,
                makeDragIcon(
                  dragPaths.length === 1 ? attachment.name : `${dragPaths.length} files`,
                  dragPaths.length,
                ),
              )
              .finally(() => onDragStateChange?.(false));
          }}
          onDragEnd={() => onDragStateChange?.(false)}
          onClick={(event) => {
            event.stopPropagation();
            if (onSelect) onSelect(event, storedId);
            else onOpen(storedId);
          }}
          onDoubleClick={(event) => {
            event.stopPropagation();
            onOpen(storedId);
          }}
        >
          {content}
        </button>
      ) : (
        <div className="attachment-open" title={detail}>
          {content}
        </div>
      )}
      {onRemove && (
        <button
          className="attachment-remove"
          title={`Remove ${attachment.name}`}
          aria-label={`Remove attachment ${attachment.name}`}
          onClick={(event) => {
            event.stopPropagation();
            onRemove();
          }}
        >
          <svg viewBox="0 0 12 12" width="8" height="8" aria-hidden>
            <path d="M2.5 2.5l7 7m0-7l-7 7" />
          </svg>
        </button>
      )}
    </div>
  );
}
