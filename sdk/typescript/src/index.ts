/**
 * TypeScript SDK for isb: declarative incus sandboxes (containers and VMs).
 * A thin client of `isb rpc` (docs/rpc.md): one long-lived subprocess per
 * {@link Client}, line-delimited JSON over its stdin/stdout.
 */

export { findIsb, platformPackage } from "./binary.js";
export {
  type BindOptions,
  type HostPortOptions,
  type NamedOptions,
  NamedVolumeMode,
  PortBinding,
  type PortOptions,
  Volume,
} from "./builders.js";
export {
  type CallOptions,
  Client,
  type ClientOptions,
  defaultClient,
  type EventHandler,
  type Hello,
  PROTOCOL,
  setDefaultClient,
} from "./client.js";
export {
  AlreadyExistsError,
  ClientClosedError,
  ConnectError,
  errorFromRpc,
  InvalidError,
  IsbError,
  IsbTimeoutError,
  NotFoundError,
  NotReadyError,
  ProcessExitedError,
  ProtocolError,
  type RpcErrorObject,
} from "./errors.js";
export {
  type Argv,
  type ExecEvent,
  type ExecOptions,
  ExecOutput,
  ExecProcess,
  type ExecStreamOptions,
} from "./exec.js";
export {
  type DownOptions,
  type LoadOptions,
  Project,
  type ProjectPlanOptions,
  type UpOptions,
} from "./project.js";
export {
  type CreateOptions,
  type EnsureOptions,
  type ListOptions,
  type PlanOptions,
  type ProgressHandler,
  Sandbox,
  type SandboxDefaults,
  type SandboxSpec,
  type SpecOptions,
  type StopOptions,
  type WaitReadyOptions,
} from "./sandbox.js";
export type * as spec from "./spec.js";
export type {
  BoolOrString,
  ComposeFile,
  ExecDefaults,
  IdmapMap,
  IdmapMode,
  IdmapRaw,
  IdmapSpec,
  InstanceType,
  IntOrString,
  NamedVolumeSpec,
  PortBind,
  PortSpec,
  ReadyCheck,
  Scalar,
  VolumeSpec,
} from "./spec.js";
export type { Spawner } from "./transport.js";
export {
  type Action,
  type ActionKind,
  type ApplyReport,
  type Plan,
  type Props,
  type PruneItem,
  planHasChanges,
  type SandboxInfo,
  type ServiceReport,
  type VolumeCreated,
  type VolumeInfo,
} from "./types.js";
export type { Duration } from "./util.js";
export { type PoolOptions, type PruneOptions, prune, volumes } from "./volumes.js";
