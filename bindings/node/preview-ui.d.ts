/** Create an object URL suitable for an `<img>` src. */
export declare function createSvgPreviewUrl(svg: string): string

/** Create a validated self-contained data URL for cross-process previews. */
export declare function createSvgPreviewDataUrl(svg: string): string

/** Revoke an object URL previously returned by `createSvgPreviewUrl()`. */
export declare function revokeSvgPreviewUrl(url: string): void

/** Copy the exact SVG XML source as plain text. */
export declare function copySvgSourceToClipboard(svg: string): Promise<void>

/**
 * Copy SVG as `image/svg+xml` when supported, with an exact-source text
 * fallback. Call this from a user gesture such as a button click.
 *
 * The resolved value reports the MIME type that was actually copied.
 */
export declare function copySvgToClipboard(
  svg: string,
): Promise<'image/svg+xml' | 'text/plain'>
