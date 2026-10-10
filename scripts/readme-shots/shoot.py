# /// script
# requires-python = ">=3.11"
# dependencies = ["playwright==1.55.0"]
# ///
"""Run as ubuntu inside the preview VM, after seed.sh and terminals.sh.
Screenshots each README cell at 1280x800 (2x): web UI pages from the
preview's own vite on 127.0.0.1:5173, which signs in as the dev superadmin,
and the terminal captures rendered by ansi2html.py.

usage: uv run shoot.py OUT_DIR [NAME...]
"""
import subprocess
import sys
from pathlib import Path

from playwright.sync_api import Page, sync_playwright

UI = "http://127.0.0.1:5173"
TERM = Path.home() / "readme-shots" / "term"


# The MCP page prints the address it is open at, so it is opened as PUBLIC
# and every request is answered from the preview's UI.
PUBLIC = "https://isb.acme.dev"


def via_ui(route) -> None:
    req = route.request
    headers = dict(req.headers)
    if "origin" in headers:
        headers["origin"] = UI
    try:
        res = route.fetch(url=req.url.replace(PUBLIC, UI), headers=headers, timeout=8000)
        route.fulfill(response=res)
    except Exception:
        # The live event stream never completes; the page shows "Connecting".
        route.abort()


def scroll_to(text: str, above: int = 24):
    # Brings TEXT to the top of whatever scrolls it, ABOVE pixels down.
    def step(page: Page) -> None:
        page.get_by_text(text, exact=True).first.evaluate(
            """(el, above) => {
                el.scrollIntoView({block: 'start'});
                let s = el.parentElement;
                while (s && s !== document.body) {
                    if (/(auto|scroll)/.test(getComputedStyle(s).overflowY) && s.scrollHeight > s.clientHeight) break;
                    s = s.parentElement;
                }
                (s && s !== document.body ? s : document.scrollingElement).scrollBy(0, -above);
            }""",
            above,
        )
        page.wait_for_timeout(600)

    return step


def open_first_deployment(page: Page) -> None:
    # The newest deployment's page: its build and rollout log, with Rollback.
    page.locator("a[href*='/deployments/']").first.click()
    page.wait_for_timeout(2500)


def workspace_terminal(page: Page) -> None:
    # Attach the web terminal (a herdr session in the workspace) and look
    # around the org with the workspace's own token.
    # A session attached before reattaches by itself; exact, or "Connect"
    # also matches Disconnect.
    connect = page.get_by_role("button", name="Connect", exact=True)
    if connect.count():
        connect.click()
    page.locator(".xterm").first.wait_for()
    page.wait_for_timeout(3000)
    page.locator(".xterm").first.click()
    # The session outlives runs: a fresh login shell has the PATH seed.sh set and the token.
    for cmd in ["exec bash -l", "clear", "isb app ls", "isb app previews ls web"]:
        page.keyboard.type(cmd)
        page.keyboard.press("Enter")
        # A login shell takes a few seconds to come up.
        page.wait_for_timeout(7000 if cmd.startswith("exec") else 2500)
    page.evaluate("""() => document.querySelectorAll('*').forEach(e => { if (e.scrollTop) e.scrollTop = 0; })""")
    page.wait_for_timeout(500)


def search_templates(page: Page) -> None:
    # One app from each catalog side by side: isb's own, Dokploy's, Coolify's.
    # The Dokploy and Coolify catalogs are fetched on demand: wait for cards.
    page.get_by_text("dokploy", exact=False).first.wait_for(timeout=90000)
    page.get_by_placeholder("Search templates").fill("n8n")
    page.get_by_text("Coolify", exact=True).first.wait_for(timeout=30000)
    page.wait_for_timeout(1500)


# name -> (url, optional step after load)
SHOTS = {
    "sandbox": (f"file://{TERM}/sandbox.html", None),
    "agents": (f"{PUBLIC}/orgs/acme/agents", scroll_to("Install it", 140)),
    "workspace": (f"{UI}/orgs/acme/workspace", workspace_terminal),
    "deploy": (f"{UI}/orgs/acme/apps/web/deployments", open_first_deployment),
    "templates": (f"{UI}/orgs/acme/templates", search_templates),
    "previews": (f"{UI}/orgs/acme/apps/web/previews", None),
    "secrets": (f"{UI}/orgs/acme/secrets", scroll_to("github-token", 40)),
    "domains": (f"{UI}/orgs/acme/apps/web/domains", None),
    "ssh": (f"file://{TERM}/ssh.html", None),
    "databases": (f"{UI}/orgs/acme/backups", None),
    "uptime": (f"{UI}/orgs/acme/uptime", None),
    "tui": (f"file://{TERM}/tui.html", None),
}


def main() -> None:
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    names = sys.argv[2:] or list(SHOTS)
    with sync_playwright() as p:
        browser = p.chromium.launch()
        ctx = browser.new_context(
            viewport={"width": 1280, "height": 800}, device_scale_factor=2, color_scheme="dark"
        )
        ctx.route(PUBLIC + "/**", via_ui)
        for name in names:
            url, step = SHOTS[name]
            page = ctx.new_page()
            page.goto(url, wait_until="load")
            # The UI streams live data, so the network never goes idle: give
            # queries and charts a moment instead.
            page.wait_for_timeout(3000)
            if step:
                step(page)
            png = out / f"{name}.png"
            page.screenshot(path=str(png))
            page.close()
            # 1600x1000 WebP: sharp at the grid's cell width on a 2x screen,
            # a tenth of the PNG's weight.
            subprocess.run(
                ["cwebp", "-quiet", "-q", "88", "-resize", "1600", "1000", str(png), "-o", str(out / f"{name}.webp")],
                check=True,
            )
            png.unlink()
            print(f"shot {name}")
        browser.close()


if __name__ == "__main__":
    main()
