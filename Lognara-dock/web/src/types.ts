export type LogLevel = "trace" | "debug" | "info" | "warn" | "error" | "fatal" | "unknown";

/** JSON-контракт core; timestamp сохраняется с исходной наносекундной точностью. */
export interface LogEvent {
  id: string;
  timestamp: string;
  ingested_at: string;
  environment: string | null;
  server: string;
  backend: string;
  service: string;
  service_instance: string | null;
  action: string | null;
  level: LogLevel;
  message: string;
  trace_id: string | null;
  span_id: string | null;
  parent_span_id: string | null;
  request_id: string | null;
  attributes: Record<string, unknown>;
}

export type QueryFilters = Record<string, string[]>;
export interface TextQuery {
  query: string;
  mode: "all" | "phrase";
}
export interface SearchRequest {
  from: string;
  to: string;
  filters: QueryFilters;
  text?: TextQuery;
  limit: number;
  cursor?: string;
}
export interface SearchResponse {
  events: LogEvent[];
  next_cursor: string | null;
  as_of: string;
}
export interface HistogramBucket {
  timestamp: string;
  count: number;
  levels?: Record<LogLevel, number>;
}
export interface HistogramResponse {
  buckets: HistogramBucket[];
  as_of: string;
}
export interface Group {
  values: Record<string, string | null>;
  count: number;
}
export interface GroupResponse {
  groups: Group[];
  as_of: string;
}

export interface Duration {
  p95_ms: number | null;
  measured: number;
  coverage: number;
  method: "tdigest";
}
export interface SummaryResponse {
  as_of: string;
  total: number;
  buckets: (HistogramBucket & { levels: Record<LogLevel, number> })[];
  levels: Record<LogLevel, number>;
  error_services: { service: string | null; count: number }[];
  duration: Duration;
  actions: { action: string | null; count: number; duration: Duration }[];
  page: SearchResponse | null;
}

export type PatternVariableType = "ip" | "uuid" | "id" | "duration" | "hex";
export type PatternPart = { kind: "literal"; text: string } |
  { kind: "variable"; name: string; variable_type: PatternVariableType };
export type PatternSort = "count" | "growth" | "last_seen";
export interface PatternRequest {
  from: string; to: string; filters: QueryFilters; text?: TextQuery;
  interval_seconds: number; limit?: number; sort?: PatternSort;
}
export interface MessagePattern {
  pattern_id: string; environment: string | null; service: string; level: LogLevel;
  template: string; parts: PatternPart[]; exact: boolean;
  count: number; share: number; previous_count: number; growth_percent: number | null;
  is_new: boolean | null; first_seen: string; last_seen: string;
  buckets: HistogramBucket[];
}
export interface PatternResponse {
  algorithm_version: 1; as_of: string; watermark: number; core_watermark: number;
  history_from: string | null; history_complete: boolean;
  total_events: number; total_patterns: number; visible_patterns: number; new_patterns: number | null;
  patterns: MessagePattern[];
}
export interface PatternDetailsResponse {
  as_of: string; pattern: MessagePattern;
  variables: { name: string; variable_type: PatternVariableType; total: number;
    values: { value: string; count: number }[]; other_count: number }[];
  examples: LogEvent[];
}

export type IssueStatus = "open" | "new" | "regressed" | "resolved";
export interface IssueState {
  status: "open" | "resolved" | "regressed"; resolved_at: string | null; resolved_by: string | null;
}
export interface Issue {
  fingerprint: string; environment: string | null; service: string; action: string | null;
  exception_type: string | null; frames: string[]; pattern_id: string;
  template: string; parts: PatternPart[]; exact: boolean;
  count: number; servers: number; is_new: boolean | null; first_seen: string; last_seen: string;
  buckets: HistogramBucket[]; state: IssueState;
}
export interface IssueResponse {
  algorithm_version: 1; as_of: string; watermark: number; core_watermark: number;
  history_from: string | null; history_complete: boolean;
  counts: { open: number; new: number | null; regressed: number; resolved: number };
  status_events: number; issues: Issue[];
}
export interface IssueDetailsResponse {
  as_of: string; issue: Omit<Issue, "state">; servers: { value: string; count: number }[]; other_servers: number;
  examples: LogEvent[]; state: IssueState;
  history: { action: "resolved" | "reopened" | "regressed"; actor: string | null; at: string; event_at: string | null }[];
}

export type NodeStatus = "offline" | "degraded" | "unknown" | "idle" | "online";
export interface SpoolUsage { used_bytes: number; limit_bytes: number; records: number }
export interface InfraNode {
  kind: "relay" | "agent"; id: number; name: string; relay: string | null;
  status: NodeStatus; reason: "no_heartbeat" | "relay_silent" | "spool" | "delivery" | null;
  heartbeat_at: string | null; last_event_at: string | null; version: string | null;
  spool: SpoolUsage | null; delivery_failures: number | null; events_per_second: number | null;
  source: { environment: string | null; server: string; backend: string; service: string; service_instance: string | null } | null;
}
export interface InfraStorage {
  segments: number; parquet_bytes: number; index_bytes: number; oldest_event_at: string | null;
  retention_seconds: number; disk_total_bytes: number; disk_free_bytes: number;
}
export interface InfrastructureResponse {
  as_of: string;
  thresholds: { heartbeat_ttl_seconds: number; idle_seconds: number; spool_warn_percent: number };
  core: { status: "online" | "not_ready" | "unavailable"; version: string | null; visibility_lag_ms: number | null;
    wal_bytes: number | null; wal_limit_bytes: number | null };
  storage: InfraStorage | null;
  dock: { version: string; sse_clients: number; sessions: number | null };
  registry: boolean;
  nodes: InfraNode[];
}
