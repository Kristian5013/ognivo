import { describe, expect, it } from "vitest";

import { MAX_BYTES, MAX_PIXELS, aboutBytes, describe as described, fromPicture, imageFiles, imageTokens, plan } from "./images.ts";

describe("an image of a message", () => {
  it("goes as it came in a format the server reads, and drawn anew otherwise", () => {
    expect(plan("image/png", 120_000, 1280, 720)).toEqual({ redraw: false, scale: 1 });
    expect(plan("image/jpeg", 2_000_000, 2048, 2048)).toEqual({ redraw: false, scale: 1 });
    // WebP, AVIF and SVG the server cannot read
    expect(plan("image/webp", 50_000, 640, 480).redraw).toBe(true);
    expect(plan("image/svg+xml", 900, 64, 64).redraw).toBe(true);
    // a PNG too large for the core
    expect(plan("image/png", MAX_BYTES + 1, 1000, 1000).redraw).toBe(true);
  });

  it("is scaled down to the pixels a projector reads", () => {
    const { redraw, scale } = plan("image/png", 3_000_000, 3840, 2160);
    expect(redraw).toBe(true);
    expect(3840 * 2160 * scale * scale).toBeCloseTo(MAX_PIXELS, -3);
  });

  it("counts as the core counts it, and is named as the core names it", () => {
    expect(imageTokens(1280, 720)).toBe(900);
    expect(imageTokens(64, 64)).toBe(256);
    expect(imageTokens(3840, 2160)).toBe(4096);
    expect(aboutBytes(120_000)).toBe("118 KB");
    expect(aboutBytes(2_500_000)).toBe("2.4 MB");
    expect(described({ mime: "image/png", width: 1280, height: 720, bytes: 120_000 })).toBe("PNG, 1280×720, 118 KB");
  });

  it("of the thread is an attachment again when its message is written anew", () => {
    const a = fromPicture({ mime: "image/png", data: "iVBORw0KGgo=", width: 2, height: 1 }, 0);
    expect(a).toMatchObject({ name: "image 1", bytes: 8, width: 2 });
  });

  it("comes from the image files of a paste or a drop, and nothing else", () => {
    const files = [new File(["x"], "a.png", { type: "image/png" }), new File(["y"], "notes.txt", { type: "text/plain" })];
    expect(imageFiles(files).map((f) => f.name)).toEqual(["a.png"]);
    expect(imageFiles(null)).toEqual([]);
  });
});
