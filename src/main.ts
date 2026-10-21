import "./styles.css";
import { mountLogs, type LogPanel } from "./logs";
import { initThemeToggle } from "./theme";
import type { Api, Config, LogEntry, Probe, Provider, Snapshot, Stats } from "./types";

const inTauri = "__TAURI_INTERNALS__" in window;

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(command, args);
}

let snapshot: Snapshot | null = null;
let draft: Config = { server: {} as never, providers: [] };
let logs: LogPanel | null = null;
let entries: LogEntry[] = [];

const $ = <T extends HTMLElement>(selector: string): T => {
  const found = document.querySelector<T>(selector);
  if (!found) throw new Error(`missing element ${selector}`);
  return found;
};

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className = ""): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  return node;
}

function field(label: string, control: HTMLElement, hint = ""): HTMLElement {
  const wrap = el("label", "field");
  const caption = el("span");
  caption.textContent = label;
  wrap.append(caption, control);
  if (hint) {
    const note = el("span", "hint");
    note.textContent = hint;
    wrap.append(note);
  }
  return wrap;
}

function textInput(
  value: string,
  onChange: (next: string) => void,
  placeholder = "",
  type = "text",
): HTMLInputElement {
  const input = el("input");
  input.type = type;
  input.value = value;
  if (placeholder) input.placeholder = placeholder;
  input.addEventListener("input", () => onChange(input.value));
  return input;
}

function numberInput(value: number, onChange: (next: number) => void): HTMLInputElement {
  const input = el("input");
  input.type = "number";
  input.value = String(value);
  let last = value;
  input.addEventListener("input", () => {
    const parsed = Number(input.value);
    // Clearing the field mid-edit must not silently write 0.
    if (input.value.trim() === "" || !Number.isFinite(parsed)) {
      return;
    }
    last = parsed;
    onChange(parsed);
  });
  input.addEventListener("blur", () => {
    if (input.value.trim() === "") {
      input.value = String(last);
    }
  });
  return input;
}

function selectInput(
  items: { value: string; label: string }[],
  value: string,
  onChange: (next: string) => void,
): HTMLSelectElement {
  const select = el("select");
  for (const item of items) {
    const option = el("option");
    option.value = item.value;
    option.textContent = item.label;
    option.selected = item.value === value;
    select.append(option);
  }
  select.addEventListener("change", () => onChange(select.value));
  return select;
}

function toggle(label: string, value: boolean, onChange: (next: boolean) => void): HTMLElement {
  const wrap = el("label", "switch");
  const input = el("input");
  input.type = "checkbox";
  input.checked = value;
  input.addEventListener("change", () => onChange(input.checked));
  const text = el("span");
  text.textContent = label;
  wrap.append(input, text);
  return wrap;
}

function toast(message: string): void {
  const host = $("#toast");
  host.textContent = message;
  host.classList.remove("hidden");
  window.setTimeout(() => host.classList.add("hidden"), 2600);
}

function banner(message: string | null): void {
  const host = $("#banner");
  host.textContent = message ?? "";
  host.classList.toggle("hidden", !message);
}

function baseUrl(): string {
  const { host, port } = draft.server;
  const shown = host === "0.0.0.0" ? "127.0.0.1" : host;
  return `http://${shown}:${port}`;
}

let savedJson = "";

/// Comparing against the last saved snapshot instead of the live config keeps
/// the flag independent of how the backend orders its JSON keys.
function rememberSaved(): void {
  savedJson = JSON.stringify(draft);
  syncDirty();
}

function isDirty(): boolean {
  return JSON.stringify(draft) !== savedJson;
}

function syncDirty(): void {
  const host = document.querySelector("#dirty");
  host?.classList.toggle("hidden", !isDirty());
}

function updateLiveRow(): void {
  const row = document.querySelector<HTMLElement>("#panel-server .live-row");
  if (!row) return;
  const wanted = `${draft.server.host}:${draft.server.port}`;
  row.classList.toggle("pending", Boolean(snapshot?.running) && snapshot?.address !== wanted);
  const label = row.querySelector("span");
  if (label) label.textContent = serverLiveHint();
}

