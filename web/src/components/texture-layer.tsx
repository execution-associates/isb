import { useEffect } from "react";
import { startGl } from "@/lib/texture-gl";
import { selectRenderer, useTexture } from "@/lib/texture";
import { useTheme } from "@/lib/theme";

/**
 * Mounts the Execution Associates texture: a WebGL canvas behind the page
 * where there is a GPU, else the CSS grain /theme.js switched on before the
 * first paint (index.css and ea-texture.css key off `data-texture-fx` on
 * <html>: "gl", "css" or "none"). Renders nothing itself.
 */
export function TextureLayer() {
  const { theme } = useTheme();
  const texture = useTexture();
  useEffect(() => {
    const root = document.documentElement;
    if (theme !== "ea" || texture === "off") {
      root.dataset.textureFx = "none";
      return;
    }
    const canvas = document.createElement("canvas");
    canvas.className = "ea-fx";
    canvas.setAttribute("aria-hidden", "true");
    const fallBack = () => {
      canvas.remove();
      root.dataset.textureFx = "css";
    };
    const gl = startGl(canvas, texture, fallBack);
    const r = selectRenderer({ theme, texture, webgl: gl !== null, software: gl?.software ?? false });
    if (r === "gl") {
      document.body.prepend(canvas);
      root.dataset.textureFx = "gl";
    } else {
      gl?.stop();
      root.dataset.textureFx = r;
    }
    return () => {
      gl?.stop();
      canvas.remove();
    };
  }, [theme, texture]);
  return null;
}
