// PDFs in the window, with pdf.js: pages drawn for the preview and, for PDFs
// without text, as images for the model. Loaded only when a PDF needs it.
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import type { PDFDocumentProxy } from "pdfjs-dist";

let lib: Promise<typeof import("pdfjs-dist")> | null = null;

function pdfjs() {
  lib ??= import("pdfjs-dist").then((m) => {
    m.GlobalWorkerOptions.workerSrc = workerUrl;
    return m;
  });
  return lib;
}

/** Opens a PDF from a URL Relay serves. */
export async function openPdf(url: string): Promise<PDFDocumentProxy> {
  const m = await pdfjs();
  const data = await (await fetch(url)).arrayBuffer();
  return m.getDocument({
    data,
    wasmUrl: new URL("/pdfjs/wasm/", location.href).href,
    // Standard fonts that aren't embedded come from Windows.
    useSystemFonts: true,
    enableXfa: false,
  }).promise;
}

/** Lets go of a PDF and its worker's memory. */
export function closePdf(doc: PDFDocumentProxy): void {
  void doc.loadingTask.destroy();
}

/** Draws page `n` (1-based) so its longer side is `longest` pixels. */
export async function drawPage(doc: PDFDocumentProxy, n: number, longest: number, canvas = document.createElement("canvas")): Promise<HTMLCanvasElement> {
  const page = await doc.getPage(n);
  const base = page.getViewport({ scale: 1 });
  const viewport = page.getViewport({ scale: longest / Math.max(base.width, base.height) });
  canvas.width = Math.round(viewport.width);
  canvas.height = Math.round(viewport.height);
  // Paper is white even where the PDF draws nothing.
  await page.render({ canvas, viewport, background: "#ffffff" }).promise;
  page.cleanup();
  return canvas;
}

/** A page's size at scale 1, for laying out the preview before it's drawn. */
export async function pageSize(doc: PDFDocumentProxy, n: number): Promise<{ width: number; height: number }> {
  const page = await doc.getPage(n);
  const v = page.getViewport({ scale: 1 });
  return { width: v.width, height: v.height };
}

/** The PDF's text, page by page, and how many letters and digits it has. */
export async function pdfText(doc: PDFDocumentProxy): Promise<{ text: string; letters: number }> {
  const pages: string[] = [];
  let letters = 0;
  for (let n = 1; n <= doc.numPages; n++) {
    const page = await doc.getPage(n);
    const content = await page.getTextContent();
    const line = content.items.map((it) => ("str" in it ? it.str + (it.hasEOL ? "\n" : "") : "")).join("");
    letters += (line.match(/[\p{L}\p{N}]/gu) ?? []).length;
    pages.push(line);
    page.cleanup();
  }
  return { text: pages.join("\n\n"), letters };
}

/** Under this many letters a page, a PDF counts as having no text (a scan's page numbers and stamps don't count). */
export const LETTERS_PER_PAGE = 20;