function serverLiveHint(): string {
  const wanted = `${draft.server.host}:${draft.server.port}`;
  if (!snapshot?.running) {
    return `代理未运行。填好地址与端口后保存即开始监听。`;
  }
  if (snapshot.address !== wanted) {
    return `当前监听 ${snapshot.address}，与上面填写的 ${wanted} 不一致：保存后会切换过去。`;
  }
  return `正在监听 ${snapshot.address}`;
}

function refreshStatus(): void {
  const running = snapshot?.running ?? false;
  const pill = $("#pill");
  pill.textContent = running ? "运行中" : "已停止";
  pill.className = `pill ${running ? "on" : "off"}`;
  $("#address").textContent = snapshot?.address ?? `${draft.server.host}:${draft.server.port}`;
  $("#toggle-run").textContent = running ? "停止" : "启动";
  $("#config-path").textContent = snapshot?.configPath ?? "";
  syncDirty();

  const stats = snapshot?.stats;
  if (!stats) return;
  $("#m-requests").textContent = String(stats.requests);
  $("#m-active").textContent = String(stats.active);
  $("#m-errors").textContent = String(stats.errors);
  $("#m-in").textContent = String(stats.input_tokens);
  $("#m-out").textContent = String(stats.output_tokens);
  $("#m-uptime").textContent = `${Math.floor(stats.uptime_ms / 1000)}s`;
  banner(snapshot?.error ?? null);
}

async function refresh(): Promise<void> {
  if (!inTauri) return;
  try {
    snapshot = await call<Snapshot>("get_snapshot");
    draft = snapshot.config;
    rememberSaved();
    entries = await call<LogEntry[]>("recent_logs", { limit: draft.server.log_capacity });
    logs?.update(entries);
    renderAll();
  } catch (error) {
    banner(`读取配置失败: ${String(error)}`);
  }
}

let selected: number | null = 0;
// Keyed by position, so renaming or duplicating an upstream keeps its result.
const probes = new Map<number, { ok: boolean; text: string }>();

function newProvider(index: number): Provider {
  return {
    name: `upstream-${index + 1}`,
    protocol: "openai",
    api: "chat",
    base_url: "https://api.openai.com/v1",
    api_key: "",
    enabled: true,
    patterns: [],
    models: [],
    headers: {},
  };
}

const API_LABELS: Record<Api, string> = {
  chat: "OpenAI Chat Completions",
  responses: "OpenAI Responses",
  messages: "Anthropic Messages",
};

const API_PATHS: Record<Api, string> = {
  chat: "/chat/completions",
  responses: "/responses",
  messages: "/messages",
};

function apisFor(protocol: Provider["protocol"]): Api[] {
  return protocol === "anthropic" ? ["messages"] : ["chat", "responses"];
}

function apiOf(provider: Provider): Api {
  const allowed = apisFor(provider.protocol);
  return provider.api && allowed.includes(provider.api) ? provider.api : allowed[0];
}

function defaultBaseUrl(api: Api): string {
  if (api === "messages") return "https://api.anthropic.com";
  if (api === "responses") return "https://api.openai.com/v1";
  return "https://api.openai.com/v1";
}

function providerSummary(provider: Provider): string {
  const bits: string[] = [provider.protocol, apiOf(provider)];
  if (provider.models.length) bits.push(`${provider.models.length} 别名`);
  if (provider.patterns.length) bits.push(`${provider.patterns.length} 匹配`);
  const headers = Object.keys(provider.headers).length;
  if (headers) bits.push(`${headers} 请求头`);
  return bits.join(" · ");
}

function renderProviders(): void {
  const host = $("#panel-providers");
  host.textContent = "";
  if (selected !== null && selected >= draft.providers.length) {
    selected = draft.providers.length ? draft.providers.length - 1 : null;
  }
  if (!draft.providers.length) {
    host.append(emptyProviders());
    return;
  }

  const grid = el("div", "cards");
  draft.providers.forEach((_, index) => grid.append(providerCard(index)));
  grid.append(addCard());
  host.append(grid);
}

