/** 画像成果物の表示。同一オリジンの `/files/...` を `<img src>` にそのまま渡す。 */
export function ImageViewer({ src, alt }: { src: string; alt: string }) {
  return <img data-testid="image-viewer" src={src} alt={alt} className="max-w-full" />;
}
