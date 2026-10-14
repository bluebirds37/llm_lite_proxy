export type Protocol = "openai" | "anthropic";

export type Api = "chat" | "responses" | "messages";

export interface ModelRoute {
  public: string;
  upstream: string;
  provider?: string | null;
}

export interface Provider {
  name: string;
  protocol: Protocol;
  api?: Api | null;
  base_url: string;
  api_key: string;
  enabled: boolean;
  patterns: string[];
  models: ModelRoute[];
  headers: Record<string, string>;
}

export interface ServerConfig {
  host: string;
  port: number;
  master_keys: string[];
  default_provider: string | null;
  default_max_tokens: number;
  passthrough_unknown_params: boolean;
  openai_include_usage: boolean;
  request_timeout_secs: number;
  stream_idle_timeout_secs: number;
  max_body_mb: number;
  log_capacity: number;
  allow_origins: string[];
}

export interface Config {
  server: ServerConfig;
  providers: Provider[];
}

export interface LogEntry {
  id: number;
  at_ms: number;
  client: string;
  endpoint: string;
  provider: string;
  upstream_protocol: string;
  requested_model: string;
  upstream_model: string;
  converted: boolean;
  streaming: boolean;
  status: number;
  latency_ms: number;
  first_token_ms: number | null;
  input_tokens: number;
  output_tokens: number;
  error: string | null;
  detail: string | null;
  dropped: string[];
  notes: string[];
}

export interface ProviderStats {
  requests: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  avg_latency_ms: number;
}

export interface Stats {
  requests: number;
  errors: number;
  input_tokens: number;
  output_tokens: number;
  active: number;
  uptime_ms: number;
  by_provider: Record<string, ProviderStats>;
}

export interface Snapshot {
  config: Config;
  running: boolean;
  address: string;
  configPath: string;
  stats: Stats;
  error: string | null;
}

export interface Probe {
  ok: boolean;
  status: number;
  latency_ms: number;
  message: string;
  detail: string | null;
}
