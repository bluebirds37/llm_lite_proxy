import type { LogEntry } from "./types";

export type LogFilter = "all" | "error" | "stream";

export interface LogPanel {
  update(entries: LogEntry[]): void;
  setFilter(filter: LogFilter): void;
}

export function mountLogs(
  host: HTMLElement,
  onClear: () => void,
): LogPanel {
  let entries: LogEntry[] = [];
  let filter: LogFilter = "all";
  let open: number | null = null;

  function visible(): LogEntry[] {
    if (filter === "error") return entries.filter((entry) => entry.status >= 400);
    if (filter === "stream") return entries.filter((entry) => entry.streaming);
    return entries;
  }

  function statusTag(status: number): string {
    if (status >= 500) return `<span class="tag bad">${status}</span>`;
    if (status >= 400) return `<span class="tag warn">${status}</span>`;
    return `<span class="tag good">${status}</span>`;
  }

  function time(ms: number): string {
    const date = new Date(ms);
    return date.toLocaleTimeString("zh-CN", { hour12: false });
  }

  function row(entry: LogEntry): string {
    const path = entry.converted
      ? `${entry.client} → ${entry.upstream_protocol}`
      : `${entry.client} 直通`;
    const flags = [
      entry.streaming ? "流式" : "",
      entry.first_token_ms !== null ? `首包 ${entry.first_token_ms}ms` : "",
      entry.notes.length > 0 ? "备注" : "",
    ]
      .filter(Boolean)
      .join(" · ");
    const extra = open === entry.id ? detailRow(entry) : "";
    return `
      <tr data-id="${entry.id}">
        <td>${time(entry.at_ms)}</td>
        <td>${statusTag(entry.status)}</td>
        <td>${escape(entry.provider || "-")}</td>
        <td>${escape(path)}</td>
        <td>${escape(entry.requested_model)}</td>
        <td class="num">${entry.input_tokens}</td>
        <td class="num">${entry.output_tokens}</td>
        <td class="num">${entry.latency_ms}</td>
        <td>${escape(flags)}</td>
        <td>${escape(entry.error ?? "")}</td>
      </tr>${extra}`;
  }

  function detailRow(entry: LogEntry): string {
    const parts = [
      entry.error ? `错误: ${entry.error}` : "",
      entry.detail ? `上游原始响应: ${entry.detail}` : "",
      entry.dropped.length ? `转换丢弃: ${entry.dropped.join(", ")}` : "",
      entry.notes.length ? `备注: ${entry.notes.join(", ")}` : "",
      `上游模型: ${entry.upstream_model}`,
    ].filter(Boolean);
    return `<tr><td colspan="10"><div class="detail">${escape(parts.join("\n"))}</div></td></tr>`;
  }

  function render(): void {
    const rows = visible().map(row).join("");
    host.innerHTML = `
      <div class="card">
        <div class="head">
          <h3>请求日志</h3>
          <div class="inline">
            <button class="small ${filter === "all" ? "accent" : "ghost"}" data-filter="all" type="button">全部</button>
            <button class="small ${filter === "error" ? "accent" : "ghost"}" data-filter="error" type="button">只看失败</button>
            <button class="small ${filter === "stream" ? "accent" : "ghost"}" data-filter="stream" type="button">只看流式</button>
            <button class="small danger" data-clear type="button">清空</button>
          </div>
        </div>
        <p class="hint">点击任意一行展开上游原始响应与转换丢弃的参数。</p>
        <div class="scroller">
          <table>
            <thead>
              <tr>
                <th>时间</th><th>状态</th><th>上游</th><th>协议路径</th><th>模型</th>
                <th>输入</th><th>输出</th><th>耗时</th><th>标记</th><th>错误</th>
              </tr>
            </thead>
            <tbody>${rows || `<tr><td colspan="10">还没有请求。把客户端的 base URL 指到这里就会实时出现。</td></tr>`}</tbody>
          </table>
        </div>
      </div>`;

    host.querySelectorAll<HTMLButtonElement>("[data-filter]").forEach((button) => {
      button.addEventListener("click", () => {
        filter = (button.dataset.filter ?? "all") as LogFilter;
        render();
      });
    });
    host.querySelector("[data-clear]")?.addEventListener("click", onClear);
    host.querySelectorAll<HTMLTableRowElement>("tbody tr[data-id]").forEach((tr) => {
      tr.addEventListener("click", () => {
        const id = Number(tr.dataset.id);
        open = open === id ? null : id;
        render();
      });
    });
  }

  render();
  return {
    update(next: LogEntry[]) {
      entries = next;
      render();
    },
    setFilter(next: LogFilter) {
      filter = next;
      render();
    },
  };
}

export function escape(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}
