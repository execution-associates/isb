// The notice shown wherever domains are edited or listed when the server
// runs without an ingress. Nothing is disabled: an admin may be preparing
// config, but the state must not be silent.
import { Globe } from "lucide-react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { cn } from "@/lib/utils";
import { useIngress } from "./api";
import { DOMAINS_DOC_URL, ingressOff, NO_INGRESS_WARNING } from "./domains";

export function NoIngressText({ className }: { className?: string }) {
  return (
    <span className={className}>
      {NO_INGRESS_WARNING}{" "}
      <a href={DOMAINS_DOC_URL} target="_blank" rel="noreferrer noopener" className="font-medium underline underline-offset-2">
        Domains and ingress
      </a>
    </span>
  );
}

export function NoIngressNotice({ off, className }: { off: boolean; className?: string }) {
  if (!off) return null;
  return (
    <Alert role="status" className={cn("border-warning/40 bg-warning/5", className)}>
      <Globe />
      <AlertDescription>
        <NoIngressText />
      </AlertDescription>
    </Alert>
  );
}

/** Reads the ingress state itself, for places that have only an org. */
export function IngressNotice({ org, className }: { org: string; className?: string }) {
  const ingress = useIngress(org);
  return <NoIngressNotice off={ingressOff(ingress.data)} className={className} />;
}
