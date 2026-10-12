/** The glyph atlas's pages as browser canvases. */
import { PAGE_SIZE } from './atlas-layout';
import { glyphScale } from './glyph-fit';
import type { GlyphRect, GlyphStyle, PageSource } from './glyph-atlas';

/** One page: its canvas, which the renderer copies from, and the context to draw on it. */
export interface CanvasPage {
  canvas: HTMLCanvasElement;
  context: CanvasRenderingContext2D;
}

export const canvasPages: PageSource<CanvasPage> = {
  createPage: () => {
    const canvas = document.createElement('canvas');
    canvas.width = PAGE_SIZE;
    canvas.height = PAGE_SIZE;
    const context = canvas.getContext('2d');
    return context === null ? null : { canvas, context };
  },
  rasterize: ({ context }: CanvasPage, slot: GlyphRect, text: string, style: GlyphStyle) => {
    context.save();
    context.beginPath();
    context.rect(slot.x, slot.y, slot.width, slot.height);
    context.clip();
    context.clearRect(slot.x, slot.y, slot.width, slot.height);
    context.font = style.font;
    context.fillStyle = style.color;
    context.textBaseline = 'alphabetic';
    const scale = glyphScale(context.measureText(text).width, slot.width);
    context.translate(slot.x, slot.y + style.baseline);
    context.scale(scale, scale);
    context.fillText(text, 0, 0);
    context.restore();
  },
};