function emptyProviders(): HTMLElement {
  const card = el("div", "empty");

  const glyph = el("span", "empty-glyph");
  glyph.setAttribute("aria-hidden", "true");
  // Same mark as the header logo, so the empty state reads as "this app, not configured yet".
  glyph.innerHTML =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8h13l-3.5-3.5M21 16H8l3.5 3.5"/></svg>';

  const title = el("h3");
  title.textContent = "还没有上游";

  const text = el("p");
  text.textContent =
    "一个上游就是一个模型服务：选协议、填地址和 Key 就行。协议选它自己说哪种，跟客户端用哪种无关，两边不一样时自动互转。";

  const actions = el("div", "empty-actions");
  const seed = el("button", "primary");
  seed.type = "button";
  seed.textContent = "填入两条示例";
  seed.addEventListener("click", seedExamples);

  const manual = el("button", "ghost");
  manual.type = "button";
  manual.textContent = "手动添加一个";
  manual.addEventListener("click", () => {
    draft.providers.push(newProvider(0));
    selected = 0;
    renderProviders();
    document
      .querySelector<HTMLInputElement>("#panel-providers .pcard.open input[data-name]")
      ?.focus();
  });

  actions.append(seed, manual);

  const note = el("p", "hint");
  note.textContent = "示例只填名字与匹配模式，Key 留空，补上你的再保存。";

  card.append(glyph, title, text, actions, note);
  return card;
}

function seedExamples(): void {
  draft.providers = [
    {
      name: "claude",
      protocol: "anthropic",
      api: "messages",
      base_url: "https://api.anthropic.com",
      api_key: "",
      enabled: true,
      patterns: ["claude*"],
      models: [],
      headers: {},
    },
    {
      name: "openai",
      protocol: "openai",
      api: "chat",
      base_url: "https://api.openai.com/v1",
      api_key: "",
      enabled: true,
      patterns: ["gpt-*"],
      models: [],
      headers: {},
    },
  ];
  selected = 0;
  renderProviders();
}

function addCard(): HTMLElement {
  const card = el("article", "pcard add");
  const button = el("button", "add-inner");
  button.type = "button";
  const glyph = el("span", "add-glyph");
  glyph.textContent = "+";
  const label = el("span", "add-label");
  label.textContent = "添加上游";
  const hint = el("span", "hint");
  hint.textContent = "一个上游就是一个模型服务";
  button.append(glyph, label, hint);
  button.addEventListener("click", () => {
    draft.providers.push(newProvider(draft.providers.length));
    selected = draft.providers.length - 1;
    renderProviders();
    document
      .querySelector<HTMLInputElement>("#panel-providers .pcard.open input[data-name]")
      ?.focus();
  });
  card.append(button);
  return card;
}

function section(title: string, hint: string, fields: HTMLElement[]): HTMLElement {
  const wrap = el("section", "section");
  const head = el("div", "section-head");
  const label = el("h4");
  label.textContent = title;
  const note = el("p", "hint");
  note.textContent = hint;
  const grid = el("div", "fields");
  grid.append(...fields);
  head.append(label, note);
  wrap.append(head, grid);
  return wrap;
}

function wide(node: HTMLElement): HTMLElement {
  node.classList.add("span-2");
  return node;
}

