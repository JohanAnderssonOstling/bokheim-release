const wireMediaType = "application/vnd.bokheim+protobuf; version=18";
const elements = Object.fromEntries([...document.querySelectorAll("[id]")].map((element) => [element.id, element]));
let hours = 24;
let refreshing = false;
let refreshPromise;
const dashboardRequestTimeoutMs = 10_000;

const pageDefinitions = {
  overview: ["Operations overview", "/admin/"],
  traffic: ["Traffic", "/admin/traffic"],
  accounts: ["Account activity", "/admin/accounts"],
  authority: ["Authority operations", "/admin/authority"],
  "authority-review": ["Authority manual review", "/admin/authority-review"],
  metadata: ["Metadata enrichment", "/admin/metadata"],
  security: ["Security and incident response", "/admin/security"],
};
const requestedPage = window.location.pathname.replace(/^\/admin\/?/, "").split("/")[0] || "overview";
const activePage = Object.hasOwn(pageDefinitions, requestedPage) ? requestedPage : "overview";

function selectPage() {
  document.title = `${pageDefinitions[activePage][0]} · Bokheim`;
  elements["page-title"].textContent = pageDefinitions[activePage][0];
  for (const section of document.querySelectorAll("[data-page]")) section.hidden = section.dataset.page !== activePage;
  for (const link of document.querySelectorAll("[data-nav-page]")) {
    const active = link.dataset.navPage === activePage;
    link.classList.toggle("active", active);
    if (active) link.setAttribute("aria-current", "page"); else link.removeAttribute("aria-current");
  }
}

