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
