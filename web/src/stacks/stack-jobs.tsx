// A compose stack's Jobs tab: the app Jobs tab, for one service at a time.
// Loaded on demand, as an app's is.
import { JobsTab } from "@/jobs/jobs-tab";
import { ServicePicker, useServicePick } from "./stack-tabs";

export function StackJobsTab({ org, name, services }: { org: string; name: string; services: string[] }) {
  const [service, pick] = useServicePick(services);
  return <JobsTab org={org} target={{ stack: name, service }} picker={<ServicePicker services={services} value={service} onChange={pick} />} />;
}