function providerCard(index: number): HTMLElement {
  const provider = draft.providers[index];
  const open = selected === index;
  const card = el("article", open ? "pcard open" : provider.enabled ? "pcard" : "pcard off");

  const head = el("div", "pcard-head");
  const dot = el("span", provider.enabled ? "dot on" : "dot");

  const name = el("input", "pcard-name");
  name.type = "text";
  name.value = provider.name;
  name.placeholder = "给这个上游起个名字";
  name.dataset.name = "1";
  name.addEventListener("input", () => {
    provider.name = name.value;
  });

  const meta = el("span", "pcard-meta");
  meta.textContent = providerSummary(provider);

  const enable = toggle("启用", provider.enabled, (next) => {
    provider.enabled = next;
    dot.className = next ? "dot on" : "dot";
    card.classList.toggle("off", !next);
  });

  const expand = el("button", "chevron");
  expand.type = "button";
  expand.setAttribute("aria-expanded", String(open));
  expand.textContent = open ? "收起" : "配置";
  expand.addEventListener("click", () => {
    selected = open ? null : index;
    renderProviders();
  });

  head.append(dot, name, meta, enable, expand);
  head.addEventListener("click", (event) => {
    // Renaming or flipping the switch must not collapse the card behind the cursor.
    if ((event.target as HTMLElement).closest("input, button, label, select")) return;
    selected = open ? null : index;
    renderProviders();
  });
  card.append(head);
  if (!open) return card;

  const probe = el("div", "probe");
  const remembered = probes.get(index);
  if (remembered) {
    probe.className = `probe ${remembered.ok ? "ok" : "bad"}`;
    probe.textContent = remembered.text;
  }

  const test = el("button", "small");
  test.type = "button";
  test.textContent = "测试连通";
  test.addEventListener("click", async () => {
    test.disabled = true;
    probe.className = "probe";
    probe.textContent = "请求中…";
    try {
      const result = await call<Probe>("test_provider", { provider });
      const text = result.detail
        ? `${result.message}  (HTTP ${result.status}, ${result.latency_ms}ms)`
        : result.message;
      probes.set(index, { ok: result.ok, text });
      probe.className = `probe ${result.ok ? "ok" : "bad"}`;
      probe.textContent = text;
    } catch (error) {
      probe.className = "probe bad";
      probe.textContent = String(error);
    } finally {
      test.disabled = false;
    }
  });

  const duplicate = el("button", "small");
  duplicate.type = "button";
  duplicate.textContent = "复制";
  duplicate.addEventListener("click", () => {
    const clone: Provider = JSON.parse(JSON.stringify(provider));
    clone.name = `${provider.name}-copy`;
    draft.providers.splice(index + 1, 0, clone);
    selected = index + 1;
    renderProviders();
  });

  const remove = el("button", "small danger");
  remove.type = "button";
  remove.textContent = "删除";
  remove.addEventListener("click", () => {
    draft.providers.splice(index, 1);
    selected = draft.providers.length ? Math.min(index, draft.providers.length - 1) : null;
    renderProviders();
  });

  const actions = el("div", "inline");
  actions.append(test, duplicate, remove);

  const body = el("div", "pcard-body");
  const sections = el("div", "sections");
  sections.append(
    section("接口", "协议族决定鉴权头与错误体形态，API 面决定调用路径与转换方式。", [
      field(
        "协议族",
        selectInput(
          [
            { value: "openai", label: "OpenAI" },
            { value: "anthropic", label: "Anthropic" },
          ],
          provider.protocol,
          (next) => {
            provider.protocol = next as Provider["protocol"];
            provider.api = apiOf(provider);
            provider.base_url = defaultBaseUrl(provider.api);
            renderProviders();
          },
        ),
      ),
      field(
        "API 面",
        selectInput(
          apisFor(provider.protocol).map((api) => ({
            value: api,
            label: `${API_LABELS[api]} (${API_PATHS[api]})`,
          })),
          apiOf(provider),
          (next) => {
            provider.api = next as Api;
            renderProviders();
          },
        ),
      ),
    ]),
    section("连接", "Base URL 可以只填主机、填到 /v1，或者直接填完整 endpoint 路径。", [
      wide(
        field(
          "Base URL",
          textInput(
            provider.base_url,
            (next) => {
              provider.base_url = next;
            },
            defaultBaseUrl(apiOf(provider)),
          ),
          "留空时按协议族取官方地址",
        ),
      ),
      wide(apiKeyField(provider)),
    ]),
    section("模型路由", "命中顺序：模型别名 → 自动匹配 → 兜底上游。都不命中时返回 404。", [
      wide(
        field(
          "自动匹配模型（逗号分隔，支持 *）",
          textInput(
            provider.patterns.join(", "),
            (next) => {
              provider.patterns = next
                .split(",")
                .map((item) => item.trim())
                .filter(Boolean);
              meta.textContent = providerSummary(provider);
            },
            "claude*, gpt-*",
          ),
        ),
      ),
      wide(modelsEditor(provider)),
    ]),
    section("附加请求头", "每次上游请求都会带上，例如网关要求的 anthropic-version 或 User-Agent。", [
      wide(headersEditor(provider)),
    ]),
  );

  const foot = el("div", "pcard-foot");
  foot.append(probe, actions);
  body.append(sections, foot);
  card.append(body);
  return card;
}

