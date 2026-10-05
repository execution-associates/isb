// Applies the saved theme before the first paint. It is a file, not an
// inline script, because the Content-Security-Policy allows no inline code.
// It mirrors src/lib/theme.ts: no saved choice (or an unknown one) means the
// Execution Associates theme, which is dark.
(function () {
  var t = null;
  try {
    t = localStorage.getItem("isb-theme");
  } catch {
    /* storage blocked */
  }
  if (t !== "light" && t !== "dark" && t !== "system") t = "ea";
  var dark = t === "ea" || t === "dark" || (t === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  if (dark) document.documentElement.classList.add("dark");
  if (t === "ea") document.documentElement.classList.add("ea");
  // The EA theme's texture (lib/texture.ts): the CSS grain shows from the
  // first paint; the WebGL layer takes over once it is up.
  document.documentElement.dataset.textureFx = t === "ea" ? "css" : "none";
})();
