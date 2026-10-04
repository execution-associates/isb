// Copy text to the clipboard. The async Clipboard API only exists in a
// secure context (https or localhost); over plain http, such as a tailnet
// address, fall back to a hidden textarea and execCommand("copy").
export async function copyText(text: string): Promise<void> {
  if (window.isSecureContext && navigator.clipboard) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.opacity = "0";
  area.style.pointerEvents = "none";
  document.body.append(area);
  const focused = document.activeElement as HTMLElement | null;
  area.select();
  try {
    // execCommand is deprecated but is the only clipboard write outside a secure context.
    if (!document.execCommand("copy")) throw new Error("copy refused");
  } finally {
    area.remove();
    focused?.focus();
  }
}