function apiKeyField(provider: Provider): HTMLElement {
  const input = textInput(
    provider.api_key,
    (next) => {
      provider.api_key = next;
    },
    "留空则转发客户端自带的 key",
    "password",
  );
  const row = el("div", "input-group");
  const reveal = el("button", "small ghost");
  reveal.type = "button";
  reveal.textContent = "显示";
  reveal.addEventListener("click", () => {
    const hidden = input.type === "password";
    input.type = hidden ? "text" : "password";
    reveal.textContent = hidden ? "隐藏" : "显示";
  });
  row.append(input, reveal);
  return field("API Key", row, "保存在本机配置目录的 config.json 里");
}

function modelsEditor(provider: Provider): HTMLElement {
  const wrap = el("div", "rows-editor");
  const label = el("span", "field-label");
  label.textContent = "模型别名（客户端发的名字 → 真正发给上游的名字）";
  wrap.append(label);

  const table = el("table", "rows-table");
  const head = el("thead");
  const headRow = el("tr");
  for (const caption of ["客户端发送的名字", "上游真实模型", ""]) {
    const cell = el("th");
    cell.textContent = caption;
    headRow.append(cell);
  }
  head.append(headRow);

  const body = el("tbody");
  provider.models.forEach((route, index) => {
    const row = el("tr");
    const publicCell = el("td");
    publicCell.append(
      textInput(
        route.public,
        (next) => {
          route.public = next;
        },
        "如 claude-sonnet",
      ),
    );
    const upstreamCell = el("td");
    upstreamCell.append(
      textInput(
        route.upstream,
        (next) => {
          route.upstream = next;
        },
        "如 claude-sonnet-4-5",
      ),
    );
    const actionCell = el("td", "actions");
    actionCell.append(
      rowRemove(() => {
        provider.models.splice(index, 1);
        renderProviders();
      }),
    );
    row.append(publicCell, upstreamCell, actionCell);
    body.append(row);
  });
  table.append(head, body);
  wrap.append(table);

  if (provider.models.length === 0) {
    const empty = el("p", "hint");
    empty.textContent = "还没有别名。不填也能用，客户端发的模型名会原样转给上游。";
    wrap.append(empty);
  }

  const add = el("button", "small ghost");
  add.type = "button";
  add.textContent = "+ 添加模型别名";
  add.addEventListener("click", () => {
    provider.models.push({ public: "", upstream: "" });
    renderProviders();
  });
  wrap.append(add);
  return wrap;
}

function headersEditor(provider: Provider): HTMLElement {
  const wrap = el("div", "rows-editor");
  const label = el("span", "field-label");
  label.textContent = "附加请求头（名字 → 值）";
  wrap.append(label);

  const table = el("table", "rows-table");
  const head = el("thead");
  const headRow = el("tr");
  for (const caption of ["名字", "值", ""]) {
    const cell = el("th");
    cell.textContent = caption;
    headRow.append(cell);
  }
  head.append(headRow);

  const body = el("tbody");
  for (const [key, value] of Object.entries(provider.headers)) {
    let current = key;
    const row = el("tr");

    const nameCell = el("td");
    nameCell.append(
      textInput(
        key,
        (next) => {
          const name = next.trim();
          // A nameless header would fail on the way out to the upstream.
          if (!name || name === current) return;
          const kept = provider.headers[current];
          delete provider.headers[current];
          provider.headers[name] = kept;
          current = name;
        },
        "anthropic-version",
      ),
    );

    const valueCell = el("td");
    valueCell.append(
      textInput(
        value,
        (next) => {
          provider.headers[current] = next;
        },
        "2023-06-01",
      ),
    );

    const actionCell = el("td", "actions");
    actionCell.append(
      rowRemove(() => {
        delete provider.headers[current];
        renderProviders();
      }),
    );

    row.append(nameCell, valueCell, actionCell);
    body.append(row);
  }
  table.append(head, body);
  wrap.append(table);

  if (!Object.keys(provider.headers).length) {
    const empty = el("p", "hint");
    empty.textContent = "没有附加请求头。大多数网关不需要这一步。";
    wrap.append(empty);
  }

  const add = el("button", "small ghost");
  add.type = "button";
  add.textContent = "+ 添加请求头";
  add.addEventListener("click", () => {
    provider.headers["X-"] = "";
    renderProviders();
  });
  wrap.append(add);
  return wrap;
}