const number = (value) => new Intl.NumberFormat().format(value);
function bytes(value) {
  if (!Number.isFinite(value) || value <= 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${(value / 1024 ** index).toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}
function duration(seconds) {
  const days = Math.floor(seconds / 86400);
  const hour = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  return days ? `${days}d ${hour}h` : hour ? `${hour}h ${minutes}m` : `${minutes}m`;
}
function timestamp(seconds) {
  return Number.isFinite(seconds) && seconds > 0 ? new Date(seconds * 1000).toLocaleString() : "—";
}
function perMinute(count, elapsedSeconds) {
  return Number.isFinite(count) && Number.isFinite(elapsedSeconds) && elapsedSeconds >= 2 ? count * 60 / elapsedSeconds : null;
}
function rate(value) {
  if (!Number.isFinite(value)) return "Measuring…";
  return new Intl.NumberFormat(undefined, { maximumFractionDigits:value < 10 ? 1 : 0 }).format(value);
}
function path(values) {
  if (!values.length) return "";
  const maximum = Math.max(...values, 1);
  return values.map((value, index) => {
    const x = values.length === 1 ? 360 : index / (values.length - 1) * 720;
    const y = 180 - value / maximum * 168 - 6;
    return `${index ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`;
  }).join(" ");
}

function refreshSession() {
  if (!refreshPromise) {
    refreshPromise = fetchWithTimeout("/api/auth/web/refresh", { method:"POST", credentials:"same-origin", headers:{ Accept:wireMediaType } })
      .then((response) => response.ok)
      .finally(() => { refreshPromise = undefined; });
  }
  return refreshPromise;
}
async function fetchWithTimeout(pathname, options) {
  const controller = new AbortController();
  const timer = window.setTimeout(() => controller.abort(), dashboardRequestTimeoutMs);
  try {
    return await fetch(pathname, { ...options, signal:controller.signal });
  } catch (error) {
    if (error?.name === "AbortError") throw new Error(`Dashboard API request timed out: ${pathname}`);
    throw new Error(`Dashboard API request failed: ${pathname}`);
  } finally {
    window.clearTimeout(timer);
  }
}
async function request(pathname, retry = true) {
  let response = await fetchWithTimeout(pathname, { credentials:"same-origin", headers:{ Accept:"application/json" } });
  if (response.status === 401 && retry) {
    if (await refreshSession()) response = await fetchWithTimeout(pathname, { credentials:"same-origin", headers:{ Accept:"application/json" } });
  }
  return response;
}
async function post(pathname, body, retry = true) {
  let response = await fetchWithTimeout(pathname, { method:"POST", credentials:"same-origin", headers:{ Accept:"application/json", "Content-Type":"application/json" }, body:JSON.stringify(body) });
  if (response.status === 401 && retry) {
    if (await refreshSession()) response = await fetchWithTimeout(pathname, { method:"POST", credentials:"same-origin", headers:{ Accept:"application/json", "Content-Type":"application/json" }, body:JSON.stringify(body) });
  }
  return response;
}

function aggregate(points) {
  const buckets = new Map();
  const routes = new Map();
  for (const point of points) {
    const bucket = buckets.get(point.bucket_ms) ?? { at:point.bucket_ms, requests:0, errors:0, inbound:0, outbound:0 };
    bucket.requests += point.request_count;
    if (point.status_class >= 4) bucket.errors += point.request_count;
    bucket.inbound += point.request_bytes;
    bucket.outbound += point.response_bytes;
    buckets.set(point.bucket_ms, bucket);
    const route = routes.get(point.route_group) ?? { route:point.route_group, requests:0, errors:0, inbound:0, outbound:0, maxLatency:0 };
    route.requests += point.request_count;
    if (point.status_class >= 4) route.errors += point.request_count;
    route.inbound += point.request_bytes;
    route.outbound += point.response_bytes;
    route.maxLatency = Math.max(route.maxLatency, point.max_duration_ms);
    routes.set(point.route_group, route);
  }
  return {
    buckets:[...buckets.values()].sort((left, right) => left.at - right.at),
    routes:[...routes.values()].sort((left, right) => right.requests - left.requests || left.route.localeCompare(right.route)),
  };
}

function card(label, value, detail) {
  const item = document.createElement("article");
  item.className = "card";
  for (const [tag, text] of [["span", label], ["strong", value], ["small", detail]]) {
    const child = document.createElement(tag); child.textContent = text; item.append(child);
  }
  return item;
}
function processControl(process) {
  const labels = {
    cover_downloads:["Cover downloads", "Downloads saved cover URLs and records the result against each book"],
    enrichment:["Enrichment pipeline", "Runs authority metadata, book-file extraction, title/author lookup, and OCR in pipeline order"],
  };
  if (!(process.process in labels)) {
    return document.createDocumentFragment();
  }
  const [label, description] = labels[process.process];
  const state = process.blocked_by_configuration
    ? "Disabled by server configuration"
    : process.pause_pending
      ? "Pausing after current work"
      : process.running
        ? "Running"
        : process.enabled
          ? "Enabled · idle"
          : "Paused";
  const item = document.createElement("article"); item.className = "process-control";
  const header = document.createElement("header");
  const title = document.createElement("h3"); title.textContent = label;
  const status = document.createElement("span"); status.className = "process-state"; status.textContent = `${state} · ${description}`;
  header.append(title, status);
  const actions = document.createElement("div"); actions.className = "process-actions";
  const toggle = document.createElement("button"); toggle.type = "button"; toggle.dataset.process = process.process; toggle.dataset.processAction = process.enabled ? "pause" : "resume"; toggle.textContent = process.enabled ? "Pause" : "Resume";
  actions.append(toggle);
  if (process.enabled && !process.blocked_by_configuration) {
    const run = document.createElement("button"); run.type = "button"; run.dataset.process = process.process; run.dataset.processAction = "run_now"; run.textContent = "Run now"; actions.append(run);
  }
  item.append(header, actions);
  return item;
}
function serviceState(service) {
  return service.available ? ["Available", `${number(service.latency_ms)} ms response`] : ["Unavailable", "Check the service or dashboard target configuration"];
}
function render(overview, history, requestHistory, engagement, services, identityReviews, accounts) {
  const authorityState = serviceState(services.authority);
  const metadataState = serviceState(services.metadata);
  elements.cards.replaceChildren(
    card("Requests · 1h", number(overview.requests_last_hour), `${number(overview.errors_last_hour)} errors`),
    card("Application API latency", `${overview.average_latency_ms_last_hour.toFixed(1)} ms`, `${number(overview.max_latency_ms_last_hour)} ms maximum · admin polling excluded`),
    card("Traffic · 1h", bytes(overview.request_bytes_last_hour + overview.response_bytes_last_hour), `↑ ${bytes(overview.request_bytes_last_hour)} · ↓ ${bytes(overview.response_bytes_last_hour)}`),
    card("Active sessions", number(overview.active_sessions), `${number(overview.active_websockets)} WebSockets`),
    card("In-flight requests", number(overview.in_flight_requests), `${number(overview.peak_in_flight_requests)} process peak`),
    card("Database pool", number(overview.database_connections), `${number(overview.database_idle_connections)} idle`),
    card("Active accounts · today", number(overview.daily_active_accounts), `${number(overview.weekly_active_accounts)} in 7d · ${number(overview.monthly_active_accounts)} in 30d`),
    card("New accounts · 30d", number(overview.new_verified_accounts_30d), `${number(overview.first_time_accounts_30d)} first-time · ${number(overview.returning_accounts_30d)} returning`),
    card("Sync accounts · 30d", number(overview.sync_accounts_30d), `${number(overview.auth_only_accounts_30d)} authenticated without sync`),
    card("Registered accounts", number(overview.registered_users), `${number(overview.unique_accounts_ever)} used server since tracking began`),
    card("Authority server", authorityState[0], authorityState[1]),
    card("Metadata enrichment", metadataState[0], metadataState[1]),
    card("Storage used", bytes(overview.storage_used_bytes), `${bytes(overview.storage_reserved_bytes)} reserved`),
    card("Process uptime", duration(overview.uptime_seconds), overview.dropped_traffic_events ? `${number(overview.dropped_traffic_events)} metrics dropped` : "No dropped metrics"),
  );
  const values = aggregate(history.points);
  elements["requests-path"].setAttribute("d", path(values.buckets.map((bucket) => bucket.requests)));
  elements["errors-path"].setAttribute("d", path(values.buckets.map((bucket) => bucket.errors)));
  elements["bytes-path"].setAttribute("d", path(values.buckets.map((bucket) => bucket.inbound + bucket.outbound)));
  elements["empty-chart"].hidden = values.buckets.length !== 0;
  elements["request-chart"].hidden = values.buckets.length === 0;
  elements["chart-period"].textContent = values.buckets.length ? `${new Date(values.buckets[0].at).toLocaleString()} — ${new Date(values.buckets.at(-1).at).toLocaleString()}` : "";
  elements["transfer-total"].textContent = bytes(values.buckets.reduce((sum, bucket) => sum + bucket.inbound + bucket.outbound, 0));
  const rows = values.routes.map((route) => {
    const row = document.createElement("tr");
    for (const [index, value] of [route.route, number(route.requests), number(route.errors), bytes(route.inbound), bytes(route.outbound), `${number(route.maxLatency)} ms`].entries()) {
      const cell = document.createElement("td");
      if (index === 0) { const code = document.createElement("code"); code.textContent = value; cell.append(code); } else cell.textContent = value;
      row.append(cell);
    }
    return row;
  });
  elements.routes.replaceChildren(...rows);
  const engagementRows = [...engagement.points].reverse().map((point) => {
    const row = document.createElement("tr");
    const values = [new Date(`${point.date}T00:00:00Z`).toLocaleDateString(), number(point.active_accounts), number(point.sync_accounts), number(point.new_verified_accounts)];
    for (const value of values) {
      const cell = document.createElement("td"); cell.textContent = value; row.append(cell);
    }
    return row;
  });
  elements.engagement.replaceChildren(...engagementRows);
  renderAccounts(accounts);

  const authority = services.authority.data;
  elements["authority-unavailable"].hidden = Boolean(authority);
  elements["authority-content"].hidden = !authority;
  if (authority) {
    const extractions = authority.epub_extractions;
    const coverDownloads = authority.cover_downloads;
    const metadataBackfill = authority.metadata_backfill;
    const descriptionBackfill = authority.description_backfill;
    const descriptionMetrics = authority.description_backfill_metrics;
    const extractionMetrics = authority.extraction_metrics;
    const descriptionBackfillAvailable = authority.description_backfill_available !== false;
    const descriptionWithoutOpenLibraryText = descriptionBackfill?.no_description ?? 0;
    const descriptionFinished = descriptionBackfill ? descriptionBackfill.succeeded + descriptionWithoutOpenLibraryText : 0;
    const descriptionTotal = descriptionBackfill ? descriptionFinished + descriptionBackfill.pending + descriptionBackfill.running : 0;
    const descriptionProgress = descriptionTotal ? descriptionFinished / descriptionTotal * 100 : 100;
    const descriptionsPerMinute = descriptionMetrics ? descriptionMetrics.jobs_per_second * 60 : null;
    const currentCrawls = authority.sources.filter((source) => source.crawl_status === "running");
    elements["authority-processes"].replaceChildren(...(authority.processes ?? []).map(processControl));
    const crawlRates = currentCrawls.map((source) => ({
      pages:perMinute(source.crawl_pages_fetched, source.crawl_elapsed_seconds),
      books:perMinute(source.crawl_books_seen, source.crawl_elapsed_seconds),
    }));
    const measuredRates = crawlRates.filter((speed) => speed.pages !== null);
    const pagesPerMinute = measuredRates.reduce((sum, speed) => sum + speed.pages, 0);
    const booksPerMinute = measuredRates.reduce((sum, speed) => sum + speed.books, 0);
    elements["authority-cards"].replaceChildren(
      card("Books", number(authority.active_book_count), `${number(authority.active_page_count)} cached pages`),
      card("Importable EPUBs", number(authority.active_epub_count), `${number(authority.sources.length)} configured sources`),
      card("Authority latency", `${number(services.authority.latency_ms)} ms`, "Operations snapshot response"),
      card("EPUB extraction queue", number(extractions.pending + extractions.running), `${number(extractions.succeeded)} succeeded · ${number(extractions.failed)} failed`),
      ...(extractionMetrics?.jobs ? [card("EPUB CPU stages", `${extractionMetrics.average_worker_ms.toFixed(0)} ms/book`, `container ${extractionMetrics.average_container_ms.toFixed(0)} · evidence ${extractionMetrics.average_evidence_ms.toFixed(0)} · TOC ${extractionMetrics.average_toc_ms.toFixed(0)} · thumbnail ${extractionMetrics.average_thumbnail_ms.toFixed(0)} ms`)] : []),
      ...(coverDownloads ? [card("Cover download queue", number(coverDownloads.pending + coverDownloads.running), `${number(coverDownloads.succeeded)} cached · ${number(coverDownloads.failed)} failed`)] : []),
      ...(metadataBackfill ? [card("Metadata backfill queue", number(metadataBackfill.pending + metadataBackfill.running), `${number(metadataBackfill.running)} running · ${number(metadataBackfill.succeeded)} succeeded · ${number(authority.identity_reviews?.failed ?? 0)} recorded failures`)] : []),
      ...(descriptionBackfill ? [card("Description backfill", descriptionBackfillAvailable ? `${descriptionProgress.toFixed(1)}%` : "Waiting", descriptionBackfillAvailable ? `${number(descriptionBackfill.pending + descriptionBackfill.running)} queued · ${rate(descriptionsPerMinute)} books/min · ${number(descriptionWithoutOpenLibraryText)} without an OL description` : "Metadata snapshot does not contain Open Library descriptions yet")] : []),
      card("Registry revision", number(authority.registry_revision), authority.metadata_enrichment_configured ? "Metadata enrichment configured" : "Metadata enrichment disabled"),
      card("Crawl speed", currentCrawls.length ? `${rate(measuredRates.length ? pagesPerMinute : null)} pages/min` : "Idle", currentCrawls.length ? `${rate(measuredRates.length ? booksPerMinute : null)} book entries/min across ${number(currentCrawls.length)} sources` : "No source is currently crawling"),
    );
    elements["authority-metadata-backfill"].hidden = !metadataBackfill;
    if (metadataBackfill) {
      const row = document.createElement("tr");
      for (const value of [metadataBackfill.pending, metadataBackfill.running, metadataBackfill.succeeded, authority.identity_reviews?.failed ?? 0, metadataBackfill.last_error || "None"]) {
        const cell = document.createElement("td");
        cell.textContent = typeof value === "number" ? number(value) : value;
        row.append(cell);
      }
      elements["authority-metadata-backfill-status"].replaceChildren(row);
    }
    elements["authority-description-backfill"].hidden = !descriptionBackfill;
    if (descriptionBackfill) {
      const row = document.createElement("tr");
      const speed = !descriptionBackfillAvailable
        ? "Paused"
        : descriptionBackfill.pending + descriptionBackfill.running === 0 && !descriptionMetrics?.jobs
        ? "Idle"
        : `${rate(descriptionsPerMinute)} books/min`;
      for (const value of [
        descriptionBackfill.pending,
        descriptionBackfill.running,
        descriptionBackfill.succeeded,
        descriptionWithoutOpenLibraryText,
        descriptionBackfillAvailable ? `${descriptionProgress.toFixed(1)}%` : "Waiting for snapshot",
        speed,
        descriptionBackfill.last_error || "None",
      ]) {
        const cell = document.createElement("td");
        cell.textContent = typeof value === "number" ? number(value) : value;
        row.append(cell);
      }
      elements["authority-description-backfill-status"].replaceChildren(row);
    }
    elements["authority-current-crawl-empty"].hidden = currentCrawls.length !== 0;
    elements["authority-current-crawl-wrap"].hidden = currentCrawls.length === 0;
    const currentCrawlRows = currentCrawls.map((source, sourceIndex) => {
      const row = document.createElement("tr");
      const extraction = source.epub_extractions;
      const values = [
        source.source_id,
        timestamp(source.last_crawl_started_at),
        crawlRates[sourceIndex].pages === null ? "Measuring…" : `${rate(crawlRates[sourceIndex].pages)} pages/min · ${rate(crawlRates[sourceIndex].books)} entries/min`,
        source.crawl_frontier_count == null ? "—" : number(source.crawl_frontier_count),
        number(source.active_page_count),
        number(source.active_book_count),
        number(extraction.pending + extraction.running),
      ];
      for (const [index, value] of values.entries()) {
        const cell = document.createElement("td");
        if (index === 0) { const code = document.createElement("code"); code.textContent = value; cell.append(code); } else cell.textContent = value;
        row.append(cell);
      }
      return row;
    });
    elements["authority-current-crawl"].replaceChildren(...currentCrawlRows);
    const sourceRows = authority.sources.map((source) => {
      const row = document.createElement("tr");
      const lastSuccess = source.last_crawl_succeeded_at ? timestamp(source.last_crawl_succeeded_at) : "Never";
      const extraction = source.epub_extractions;
      const crawlStatus = source.last_crawl_error_category ? `${source.crawl_status} · ${source.last_crawl_error_category.replaceAll("_", " ")}` : source.crawl_status;
      const blockedSeconds = Math.ceil(source.crawl_origin_blocked_ms / 1000);
      const throttle = blockedSeconds > 0
        ? `Waiting ${blockedSeconds < 60 ? `${blockedSeconds}s` : duration(blockedSeconds)}`
        : `${number(source.crawl_origin_in_flight)} / ${number(source.crawl_origin_concurrency)} active · ${number(source.crawl_origin_spacing_ms)} ms spacing`;
      for (const [index, value] of [source.source_id, crawlStatus, throttle, source.crawl_frontier_count == null ? "—" : number(source.crawl_frontier_count), lastSuccess, number(source.active_book_count), number(source.active_epub_count), `${number(extraction.pending + extraction.running)} / ${number(extraction.failed)}`].entries()) {
        const cell = document.createElement("td");
        if (index === 0) { const code = document.createElement("code"); code.textContent = value; cell.append(code); } else cell.textContent = value;
        row.append(cell);
      }
      return row;
    });
    elements["authority-sources"].replaceChildren(...sourceRows);
    const failures = authority.extraction_failures ?? [];
    elements["authority-failures-empty"].hidden = failures.length !== 0;
    elements["authority-failures-wrap"].hidden = failures.length === 0;
    const failureRows = failures.map((failure) => {
      const row = document.createElement("tr");
      const retry = failure.retryable ? "Retryable with backoff" : "Permanent";
      const observed = `${timestamp(failure.first_failed_at)} / ${timestamp(failure.last_failed_at)}`;
      const examples = failure.examples.map((example) => `${example.source_id} · ${example.acquisition_host} · ${example.book_id || "unknown"} · ${number(example.attempts)} attempts`).join("; ");
      for (const [index, value] of [failure.category.replaceAll("_", " "), retry, number(failure.count), observed, failure.next_retry_at ? timestamp(failure.next_retry_at) : "—", examples || "—"].entries()) {
        const cell = document.createElement("td");
        if (index === 0) { const code = document.createElement("code"); code.textContent = value; cell.append(code); } else cell.textContent = value;
        row.append(cell);
      }
      return row;
    });
    elements["authority-failures"].replaceChildren(...failureRows);
  }

  const metadata = services.metadata.data;
  elements["metadata-unavailable"].hidden = Boolean(metadata);
  elements["metadata-content"].hidden = !metadata;
  if (metadata) {
    elements["metadata-cards"].replaceChildren(
      card("Snapshot", metadata.dump_date, `Schema v${number(metadata.schema_version)} · imported ${new Date(metadata.imported_at_ms).toLocaleDateString()}`),
      card("Snapshot size", bytes(metadata.database_bytes), `${number(metadata.edition_records)} retained editions`),
      card("Retained works", number(metadata.work_records), `${number(metadata.author_records)} retained authors`),
      card("Enrichment batches", number(metadata.requests), `${number(metadata.errors)} errors · ${metadata.average_duration_ms.toFixed(1)} ms/batch`),
      ...(Number.isFinite(metadata.processed_items) ? [card("Enrichment throughput", `${rate(metadata.items_per_processing_second)} items/s`, `${number(metadata.processed_items)} items · ${metadata.average_duration_ms_per_item.toFixed(2)} ms/item`)] : []),
    );
    const metricRows = Object.entries(metadata.metrics).map(([name, value]) => {
      const row = document.createElement("tr");
      const key = document.createElement("td"); const code = document.createElement("code"); code.textContent = name; key.append(code);
      const metricValue = document.createElement("td"); metricValue.textContent = number(value); row.append(key, metricValue); return row;
    });
    elements["metadata-metrics"].replaceChildren(...metricRows);
  }
  const requestRows = requestHistory.points.map((request) => {
    const row = document.createElement("tr");
    const values = [
      new Date(request.occurred_at_ms).toLocaleString(),
      request.client_ip,
      request.route_group,
      request.method,
      `${request.status_class}xx`,
      bytes(request.request_bytes + request.response_bytes),
      `${number(request.duration_ms)} ms`,
    ];
    for (const [index, value] of values.entries()) {
      const cell = document.createElement("td");
      if (index === 1 || index === 2) { const code = document.createElement("code"); code.textContent = value; cell.append(code); } else cell.textContent = value;
      row.append(cell);
    }
    return row;
  });
  elements["request-ips"].replaceChildren(...requestRows);

  const sources = services.authority.data?.sources ?? [];
  const selectedSource = elements["review-source"].value;
  elements["review-source"].replaceChildren(new Option("All sources", ""), ...sources.map((source) => new Option(source.source_id, source.source_id)));
  elements["review-source"].value = selectedSource;
  renderIdentityReviews(identityReviews);
}

function renderAccounts(data) {
  if (!data) return;
  const shown = data.accounts.length;
  elements["accounts-total"].textContent = shown < data.total
    ? `${number(shown)} of ${number(data.total)} registered addresses · newest first · every view is audited`
    : `${number(data.total)} registered addresses · every view is audited`;
  elements["accounts-empty"].hidden = shown !== 0;
  const rows = data.accounts.map((account) => {
    const row = document.createElement("tr");
    const status = [account.verified ? "Verified" : "Unverified", account.is_admin ? "administrator" : null].filter(Boolean).join(" · ");
    const values = [
      account.email,
      new Date(account.created_at_ms).toLocaleString(),
      status,
      account.last_seen_at_ms ? new Date(account.last_seen_at_ms).toLocaleString() : "Never",
      number(account.request_count),
      number(account.libraries),
    ];
    for (const [index, value] of values.entries()) {
      const cell = document.createElement("td");
      if (index === 0) { const code = document.createElement("code"); code.textContent = value; cell.append(code); } else cell.textContent = value;
      row.append(cell);
    }
    return row;
  });
  elements.accounts.replaceChildren(...rows);
}

function renderIdentityReviews(data) {
  if (!data) return;
  const inventory = data.inventory;
  elements["review-cards"].replaceChildren(
    card("Ambiguous", number(inventory.ambiguous), "Select the supported edition"),
    card("No match", number(inventory.no_match), "Enter an ISBN, retry, or reject"),
    card("Insufficient metadata", number(inventory.insufficient_metadata), "Manual identification required"),
    card("Failed", number(inventory.failed), "Transient requests eligible for retry"),
    card("Resolved", number(inventory.resolved), `${number(inventory.rejected)} rejected`),
  );
  elements["reviews-empty"].hidden = data.reviews.length !== 0;
  const reviews = data.reviews.map((review) => {
    const article = document.createElement("article");
    article.className = "review-item";
    article.dataset.reviewId = review.review_id;
    const heading = document.createElement("h3"); heading.textContent = review.title || "Untitled book";
    const meta = document.createElement("p"); meta.className = "review-meta";
    meta.textContent = [review.authors.join(", ") || "Unknown author", review.book_year, review.source_id, review.status.replaceAll("_", " ")].filter(Boolean).join(" · ");
    article.append(heading, meta);
    if (review.detail) { const detail = document.createElement("p"); detail.className = "review-detail"; detail.textContent = review.detail; article.append(detail); }
    const candidates = document.createElement("div"); candidates.className = "review-candidates";
    for (const candidate of review.candidates) {
      const row = document.createElement("div"); row.className = "review-candidate";
      const description = document.createElement("div");
      const title = document.createElement("strong"); title.textContent = candidate.title;
      const evidence = document.createElement("span");
      const classifications = candidate.classifications.length ? ` · ${candidate.classifications.join("; ")}` : " · no mapped classifications";
      evidence.textContent = `${candidate.authors.join(", ") || "Unknown author"} · ${candidate.book_year || "year unknown"} · ISBN ${candidate.isbn13}${classifications}`;
      description.append(title, evidence);
      const choose = document.createElement("button"); choose.type = "button"; choose.textContent = "Select edition"; choose.dataset.reviewAction = "select_candidate"; choose.dataset.editionId = candidate.edition_id; choose.dataset.isbn13 = candidate.isbn13;
      row.append(description, choose); candidates.append(row);
    }
    if (review.candidates_truncated) { const note = document.createElement("p"); note.className = "review-detail"; note.textContent = "Candidate list was truncated; refine metadata or enter an ISBN manually."; candidates.append(note); }
    article.append(candidates);
    const actions = document.createElement("div"); actions.className = "review-actions";
    const form = document.createElement("form"); form.dataset.reviewAction = "manual_isbn";
    const isbn = document.createElement("input"); isbn.name = "isbn"; isbn.placeholder = "ISBN-10 or ISBN-13"; isbn.required = true; isbn.setAttribute("aria-label", "Manual ISBN");
    const apply = document.createElement("button"); apply.type = "submit"; apply.textContent = "Apply ISBN"; form.append(isbn, apply);
    const retry = document.createElement("button"); retry.type = "button"; retry.textContent = "Retry match"; retry.dataset.reviewAction = "retry";
    const reject = document.createElement("button"); reject.type = "button"; reject.textContent = "No suitable edition"; reject.className = "danger"; reject.dataset.reviewAction = "reject";
    actions.append(form, retry, reject); article.append(actions); return article;
  });
  elements.reviews.replaceChildren(...reviews);
}

function showError(message, signIn) {
  elements.loading.hidden = true;
  elements.dashboard.hidden = true;
  elements.notice.hidden = false;
  elements.logout.hidden = true;
  elements["notice-message"].textContent = message;
  elements["login-form"].hidden = !signIn;
  elements.retry.hidden = signIn;
}
async function load() {
  if (refreshing) return;
  refreshing = true;
  elements.refresh.disabled = true;
  try {
    const reviewQuery = new URLSearchParams({ limit:"100" });
    if (elements["review-source"].value) reviewQuery.set("source_id", elements["review-source"].value);
    if (elements["review-status"].value) reviewQuery.set("status", elements["review-status"].value);
    const identityReviewRequest = activePage === "authority-review"
      ? request(`/api/admin/authority-identity-reviews?${reviewQuery}`)
      : Promise.resolve({ ok:true, status:200, json:async () => null });
    // Email addresses are only fetched while the accounts page is open, so the
    // background poll on other pages does not keep reading account identities.
    const accountsRequest = activePage === "accounts"
      ? request("/api/admin/accounts?limit=200")
      : Promise.resolve({ ok:true, status:200, json:async () => null });
    const responses = await Promise.all([
      request("/api/admin/overview"),
      request(`/api/admin/traffic?hours=${hours}`),
      request(`/api/admin/requests?hours=${Math.min(hours, 168)}&limit=200`),
      request("/api/admin/engagement?days=30"),
      request("/api/admin/services"),
      identityReviewRequest,
      accountsRequest,
    ]);
    if (responses.some((response) => response.status === 401)) return showError("Sign in with an administrator account.", true);
    if (responses.some((response) => response.status === 403)) return showError("This account is not an administrator.", true);
    if (responses.some((response) => response.status === 429)) return showError("The dashboard is being refreshed too frequently. Wait a moment and try again.", false);
    if (responses.some((response) => !response.ok)) return showError("The operational API did not respond successfully.", false);
    render(...await Promise.all(responses.map((response) => response.json())));
    elements.loading.hidden = true;
    elements.notice.hidden = true;
    elements.dashboard.hidden = false;
    elements.logout.hidden = false;
    elements.updated.textContent = `Updated ${new Date().toLocaleTimeString()}`;
  } catch (error) {
    showError(error instanceof Error ? error.message : "The dashboard could not reach the operational API.", false);
  } finally {
    refreshing = false;
    elements.refresh.disabled = false;
  }
}

elements.refresh.addEventListener("click", load);
elements.retry.addEventListener("click", load);
elements.logout.addEventListener("click", async () => {
  elements.logout.disabled = true;
  try { await fetch("/api/auth/web/logout", { method:"POST", credentials:"same-origin", headers:{ Accept:wireMediaType } }); } finally {
    elements.logout.disabled = false;
    elements.updated.textContent = "";
    showError("Signed out. Sign in with an administrator account.", true);
  }
});
elements.ranges.addEventListener("click", (event) => {
  const button = event.target.closest("button[data-hours]");
  if (!button) return;
  hours = Number(button.dataset.hours);
  for (const range of elements.ranges.querySelectorAll("button")) range.classList.toggle("active", range === button);
  load();
});
elements["review-source"].addEventListener("change", load);
elements["review-status"].addEventListener("change", load);
elements["authority-processes"].addEventListener("click", async (event) => {
  const button = event.target.closest("button[data-process-action]");
  if (!button) return;
  button.disabled = true;
  try {
    const response = await post(`/api/admin/authority-processes/${button.dataset.process}`, { action:button.dataset.processAction });
    if (!response.ok) throw new Error();
    await load();
  } catch {
    button.disabled = false;
    window.alert("The authority process control could not be changed.");
  }
});
elements.reviews.addEventListener("click", async (event) => {
  const button = event.target.closest("button[data-review-action]");
  if (!button) return;
  const review = button.closest("[data-review-id]");
  const action = button.dataset.reviewAction;
  const body = action === "select_candidate" ? { action, edition_id:button.dataset.editionId, isbn13:button.dataset.isbn13 } : { action };
  button.disabled = true;
  try { const response = await post(`/api/admin/authority-identity-reviews/${review.dataset.reviewId}`, body); if (!response.ok) throw new Error(); await load(); }
  catch { button.disabled = false; window.alert("The review decision could not be saved."); }
});
elements.reviews.addEventListener("submit", async (event) => {
  const form = event.target.closest("form[data-review-action='manual_isbn']");
  if (!form) return;
  event.preventDefault();
  const review = form.closest("[data-review-id]");
  const submit = form.querySelector("button[type='submit']"); submit.disabled = true;
  try { const response = await post(`/api/admin/authority-identity-reviews/${review.dataset.reviewId}`, { action:"manual_isbn", isbn:new FormData(form).get("isbn") }); if (!response.ok) throw new Error(); await load(); }
  catch { submit.disabled = false; window.alert("The ISBN could not be applied."); }
});
elements["login-form"].addEventListener("submit", async (event) => {
  event.preventDefault();
  elements.login.disabled = true;
  try {
    const response = await fetch("/api/admin/login", { method:"POST", credentials:"same-origin", headers:{ "Content-Type":"application/json", Accept:"application/json" }, body:JSON.stringify({ email:elements.email.value, password:elements.password.value }) });
    elements.password.value = "";
    if (!response.ok) {
      const message = response.status === 429
        ? "Too many sign-in attempts. Wait before trying again."
        : response.status === 403
          ? "Firefox did not permit the same-origin sign-in request. Reload the page and try again."
          : "Administrator email or password is incorrect.";
      return showError(message, true);
    }
    await load();
  } catch {
    showError("The sign-in request could not reach the server.", true);
  } finally {
    elements.login.disabled = false;
  }
});

selectPage();
load();
window.setInterval(() => {
  const editingReview = activePage === "authority-review" && document.activeElement?.closest("form[data-review-action='manual_isbn']");
  if (!elements.dashboard.hidden && !editingReview) load();
}, 15_000);
