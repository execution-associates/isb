// Applies the saved theme before the first paint. It is a file, not an
// inline script, because the Content-Security-Policy allows no inline code.
(function () {
  var t = null;
  try {
    t = localStorage.getItem("isb-theme");
  } catch (e) {
    /* storage blocked */
  }
  var dark = t === "dark" || (t !== "light" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  if (dark) document.documentElement.classList.add("dark");
})();