function rowRemove(onClick: () => void): HTMLButtonElement {
  const button = el("button", "small danger");
  button.type = "button";
  button.textContent = "×";
  button.title = "删除这一行";
  button.addEventListener("click", onClick);
  return button;
}
function switchRow(
  title: string,
  hint: string,
  value: boolean,
  onChange: (next: boolean) => void,
): HTMLElement {
  const row = el("div", "switch-row");
  row.append(toggle(title, value, onChange));
  if (hint) {
    const note = el("p", "hint");
    note.textContent = hint;
    row.append(note);
  }
  return row;
}

function renderServer(): void {
  const host = $("#panel-server");
  host.textContent = "";
  const card = el("div", "card");
  const head = el("h3");
  head.textContent = "代理设置";
  const note = el("p", "note");
  note.textContent =
    "所有改动都会在停止输入后自动保存，不需要手动点保存。改监听地址、端口或请求体上限会重启监听（进行中的请求会断）。";

  const live = el("div", isDirty() ? "live-row pending" : "live-row");
  live.innerHTML = "";
  const liveLabel = el("span");
  liveLabel.textContent = serverLiveHint();
  live.append(liveLabel);

  const hostInput = textInput(draft.server.host, (next) => {
    draft.server.host = next;
  });

  const portInput = numberInput(draft.server.port, (next) => {
    draft.server.port = next;
  });

  const sections = el("div", "sections");
  sections.append(
    section("监听", "地址填 0.0.0.0 会让局域网内其他机器也能访问，请务必设置客户端密钥。", [
      field("监听地址", hostInput, "改动会自动保存并切换监听"),
      field("端口", portInput, "改动会自动保存并切换监听"),
    ]),
    section("鉴权与路由", "两边都留空时：客户端不校验，上游用客户端自带的 key。", [
      wide(
        field(
          "客户端密钥（逗号分隔，留空则不校验）",
          textInput(draft.server.master_keys.join(", "), (next) => {
            draft.server.master_keys = next
              .split(",")
              .map((item) => item.trim())
              .filter(Boolean);
          }),
          "客户端把它填到 Authorization: Bearer 或 x-api-key",
        ),
      ),
      field(
        "兜底上游",
        selectInput(
          [
            { value: "", label: "不指定" },
            ...draft.providers.map((provider) => ({ value: provider.name, label: provider.name })),
          ],
          draft.server.default_provider ?? "",
          (next) => {
            draft.server.default_provider = next || null;
          },
        ),
        "模型名谁都没命中时走它",
      ),
    ]),
    section("超时与上限", "超时是从收到请求开始算的总时长；空闲超时只管流式响应两块数据之间。", [
      field("默认 max_tokens（客户端没填时）", numberInput(draft.server.default_max_tokens, (next) => {
        draft.server.default_max_tokens = next;
      })),
      field("请求体上限（MB）", numberInput(draft.server.max_body_mb, (next) => {
        draft.server.max_body_mb = next;
      }), "改动会重启监听"),
      field("请求总超时（秒）", numberInput(draft.server.request_timeout_secs, (next) => {
        draft.server.request_timeout_secs = next;
      })),
      field("流式空闲超时（秒）", numberInput(draft.server.stream_idle_timeout_secs, (next) => {
        draft.server.stream_idle_timeout_secs = next;
      })),
    ]),
    section("日志与跨源", "日志只存在内存里，重启窗口即清空。", [
      field("日志保留条数", numberInput(draft.server.log_capacity, (next) => {
        draft.server.log_capacity = next;
      }), "实时生效，不丢已有记录"),
      wide(
        field(
          "允许跨源的浏览器来源",
          textInput(draft.server.allow_origins.join(", "), (next) => {
            draft.server.allow_origins = next
              .split(",")
              .map((item) => item.trim())
              .filter(Boolean);
          }),
          "例如 http://localhost:1420，或填 *",
        ),
      ),
    ]),
    section("行为开关", "", [
      wide(
        (() => {
          const rows = el("div", "switch-rows");
          rows.append(
            switchRow(
              "透传本项目未识别的请求参数",
              "打开后，顶层不认识的参数原样发给上游；关闭则丢弃并记进日志的「转换丢弃」。",
              draft.server.passthrough_unknown_params,
              (next) => {
                draft.server.passthrough_unknown_params = next;
              },
            ),
            switchRow(
              "向 OpenAI 上游索取 usage 块",
              "关掉后流式响应不带 token 统计；个别对请求体挑剔的网关需要关闭。",
              draft.server.openai_include_usage,
              (next) => {
                draft.server.openai_include_usage = next;
              },
            ),
          );
          return rows;
        })(),
      ),
    ]),
  );

  card.append(head, note, live, sections);
  host.append(card);
}

