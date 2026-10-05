// How an app names its image, and the slips the server would refuse or
// misread. The server looks the image up in its registry when it is saved
// (crates/isb-core/src/image_check.rs); this only catches what needs no network.

/** The prefixes isb reads before the first colon (crates/isb-core/src/plan.rs, ImageSource::parse). */
const PREFIXES = ["docker", "ghcr", "quay", "oci", "registry", "images", "ubuntu", "ubuntu-daily", "ubuntu-minimal"];

export const IMAGE_HINT =
  "Docker Hub images start with docker:, as in docker:nginx:1.27, docker:traefik/whoami. Also ghcr:owner/app:tag, registry:app:tag (this org's builds), or the name of an image on this host. Looked up in its registry when saved.";

export const IMAGE_PLACEHOLDER = "docker:traefik/whoami";

/** Why `image` cannot be right, or null. */
export function imageProblem(image: string): string | null {
  const s = image.trim();
  if (!s) return "Enter an image, e.g. docker:nginx:1.27.";
  if (/\s/.test(s)) return "An image reference has no spaces.";
  const colon = s.indexOf(":");
  if (colon < 0) return null;
  const prefix = s.slice(0, colon);
  if (PREFIXES.includes(prefix)) return null;
  // `traefik:whoami`: a Docker Hub reference without docker:, and maybe a
  // colon where a slash was meant.
  const rest = s.slice(colon + 1);
  if (!prefix.includes("/") && !rest.includes("/") && !rest.includes(":")) {
    return `Start with the registry: docker:${prefix}/${rest} (the image ${prefix}/${rest}) or docker:${s} (the image ${prefix}, tag ${rest}).`;
  }
  return `Start with the registry, e.g. docker:${s}.`;
}

/** A gentler note for a name the server will read as an image on this host. */
export function imageNote(image: string): string | null {
  const s = image.trim();
  if (!s.includes(":") && s.includes("/")) return `Without a prefix this is an image on this host. From Docker Hub, it is docker:${s}.`;
  return null;
}