function snippetBlock(title: string, body: string): HTMLElement {
  const wrap = el("div");
  const head = el("div", "head");
  const label = el("h3");
  label.textContent = title;
  const copy = el("button", "small ghost");
  copy.type = "button";
  copy.textContent = "复制";
  copy.addEventListener("click", async () => {
    await navigator.clipboard.writeText(body);
    toast("已复制");
  });
  const code = el("pre");
  code.textContent = body;
  head.append(label, copy);
  wrap.append(head, code);
  return wrap;
}

function renderStart(): void {
  const host = $("#panel-start");
  host.textContent = "";
  const card = el("div", "card");
  const head = el("h3");
  head.textContent = "客户端接入";
  const note = el("p", "note");
  note.textContent = `代理地址 ${baseUrl()}。三个入站端点 /v1/chat/completions、/v1/responses、/v1/messages 任选，上游走哪种由上面的协议族 + API 面决定，不一样时自动互转。`;
  card.append(head, note);

  const key = draft.server.master_keys[0] ?? "任意值（未开鉴权）";
  const aliases = draft.providers
    .flatMap((provider) => provider.models.map((model) => `${provider.name}/${model.public}`))
    .slice(0, 6)
    .join(", ");

  card.append(
    snippetBlock(
      "Claude Code / Anthropic SDK",
      [
        `export ANTHROPIC_BASE_URL=${baseUrl()}`,
        `export ANTHROPIC_API_KEY=${key}`,
        aliases ? `# 可用模型: ${aliases}` : "",
      ]
        .filter(Boolean)
        .join("\n"),
    ),
    snippetBlock(
      "Codex / OpenAI SDK / 任何 OpenAI 兼容客户端",
      [
        `export OPENAI_BASE_URL=${baseUrl()}/v1`,
        `export OPENAI_API_KEY=${key}`,
      ].join("\n"),
    ),
    snippetBlock(
      "OpenAI Responses 客户端（Codex 等）",
      [
        `export OPENAI_BASE_URL=${baseUrl()}/v1`,
        `export OPENAI_API_KEY=${key}`,
        "# 客户端打 /v1/responses，上游可以是 Chat 或 Messages 协议",
      ].join("\n"),
    ),
    snippetBlock(
      "curl：用 OpenAI 格式调用 Claude 上游",
      [
        `curl ${baseUrl()}/v1/chat/completions \\`,
        `  -H "content-type: application/json" \\`,
        `  -H "x-api-key: ${key}" \\`,
        `  -d '{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hi"}]}'`,
      ].join("\n"),
    ),
    snippetBlock(
      "curl：用 Anthropic 流式格式调用 GPT 上游",
      [
        `curl ${baseUrl()}/v1/messages \\`,
        `  -H "content-type: application/json" \\`,
        `  -H "x-api-key: ${key}" \\`,
        `  -H "anthropic-version: 2023-06-01" \\`,
        `  -d '{"model":"gpt-4o-mini","max_tokens":64,"stream":true,"messages":[{"role":"user","content":"hi"}]}'`,
      ].join("\n"),
    ),
  );

  if (draft.providers.length === 0) {
    const warning = el("p", "hint");
    warning.textContent = "先在“上游配置”里加一个上游，否则任何请求都会返回 404。";
    card.append(warning);
  }
  host.append(card);
}

function renderAll(): void {
  renderProviders();
  renderServer();
  renderStart();
  refreshStatus();
}

let saving = false;
let queuedSave = false;
let autosaveTimer: number | undefined;

function scheduleAutosave(): void {
  if (!inTauri) return;
  window.clearTimeout(autosaveTimer);
  autosaveTimer = window.setTimeout(() => {
    if (isDirty()) {
      void save({ silent: true });
    }
  }, 800);
}

async function save(options: { silent?: boolean } = {}): Promise<void> {
  if (!inTauri) {
    if (!options.silent) {
      toast("这是纯浏览器预览，配置保存只在 Tauri 窗口里可用");
    }
    return;
  }
  if (saving) {
    queuedSave = true;
    return;
  }
  if (options.silent && !isDirty()) {
    return;
  }

  saving = true;
  const sent = JSON.stringify(draft);
  try {
    snapshot = await call<Snapshot>("save_config", { config: draft });
    // The draft stays the source of truth: adopting the server copy would drop
    // whatever was typed during the round trip.
    savedJson = sent;
    syncDirty();
    refreshStatus();
    updateLiveRow();
    if (!options.silent) {
      const saved = $("#saved");
      saved.classList.remove("hidden");
      window.setTimeout(() => saved.classList.add("hidden"), 2400);
      toast(
        snapshot.running
          ? `配置已保存，正在监听 ${snapshot.address}`
          : "配置已保存，代理未运行",
      );
    }
  } catch (error) {
    toast(`保存失败：${String(error)}`);
    syncDirty();
  } finally {
    saving = false;
    if (queuedSave) {
      queuedSave = false;
      void save({ silent: true });
    }
  }
}

async function toggleRunning(): Promise<void> {
  const running = snapshot?.running ?? false;
  try {
    snapshot = await call<Snapshot>("set_running", { running: !running });
    draft = snapshot.config;
    renderAll();
  } catch (error) {
    toast(`启动失败：${String(error)}`);
    await refresh();
  }
}

async function boot(): Promise<void> {
  $("#tabs").querySelectorAll<HTMLButtonElement>(".tab").forEach((tab) => {
    tab.addEventListener("click", () => {
      $("#tabs").querySelector(".tab.active")?.classList.remove("active");
      tab.classList.add("active");
      document.querySelectorAll(".panel").forEach((panel) => panel.classList.remove("active"));
      const name = tab.dataset.panel ?? "";
      $(`#panel-${name}`).classList.add("active");
      if (name === "providers") renderProviders();
      if (name === "server") renderServer();
      if (name === "start") renderStart();
    });
  });

  initThemeToggle($("#theme-toggle"), (explicit) => {
    if (!inTauri) return;
    // The CSS only paints the client area; the title bar needs the same call.
    void call("set_window_theme", { theme: explicit }).catch(() => undefined);
  });
  $("#save").addEventListener("click", () => void save());
  $("#toggle-run").addEventListener("click", () => void toggleRunning());
  $("#address").addEventListener("click", () => void navigator.clipboard.writeText(baseUrl()).then(() => toast("已复制 base URL")));
  $("#copy-url").addEventListener("click", () =>
    void navigator.clipboard.writeText(baseUrl()).then(() => toast("已复制 base URL")),
  );

  logs = mountLogs($("#panel-logs"), () => {
    entries = [];
    logs?.update(entries);
  });

  document.addEventListener("input", () => {
    syncDirty();
    scheduleAutosave();
  });
  document.addEventListener("change", () => {
    syncDirty();
    scheduleAutosave();
  });

  if (!inTauri) {
    banner("这是纯浏览器预览：配置读写需要运行在 Tauri 桌面窗口里（npm run tauri dev）。");
    draft = {
      server: {
        host: "127.0.0.1",
        port: 8787,
        master_keys: [],
        default_provider: null,
        default_max_tokens: 4096,
        passthrough_unknown_params: false,
        openai_include_usage: true,
        request_timeout_secs: 600,
        stream_idle_timeout_secs: 120,
        max_body_mb: 64,
        log_capacity: 400,
        allow_origins: [],
      },
      providers: [],
    };
    rememberSaved();
    renderAll();
    return;
  }

  await refresh();

  const { listen } = await import("@tauri-apps/api/event");
  await listen<LogEntry>("log-entry", (event) => {
    entries = [event.payload, ...entries].slice(0, draft.server.log_capacity);
    logs?.update(entries);
    void call<Stats>("stats")
      .then((stats) => {
        if (snapshot) snapshot.stats = stats;
        refreshStatus();
      })
      .catch(() => undefined);
  });

  window.setInterval(() => {
    void call<Snapshot>("get_snapshot")
      .then((next) => {
        snapshot = next;
        refreshStatus();
      })
      .catch(() => undefined);
  }, 4000);
}

void boot();
