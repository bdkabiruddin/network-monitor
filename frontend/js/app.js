// Network Monitor — Tauri frontend shell.
//
// This file wires up navigation, theme toggling, dialogs/dropdowns, and
// the 24H Traffic bin selector — all real interactivity. The Dashboard
// screen is wired to real data via src-tauri/'s `get_dashboard` and
// `get_traffic_chart` commands (see refreshDashboard() below); every
// other screen still shows honest "No data yet" placeholders until its
// own backend commands exist — never fill those with random/sample
// values in the meantime.

const PAGES = {
  dashboard: ['Dashboard', 'Live bandwidth, interface, and session overview'],
  connections: ['Connections', 'Active TCP/UDP sockets and peer geography'],
  processes: ['Processes', 'Network activity ranked by process'],
  wifi: ['Wi-Fi Analyzer', 'Current connection and nearby access points'],
  firewall: ['Firewall', 'nftables rules and traffic shaping'],
  vpn: ['VPN', 'NetworkManager VPN profiles and status'],
  alerts: ['Alerts', 'Bandwidth anomaly detection'],
  reports: ['Reports', 'Exportable traffic history'],
  interfaces: ['Interfaces', 'Every interface the kernel knows about'],
  speedtest: ['Speed Test', 'Download/upload throughput and diagnostics'],
  settings: ['Settings', 'App preferences'],
};

function goToPage(page) {
  for (const btn of document.querySelectorAll('.nav-btn')) {
    btn.classList.toggle('active', btn.dataset.page === page);
  }
  for (const section of document.querySelectorAll('main.content > section')) {
    section.classList.toggle('hidden', section.dataset.page !== page);
  }
  const [title, subtitle] = PAGES[page] || PAGES.dashboard;
  document.getElementById('page-title').textContent = title;
  document.getElementById('page-subtitle').textContent = subtitle;
}

function initNav() {
  for (const btn of document.querySelectorAll('.nav-btn')) {
    btn.addEventListener('click', () => {
      goToPage(btn.dataset.page);
      // Fetch immediately on switch rather than waiting for the next
      // 2s tick — refreshCurrentPage is defined further down the file
      // but already exists by the time a click can happen.
      refreshCurrentPage();
      // Settings has no PAGE_REFRESHERS entry (its form loads once at
      // startup so a 2s re-poll can't fight an in-progress slider drag),
      // but usage-vs-cap doesn't touch the form — refresh it on every
      // visit so it reflects this month's current total, not just
      // whatever was true at launch.
      if (btn.dataset.page === 'settings') refreshDataUsage();
    });
  }
  goToPage('dashboard');
}

function syncThemeIcon() {
  const isDark = document.documentElement.getAttribute('data-theme') === 'dark';
  // Sun = "switch to light" affordance (shown while dark); moon = "switch to dark" (shown while light).
  document.getElementById('theme-icon-sun').classList.toggle('hidden', !isDark);
  document.getElementById('theme-icon-moon').classList.toggle('hidden', isDark);
}

function toggleTheme() {
  const root = document.documentElement;
  const isDark = root.getAttribute('data-theme') === 'dark';
  root.setAttribute('data-theme', isDark ? 'light' : 'dark');
  syncThemeIcon();
}

function initTheme() {
  document.getElementById('theme-btn').addEventListener('click', toggleTheme);
  document.getElementById('qa-toggle-theme').addEventListener('click', () => {
    toggleTheme();
    closeAllMenus();
  });
  syncThemeIcon();
}

function initRefresh() {
  document.getElementById('refresh-btn').addEventListener('click', () => refreshCurrentPage(true));
}

// — dropdowns (quick actions, sidebar profile) — click the trigger to
// toggle, click anywhere else (or Escape) to close. Only one open at a time.
function closeAllMenus() {
  document.getElementById('quickactions-menu').classList.add('hidden');
  document.getElementById('profile-menu').classList.add('hidden');
}

function initDropdowns() {
  const pairs = [
    ['quickactions-btn', 'quickactions-menu'],
    ['profile-btn', 'profile-menu'],
  ];
  for (const [btnId, menuId] of pairs) {
    const btn = document.getElementById(btnId);
    const menu = document.getElementById(menuId);
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const wasOpen = !menu.classList.contains('hidden');
      closeAllMenus();
      menu.classList.toggle('hidden', wasOpen);
    });
  }
  document.addEventListener('click', closeAllMenus);
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closeAllMenus();
  });
}

// Header quick-actions menu items that jump to a screen and trigger its
// real action, rather than duplicating that action's logic here.
function initQuickActionShortcuts() {
  document.getElementById('qa-run-speedtest').addEventListener('click', () => {
    closeAllMenus();
    document.querySelector('.nav-btn[data-page="speedtest"]').click();
    runSpeedTest();
  });
  document.getElementById('qa-capture-packets').addEventListener('click', () => {
    closeAllMenus();
    document.querySelector('.nav-btn[data-page="speedtest"]').click();
    runPacketCapture();
  });
  document.getElementById('qa-generate-report').addEventListener('click', async () => {
    closeAllMenus();
    document.querySelector('.nav-btn[data-page="reports"]').click();
    try {
      await invoke('generate_report', { period: '24h' });
      await refreshReports();
    } catch (e) {
      /* surfaced via the Reports page's own status line on next manual generate */
    }
  });
}

// — dialogs (command palette, keyboard shortcuts, add firewall rule) —
function openDialog(id) {
  document.getElementById(id).classList.remove('hidden');
}
function closeDialog(id) {
  document.getElementById(id).classList.add('hidden');
}

function openPalette() {
  const input = document.getElementById('palette-input');
  input.value = '';
  openDialog('palette-backdrop');
  renderPaletteResults('');
  input.focus();
}

function initDialogs() {
  document.getElementById('palette-btn').addEventListener('click', openPalette);
  document.getElementById('palette-backdrop').addEventListener('click', (e) => {
    if (e.target.id === 'palette-backdrop') closeDialog('palette-backdrop');
  });

  document.getElementById('shortcuts-btn').addEventListener('click', () => openDialog('shortcuts-backdrop'));
  document.getElementById('shortcuts-close').addEventListener('click', () => closeDialog('shortcuts-backdrop'));
  document.getElementById('shortcuts-backdrop').addEventListener('click', (e) => {
    if (e.target.id === 'shortcuts-backdrop') closeDialog('shortcuts-backdrop');
  });

  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    for (const id of ['palette-backdrop', 'shortcuts-backdrop']) closeDialog(id);
  });

  document.addEventListener('keydown', (e) => {
    if (!(e.metaKey || e.ctrlKey)) return;
    const key = e.key.toLowerCase();
    if (key === 'k') {
      e.preventDefault();
      openPalette();
    } else if (key === 'j') {
      e.preventDefault();
      toggleTheme();
    } else if (key === 'r') {
      e.preventDefault();
      refreshCurrentPage(true);
    }
  });

  initPalette();
}

// Command palette: filters PAGES by title/subtitle, click or Enter
// navigates. Kept intentionally simple — this is page navigation, not
// a fuzzy-search action runner.
function renderPaletteResults(query) {
  const results = document.getElementById('palette-results');
  const q = query.trim().toLowerCase();
  const entries = Object.entries(PAGES).filter(
    ([, [title, subtitle]]) => !q || title.toLowerCase().includes(q) || subtitle.toLowerCase().includes(q)
  );
  results.innerHTML = entries.length
    ? entries
        .map(
          ([page, [title]], i) =>
            `<button type="button" class="btn btn-ghost palette-result" style="justify-content:flex-start;width:100%;padding:10px 12px" data-page="${page}" data-idx="${i}">${escapeHtml(title)}</button>`
        )
        .join('')
    : `<div class="text-muted" style="padding:10px 12px;font-size:13px">No matching page</div>`;
}
function initPalette() {
  const input = document.getElementById('palette-input');
  input.addEventListener('input', () => renderPaletteResults(input.value));
  document.getElementById('palette-results').addEventListener('click', (e) => {
    const btn = e.target.closest('.palette-result');
    if (!btn) return;
    document.querySelector(`.nav-btn[data-page="${btn.dataset.page}"]`).click();
    closeDialog('palette-backdrop');
    input.value = '';
  });
  input.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter') return;
    const first = document.querySelector('.palette-result');
    if (first) first.click();
  });
}

// — range sliders that echo their value into an adjacent label —
function initRangeLabels() {
  const pairs = [
    ['refresh-interval-range', 'refresh-interval-label'],
    ['cap-gb-range', 'cap-gb-label'],
    ['history-retention-range', 'history-retention-label'],
  ];
  for (const [rangeId, labelId] of pairs) {
    const range = document.getElementById(rangeId);
    const label = document.getElementById(labelId);
    range.addEventListener('input', () => { label.textContent = range.value; });
  }
}

// ── Connection health ────────────────────────────────────────────────
//
// Every invoke() goes through here, so a real backend/IPC outage (poll
// loop crashed, IPC channel broken) shows up as several rejections in a
// row -- a single one-off failure (a cancelled pkexec prompt, one bad
// tick) isn't enough to trip this, since the very next successful poll
// resets the streak to 0 before it reaches the threshold. Previously
// every failure just went to console.error with zero user-visible
// indication anything was wrong -- the "Live" dot kept glowing and the
// timestamp just silently froze.
const CONNECTION_FAILURE_THRESHOLD = 3;
let connectionFailureStreak = 0;
let connectionIsDown = false;

function setLiveStatus(online) {
  const row = document.getElementById('live-status-row');
  const text = document.getElementById('live-status-text');
  if (!row || !text) return;
  row.classList.toggle('offline', !online);
  text.textContent = online ? 'Live' : 'Offline';
}

function showConnectionToast() {
  const toast = document.getElementById('connection-toast');
  if (toast) toast.classList.remove('hidden');
  setLiveStatus(false);
}

function hideConnectionToast() {
  const toast = document.getElementById('connection-toast');
  if (toast) toast.classList.add('hidden');
  setLiveStatus(true);
}

function invoke(cmd, args) {
  if (!window.__TAURI__) return Promise.reject(new Error('not running inside Tauri'));
  return window.__TAURI__.core.invoke(cmd, args).then(
    (result) => {
      connectionFailureStreak = 0;
      if (connectionIsDown) {
        connectionIsDown = false;
        hideConnectionToast();
      }
      return result;
    },
    (err) => {
      connectionFailureStreak += 1;
      if (!connectionIsDown && connectionFailureStreak >= CONNECTION_FAILURE_THRESHOLD) {
        connectionIsDown = true;
        showConnectionToast();
      }
      throw err;
    },
  );
}

// Normalizes a series of numbers into an SVG <polyline> `points` string
// for the small per-metric sparklines (viewBox 0 0 100 30). A flat/empty
// series draws a flat mid-line rather than dividing by a zero range.
function sparklinePoints(values) {
  if (!values || values.length === 0) return '';
  const min = Math.min(...values);
  const max = Math.max(...values);
  const range = max - min || 1;
  const stepX = values.length > 1 ? 100 / (values.length - 1) : 0;
  return values
    .map((v, i) => {
      const x = (i * stepX).toFixed(1);
      const y = (28 - ((v - min) / range) * 26).toFixed(1);
      return `${x},${y}`;
    })
    .join(' ');
}

// Matches netmon-core's format::format_bytes_rate exactly (same unit
// thresholds/precision) so the JS-rendered axis/tooltip values read the
// same as every other rate the Rust side already formats.
function formatBytesRate(v) {
  if (v == null) return '—';
  let val = v;
  for (const unit of ['B/s', 'KB/s', 'MB/s']) {
    if (val < 1024) return `${val.toFixed(1)} ${unit}`;
    val /= 1024;
  }
  return `${val.toFixed(1)} GB/s`;
}

// Builds a dual-series (download/upload) filled-area + line path set
// for a viewBox "0 0 1000 <height>" chart, both series sharing one
// scale (with 15% headroom, like the egui build's annotated_area_chart)
// so they're visually comparable. Bins/samples with no data come back
// as 0 from the backend — rendered as a real trough, not interpolated
// across the gap. Shared by Dashboard's 24H Traffic and Live Telemetry
// charts.
function buildTrafficPaths(download, upload, height = 200) {
  const empty = { dlArea: '', ulArea: '', dlLine: '', ulLine: '', maxVal: 1 };
  if (!download || download.length === 0) return empty;
  const maxVal = Math.max(...download, ...upload, 1) * 1.15;
  const n = download.length;
  const stepX = n > 1 ? 1000 / (n - 1) : 0;
  const top = height - 2;
  const plotH = height - 10;
  const toPoints = (arr) => arr.map((v, i) => [i * stepX, top - ((v || 0) / maxVal) * plotH]);
  const toLine = (pts) => pts.map(([x, y], i) => `${i === 0 ? 'M' : 'L'}${x.toFixed(1)} ${y.toFixed(1)}`).join(' ');
  const toArea = (pts) => {
    const first = pts[0][0].toFixed(1);
    const last = pts[pts.length - 1][0].toFixed(1);
    return `M${first} ${height} ${pts.map(([x, y]) => `L${x.toFixed(1)} ${y.toFixed(1)}`).join(' ')} L${last} ${height} Z`;
  };
  const dlPoints = toPoints(download);
  const ulPoints = toPoints(upload);
  return {
    dlArea: toArea(dlPoints),
    ulArea: toArea(ulPoints),
    dlLine: toLine(dlPoints),
    ulLine: toLine(ulPoints),
    maxVal,
  };
}

// 5 evenly-spaced horizontal gridlines + matching Y-axis rate labels
// (top = maxVal, bottom = 0) mirrored on both sides, same 5-tick scheme
// the egui build uses.
function renderTrafficGrid(maxVal, ids, height = 200) {
  const grid = document.getElementById(ids.grid);
  const yaxisL = document.getElementById(ids.yaxisL);
  const yaxisR = document.getElementById(ids.yaxisR);
  if (!grid || !yaxisL || !yaxisR) return;
  const rows = [];
  const labels = [];
  for (let i = 0; i <= 4; i++) {
    const y = (height * i) / 4;
    rows.push(`<line x1="0" y1="${y}" x2="1000" y2="${y}" stroke="var(--color-divider)" stroke-width="1"></line>`);
    labels.push(`<span>${formatBytesRate((maxVal * (4 - i)) / 4)}</span>`);
  }
  grid.innerHTML = rows.join('');
  yaxisL.innerHTML = labels.join('');
  yaxisR.innerHTML = labels.join('');
}

// Evenly-spaced 12-hour-format timestamp labels under the 24H Traffic
// chart, spanning the last 24h regardless of which bin size is active.
function updateTrafficLabels(binSeconds, binCount) {
  const container = document.getElementById('traffic24h-labels');
  if (!container || binCount === 0) return;
  const labelCount = 6;
  const now = Date.now();
  const labels = [];
  for (let i = 0; i < labelCount; i++) {
    const binIndex = Math.round((i / (labelCount - 1)) * (binCount - 1));
    const ts = now - (binCount - 1 - binIndex) * binSeconds * 1000;
    labels.push(new Date(ts).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit', hour12: true }));
  }
  container.innerHTML = labels.map((l) => `<span>${l}</span>`).join('');
}

// SQLite's bucketed datetime() rows come back "YYYY-MM-DD HH:MM:SS"
// (space, no "T") which some JS engines won't parse as local time
// reliably — the raw-row path already writes "YYYY-MM-DDTHH:MM:SS.ffffff".
// Normalizing both to have a "T" fixes that without needing to know
// which path produced a given row.
function parseSqlTimestamp(s) {
  return new Date(s.replace(' ', 'T'));
}

// Evenly-spaced 12-hour-format labels from a series of *real* per-row
// timestamps (the Live Telemetry chart) rather than an assumed fixed
// interval — samples aren't perfectly evenly spaced (a missed poll, or
// a bucket's actual width), so this reads the actual row timestamps.
function updateTimestampLabelsFromRows(containerId, timestamps) {
  const container = document.getElementById(containerId);
  if (!container || timestamps.length === 0) {
    if (container) container.innerHTML = '';
    return;
  }
  const labelCount = Math.min(6, timestamps.length);
  const labels = [];
  for (let i = 0; i < labelCount; i++) {
    const idx = labelCount <= 1 ? 0 : Math.round((i / (labelCount - 1)) * (timestamps.length - 1));
    labels.push(
      parseSqlTimestamp(timestamps[idx]).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit', hour12: true })
    );
  }
  container.innerHTML = labels.map((l) => `<span>${l}</span>`).join('');
}

// Latest chart series + bin size, kept around so the hover handler (set
// up once in initDashboard) always reads current data without redoing
// the network fetch on every mousemove.
let trafficChartState = null;

function updateTrafficTooltip(clientX, clientY) {
  const svg = document.getElementById('traffic24h-svg');
  const tooltip = document.getElementById('traffic24h-tooltip');
  const crosshair = document.getElementById('traffic24h-crosshair');
  if (!trafficChartState || !svg || !tooltip || !crosshair) return;
  const { download, upload, latency, binSeconds } = trafficChartState;
  const n = download.length;
  if (n < 2) return;

  const rect = svg.getBoundingClientRect();
  const frac = Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
  const idx = Math.round(frac * (n - 1));
  const xSvg = (idx / (n - 1)) * 1000;

  crosshair.setAttribute('x1', xSvg);
  crosshair.setAttribute('x2', xSvg);
  crosshair.classList.remove('hidden');

  const now = Date.now();
  const ts = now - (n - 1 - idx) * binSeconds * 1000;
  const dt = new Date(ts);
  const timeLabel = dt.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit', second: '2-digit', hour12: true });
  const dateLabel = dt.toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
  const lat = latency && latency[idx];

  tooltip.innerHTML = `
    <div class="tt-row" style="justify-content:space-between;gap:12px"><span>${timeLabel}</span><span class="text-muted">${dateLabel}</span></div>
    <div class="tt-row" style="color:var(--color-dl)"><span class="swatch" style="background:var(--color-dl)"></span>Download: ${formatBytesRate(download[idx])}</div>
    <div class="tt-row" style="color:var(--color-ul)"><span class="swatch" style="background:var(--color-ul)"></span>Upload: ${formatBytesRate(upload[idx])}</div>
    ${lat ? `<div class="tt-row text-muted">Latency: ${lat.toFixed(0)} ms</div>` : ''}
  `;
  tooltip.classList.remove('hidden');

  const wrapperRect = svg.parentElement.getBoundingClientRect();
  const xPixel = (xSvg / 1000) * wrapperRect.width;
  const tooltipW = tooltip.offsetWidth || 150;
  const left = xPixel + 12 + tooltipW > wrapperRect.width ? xPixel - 12 - tooltipW : xPixel + 12;
  tooltip.style.left = `${Math.max(0, left)}px`;
}

function hideTrafficTooltip() {
  const tooltip = document.getElementById('traffic24h-tooltip');
  const crosshair = document.getElementById('traffic24h-crosshair');
  if (tooltip) tooltip.classList.add('hidden');
  if (crosshair) crosshair.classList.add('hidden');
}

function initTrafficHover() {
  const svg = document.getElementById('traffic24h-svg');
  if (!svg) return;
  svg.addEventListener('mousemove', (e) => updateTrafficTooltip(e.clientX, e.clientY));
  svg.addEventListener('mouseleave', hideTrafficTooltip);
}

function setText(id, text) {
  const el = document.getElementById(id);
  if (el) el.textContent = text;
}

async function refreshDashboard() {
  let snap;
  try {
    snap = await invoke('get_dashboard');
  } catch (e) {
    console.error(e);
    return; // Not running inside Tauri (e.g. opened as a plain file) — leave placeholders as-is.
  }

  setText('ov-iface-badge', `${snap.defaultIface} · ${snap.ifaceState}`);
  setText('ov-download', snap.downloadRate);
  setText('ov-upload', snap.uploadRate);
  setText('ov-mac', snap.mac);
  setText('ov-mtu', snap.mtu);
  setText('ov-speed', snap.linkSpeed);
  setText('ov-state', snap.ifaceState);

  setText('lm-download', snap.downloadRate);
  setText('lm-upload', snap.uploadRate);
  setText('lm-latency', snap.latency);
  setText('lm-total', snap.totalTransferred);
  // Sparkline shapes come from refreshLiveMetricsChart() instead — it
  // queries the window the "Last N min" select is set to, rather than
  // this snapshot's fixed short in-memory buffer.

  setText('ni-iface', snap.defaultIface);
  setText('ni-total-dl', snap.totalDownloaded);
  setText('ni-total-ul', snap.totalUploaded);
  setText('ni-conns', String(snap.activeConnections));
  setText('ni-errors', String(snap.errorsDrops));

  setText('qs-latency', snap.latency);
  setText('qs-peak-dl', snap.peakDownload);
  setText('qs-peak-ul', snap.peakUpload);
  setText('qs-total', snap.totalData);

  setText('updated-at', 'Updated ' + snap.lastUpdated);
}

// The 24H chart's finest bin is 15 minutes wide (see #bin-seg's options),
// so its bins can't meaningfully change within a single fast poll tick —
// refetching it every 2s like the live counters would just be wasted work
// against a history table that only grows. Only actually query on the
// first dashboard visit, when the bin selector changes (`force`), or once
// this many ms have passed since the last real fetch.
const TRAFFIC_CHART_REFETCH_MS = 30000;
let lastTrafficChartFetchAt = 0;

async function refreshTrafficChart(force = false) {
  const now = Date.now();
  if (!force && now - lastTrafficChartFetchAt < TRAFFIC_CHART_REFETCH_MS) return;
  lastTrafficChartFetchAt = now;

  const binSeconds = Number(document.getElementById('bin-seg').value) || 3600;
  let chart;
  try {
    chart = await invoke('get_traffic_chart', { binSeconds });
  } catch (e) {
    console.error(e);
    return;
  }
  const { dlArea, ulArea, dlLine, ulLine, maxVal } = buildTrafficPaths(chart.download, chart.upload);
  document.getElementById('traffic24h-dl-area').setAttribute('d', dlArea);
  document.getElementById('traffic24h-ul-area').setAttribute('d', ulArea);
  document.getElementById('traffic24h-dl-line').setAttribute('d', dlLine);
  document.getElementById('traffic24h-ul-line').setAttribute('d', ulLine);
  renderTrafficGrid(maxVal, { grid: 'traffic24h-grid', yaxisL: 'traffic24h-yaxis-l', yaxisR: 'traffic24h-yaxis-r' });
  updateTrafficLabels(binSeconds, chart.download.length);
  trafficChartState = { download: chart.download, upload: chart.upload, latency: chart.latency, binSeconds };
}

async function refreshLiveMetricsChart() {
  const select = document.getElementById('lm-window');
  const minutes = select ? Number(select.value) : 15;
  let chart;
  try {
    chart = await invoke('get_live_metrics_chart', { minutes });
  } catch (e) {
    console.error(e);
    return;
  }
  document.getElementById('lm-dl-spark').setAttribute('points', sparklinePoints(chart.download));
  document.getElementById('lm-ul-spark').setAttribute('points', sparklinePoints(chart.upload));
  document.getElementById('lm-lat-spark').setAttribute('points', sparklinePoints(chart.latency));
}

function initDashboard() {
  document.getElementById('bin-seg').addEventListener('change', () => refreshTrafficChart(true));
  document.getElementById('lm-window').addEventListener('change', refreshLiveMetricsChart);
  initTrafficHover();
  document.getElementById('lt-range-seg').addEventListener('change', refreshLiveTraffic);
}

// Escapes real system strings (process names, SSIDs, MACs, ...) before
// they go into innerHTML — this is live data from the machine, not
// trusted markup.
function escapeHtml(s) {
  const div = document.createElement('div');
  div.textContent = s == null ? '' : String(s);
  // The text-node round-trip above only escapes &/</> — safe for
  // element *content*, but several call sites interpolate this into a
  // quoted HTML *attribute* (e.g. the VPN table's data-name), and a
  // real system value (an nmcli connection name, say) can contain a
  // literal `"`. Escape quotes too so a crafted name can't break out
  // of the attribute and inject markup/handlers.
  return div.innerHTML.replaceAll('"', '&quot;').replaceAll("'", '&#39;');
}

// — Live Telemetry / Top Bandwidth Consumers / Session (Dashboard's
// second column) — reuses the 24H chart's builder at a taller viewBox,
// and get_dashboard's snapshot for the Session card (peaks/totals are
// session-wide, not range-specific).
async function refreshLiveTraffic() {
  const minutes = Number(document.getElementById('lt-range-seg').value) || 60;
  let chart, snap;
  try {
    [chart, snap] = await Promise.all([invoke('get_live_metrics_chart', { minutes }), invoke('get_dashboard')]);
  } catch (e) {
    console.error(e);
    return;
  }
  const { dlArea, ulArea, dlLine, ulLine, maxVal } = buildTrafficPaths(chart.download, chart.upload);
  document.getElementById('lt-dl-area').setAttribute('d', dlArea);
  document.getElementById('lt-ul-area').setAttribute('d', ulArea);
  document.getElementById('lt-dl-line').setAttribute('d', dlLine);
  document.getElementById('lt-ul-line').setAttribute('d', ulLine);
  renderTrafficGrid(maxVal, { grid: 'lt-grid', yaxisL: 'lt-yaxis-l', yaxisR: 'lt-yaxis-r' });
  updateTimestampLabelsFromRows('lt-labels', chart.timestamps);

  setText('lt-current', `${snap.downloadRate} / ${snap.uploadRate}`);
  setText('lt-peak-dl', snap.peakDownload);
  setText('lt-peak-ul', snap.peakUpload);
  setText('lt-total', snap.totalData);
  setText('lt-errors', String(snap.errorsDrops));
}

// — Connections — full list fetched once per refresh, search/filter/
// sort/pagination all applied client-side so they're instant.
let connectionsData = [];
const CONN_PAGE_SIZE = 10;
let connSortCol = 'durationSecs'; // default sort, per the design ask
let connSortDesc = true; // longest-tracked connections first
let connPage = 0;
const CONN_STRING_COLS = new Set(['proto', 'local', 'peer', 'process', 'state']);

function sortConnections(rows) {
  const sorted = rows.slice();
  sorted.sort((a, b) => {
    if (CONN_STRING_COLS.has(connSortCol)) {
      const cmp = String(a[connSortCol]).localeCompare(String(b[connSortCol]));
      return connSortDesc ? -cmp : cmp;
    }
    // Numeric columns (sentBytes/recvBytes/durationSecs): null/undefined
    // — nothing to sort by yet (UDP has no byte counters; a connection's
    // first poll has no duration/rate) — always sorts last, in either
    // direction, rather than being treated as a fake 0.
    const av = a[connSortCol];
    const bv = b[connSortCol];
    if (av == null && bv == null) return 0;
    if (av == null) return 1;
    if (bv == null) return -1;
    const cmp = av - bv;
    return connSortDesc ? -cmp : cmp;
  });
  return sorted;
}

function updateConnSortHeaders() {
  for (const th of document.querySelectorAll('#conn-head th.sortable')) {
    const col = th.dataset.sort;
    th.classList.toggle('active', col === connSortCol);
    const base = th.textContent.replace(/ [▲▼]$/, '');
    th.textContent = col === connSortCol ? `${base} ${connSortDesc ? '▼' : '▲'}` : base;
  }
}

function renderConnections() {
  const tbody = document.getElementById('conn-body');
  const search = (document.getElementById('conn-search').value || '').toLowerCase();
  const filter = document.getElementById('conn-filter-seg').value;
  let rows = connectionsData.filter((c) => {
    if (filter !== 'all' && c.proto.toLowerCase() !== filter) return false;
    if (search && !c.peer.toLowerCase().includes(search) && !c.process.toLowerCase().includes(search)) return false;
    return true;
  });
  rows = sortConnections(rows);

  const total = rows.length;
  const totalPages = Math.max(1, Math.ceil(total / CONN_PAGE_SIZE));
  connPage = Math.min(connPage, totalPages - 1);
  const pageRows = rows.slice(connPage * CONN_PAGE_SIZE, (connPage + 1) * CONN_PAGE_SIZE);

  setText('conn-count', `Active Connections · ${total} shown`);
  setText('conn-page-label', `Page ${connPage + 1} of ${totalPages}`);
  document.getElementById('conn-prev-btn').disabled = connPage === 0;
  document.getElementById('conn-next-btn').disabled = connPage + 1 >= totalPages;

  // Total + current rate together ("1.6 MB · 12.3 KB/s") rather than two
  // more columns — the rate is only "—" on a connection's very first
  // tick (needs two polls to diff), which is honest, not a bug.
  const sentRecvCell = (total, rate) => (rate === '—' ? escapeHtml(total) : `${escapeHtml(total)} · ${escapeHtml(rate)}`);
  tbody.innerHTML = pageRows.length
    ? pageRows
        .map(
          (c) =>
            `<tr><td>${escapeHtml(c.proto)}</td><td>${escapeHtml(c.local)}</td><td>${escapeHtml(c.peer)}</td><td>${escapeHtml(c.process)}</td><td><span class="tag tag-neutral">${escapeHtml(c.state)}</span></td><td>${sentRecvCell(c.sent, c.sentRate)}</td><td>${sentRecvCell(c.recv, c.recvRate)}</td><td>${escapeHtml(c.duration)}</td></tr>`
        )
        .join('')
    : `<tr><td colspan="8" class="text-muted">${
        search || filter !== 'all' ? 'No connections match this filter.' : 'No active connections'
      }</td></tr>`;

  updateConnSortHeaders();
}
// The main table and the Geo-IP card both need a fresh `ss -tuinp`
// snapshot, and get_connections_page computes both from a single one
// backend-side rather than shelling out to `ss` twice per poll tick
// (see that command's doc comment) — so one fetch here feeds both
// render paths instead of two independent invoke() calls.
async function refreshConnections() {
  try {
    const page = await invoke('get_connections_page');
    connectionsData = page.connections;
    geoConnectionsData = page.geo;
  } catch (e) {
    console.error(e);
    return;
  }
  renderConnections();
  renderGeoTable(geoConnectionsData);
  renderGeoDots(geoConnectionsData);
}

// — Geo-IP — real peer locations + registrant (ASN/org) info via two
// local MMDB databases (DB-IP City Lite + ASN Lite), downloaded on
// demand into the app's data dir since together they're ~65MB and go
// stale monthly, not bundled with the app.
function renderGeoDots(rows) {
  const g = document.getElementById('geoip-dots');
  // Simple equirectangular projection into the 1000x320 viewBox — this
  // card is an abstract grid, not a real map image, so a straight
  // lon/lat -> x/y scale fits the existing look rather than needing map
  // tile assets this offline app has no business bundling.
  g.innerHTML = rows
    .map((r) => {
      const x = ((r.lon + 180) / 360) * 1000;
      const y = ((90 - r.lat) / 180) * 320;
      return `<circle cx="${x.toFixed(1)}" cy="${y.toFixed(1)}" r="4" fill="var(--color-accent)" fill-opacity="0.85"><title>${escapeHtml(r.city)}, ${escapeHtml(r.country)}</title></circle>`;
    })
    .join('');
}

// Paginated at 10 rows/page like the main Connections table — the map
// above it always plots every real location though, since pagination
// is a table-legibility concern, not something a scatter plot needs.
const GEOIP_PAGE_SIZE = 10;
let geoipPage = 0;
let geoConnectionsData = [];

function renderGeoTable(rows) {
  const tbody = document.getElementById('geoip-body');
  const total = rows.length;
  const totalPages = Math.max(1, Math.ceil(total / GEOIP_PAGE_SIZE));
  geoipPage = Math.min(geoipPage, totalPages - 1);
  const pageRows = rows.slice(geoipPage * GEOIP_PAGE_SIZE, (geoipPage + 1) * GEOIP_PAGE_SIZE);

  setText('geoip-page-label', `Page ${geoipPage + 1} of ${totalPages}`);
  document.getElementById('geoip-prev-btn').disabled = geoipPage === 0;
  document.getElementById('geoip-next-btn').disabled = geoipPage + 1 >= totalPages;

  tbody.innerHTML = pageRows.length
    ? pageRows
        .map(
          (r) =>
            `<tr><td>${escapeHtml(r.city)}, ${escapeHtml(r.country)}</td><td>${escapeHtml(r.ip)}</td><td>${escapeHtml(r.registrant)}</td><td>${escapeHtml(r.process)}</td></tr>`
        )
        .join('')
    : `<tr><td colspan="4" class="text-muted">No public peers with location data right now</td></tr>`;
}

// Just the status text/button — the actual peer data is fetched and
// rendered by refreshConnections() now (see get_connections_page's doc
// comment), since get_geoip_status is a separate, cheap fs::metadata
// check unrelated to the `ss` snapshot the table/map need.
async function refreshGeoIpStatus() {
  let status;
  try {
    status = await invoke('get_geoip_status');
  } catch (e) {
    console.error(e);
    return;
  }
  const statusEl = document.getElementById('geoip-status');
  const btn = document.getElementById('geoip-download-btn');
  if (!status.present) {
    statusEl.textContent = 'No local GeoIP database yet — download one to see real peer locations.';
    btn.textContent = 'Download database (~65MB)';
    return;
  }
  const age = status.ageDays === 0 ? 'today' : status.ageDays === 1 ? '1 day ago' : `${status.ageDays} days ago`;
  statusEl.textContent =
    status.ageDays > 40
      ? `Database is ${age} old (DB-IP republishes monthly) — consider updating.`
      : `Database updated ${age}.`;
  btn.textContent = 'Update database';
}

function initConnections() {
  document.getElementById('conn-search').addEventListener('input', () => {
    connPage = 0;
    renderConnections();
  });
  document.getElementById('conn-filter-seg').addEventListener('change', () => {
    connPage = 0;
    renderConnections();
  });
  document.getElementById('conn-prev-btn').addEventListener('click', () => {
    connPage = Math.max(0, connPage - 1);
    renderConnections();
  });
  document.getElementById('conn-next-btn').addEventListener('click', () => {
    connPage += 1;
    renderConnections();
  });
  document.getElementById('conn-head').addEventListener('click', (e) => {
    const th = e.target.closest('th.sortable');
    if (!th) return;
    const col = th.dataset.sort;
    if (connSortCol === col) {
      connSortDesc = !connSortDesc;
    } else {
      connSortCol = col;
      connSortDesc = true;
    }
    connPage = 0;
    renderConnections();
  });

  document.getElementById('geoip-download-btn').addEventListener('click', async () => {
    const btn = document.getElementById('geoip-download-btn');
    const statusEl = document.getElementById('geoip-status');
    btn.disabled = true;
    statusEl.textContent = 'Downloading city + ASN databases (~65MB, may take a moment)…';
    try {
      await invoke('download_geoip_database');
      statusEl.textContent = 'Downloaded — loading peer locations…';
      await Promise.all([refreshGeoIpStatus(), refreshConnections()]);
    } catch (e) {
      console.error(e);
      statusEl.textContent = `Download failed: ${e}`;
    } finally {
      btn.disabled = false;
    }
  });
  document.getElementById('geoip-prev-btn').addEventListener('click', () => {
    geoipPage = Math.max(0, geoipPage - 1);
    renderGeoTable(geoConnectionsData);
  });
  document.getElementById('geoip-next-btn').addEventListener('click', () => {
    geoipPage += 1;
    renderGeoTable(geoConnectionsData);
  });
}

// Generic column-sort helper for tables that don't need Connections'
// extra pagination/filter complexity (Processes, Reports) -- same
// active-column-class + arrow-appended-to-textContent convention as
// updateConnSortHeaders above, just parameterized instead of
// copy-pasted a second and third time.
function createSortState(col, desc, stringCols) {
  return { col, desc, stringCols: new Set(stringCols) };
}
function applySort(rows, state) {
  const sorted = rows.slice();
  sorted.sort((a, b) => {
    if (state.stringCols.has(state.col)) {
      const cmp = String(a[state.col]).localeCompare(String(b[state.col]));
      return state.desc ? -cmp : cmp;
    }
    const av = a[state.col];
    const bv = b[state.col];
    if (av == null && bv == null) return 0;
    if (av == null) return 1;
    if (bv == null) return -1;
    const cmp = av - bv;
    return state.desc ? -cmp : cmp;
  });
  return sorted;
}
function updateSortHeaders(headId, state) {
  for (const th of document.querySelectorAll(`#${headId} th.sortable`)) {
    const col = th.dataset.sort;
    th.classList.toggle('active', col === state.col);
    const base = th.textContent.replace(/ [▲▼]$/, '');
    th.textContent = col === state.col ? `${base} ${state.desc ? '▼' : '▲'}` : base;
  }
}
function initSortClicks(headId, state, onSort) {
  document.getElementById(headId).addEventListener('click', (e) => {
    const th = e.target.closest('th.sortable');
    if (!th) return;
    const col = th.dataset.sort;
    if (state.col === col) {
      state.desc = !state.desc;
    } else {
      state.col = col;
      state.desc = true;
    }
    onSort();
  });
}

// — Processes —
let processesData = [];
// 'pid' is a String field in the DTO but sorts fine through the numeric
// branch below (JS coerces "12" - "9" to a real number), so only 'name'
// needs the string/localeCompare path.
const procSort = createSortState('connections', true, ['name']);
function renderProcesses() {
  const tbody = document.getElementById('proc-body');
  const search = (document.getElementById('proc-search').value || '').toLowerCase();
  const filtered = processesData.filter((p) => !search || p.name.toLowerCase().includes(search) || p.pid.includes(search));
  const rows = applySort(filtered, procSort);
  updateSortHeaders('proc-head', procSort);
  if (rows.length === 0) {
    const message = search ? `No processes match "${escapeHtml(document.getElementById('proc-search').value)}".` : 'No processes with open connections';
    tbody.innerHTML = `<tr><td colspan="3" class="text-muted">${message}</td></tr>`;
    return;
  }
  tbody.innerHTML = rows
    .map((p) => `<tr><td>${escapeHtml(p.pid)}</td><td>${escapeHtml(p.name || '—')}</td><td>${p.connections}</td></tr>`)
    .join('');
}
async function refreshProcesses() {
  try {
    processesData = await invoke('get_processes');
  } catch (e) {
    console.error(e);
    return;
  }
  renderProcesses();
}
function initProcesses() {
  document.getElementById('proc-search').addEventListener('input', renderProcesses);
  initSortClicks('proc-head', procSort, renderProcesses);
}

// — Wi-Fi Analyzer —
let wifiNetworksData = [];
function renderWifiTable() {
  const tbody = document.getElementById('wifi-body');
  const search = (document.getElementById('wifi-search').value || '').toLowerCase();
  const rows = wifiNetworksData.filter((n) => !search || n.ssid.toLowerCase().includes(search));
  tbody.innerHTML = rows.length
    ? rows
        .map(
          (n) => `<tr>
      <td>${escapeHtml(n.ssid)}${n.inUse ? ' <span class="tag tag-accent" style="margin-left:6px">connected</span>' : ''}</td>
      <td>${escapeHtml(n.security)}</td><td>Ch ${escapeHtml(n.channel)} · ${escapeHtml(n.band)}</td><td>${escapeHtml(n.speed)}</td>
      <td class="text-muted">${escapeHtml(n.bssid)}</td>
      <td><div style="display:flex;align-items:center;gap:8px"><div style="height:6px;flex:1;background:var(--color-divider)"><div style="height:6px;background:var(--color-accent-600);width:${n.signal}%"></div></div><span style="font-size:12px;width:30px;text-align:right">${n.signal}%</span></div></td>
    </tr>`
        )
        .join('')
    : `<tr><td colspan="6" class="text-muted">No networks found</td></tr>`;
}
async function refreshWifi() {
  let snap;
  try {
    snap = await invoke('get_wifi');
  } catch (e) {
    console.error(e);
    return;
  }
  wifiNetworksData = snap.networks;
  if (snap.connected) {
    setText('wifi-signal', `${snap.connected.signal} %`);
    setText('wifi-ssid', snap.connected.ssid);
    setText('wifi-security', snap.connected.security);
    setText('wifi-detail', `Ch ${snap.connected.channel} · ${snap.connected.band} · ${snap.connected.speed} · ${snap.connected.bssid}`);
  } else {
    setText('wifi-signal', '— %');
    setText('wifi-ssid', 'Not connected');
    setText('wifi-security', '—');
    setText('wifi-detail', 'Ch — · — · — · —');
  }
  setText(
    'wifi-count',
    `Nearby Networks · ${snap.networks.length} · updated ${new Date().toLocaleTimeString(undefined, { hour12: false })}`
  );
  renderWifiTable();
}
function initWifi() {
  document.getElementById('wifi-search').addEventListener('input', renderWifiTable);
}

// — Interfaces —
async function refreshInterfaces() {
  let snap;
  try {
    snap = await invoke('get_interfaces');
  } catch (e) {
    console.error(e);
    return;
  }
  document.getElementById('iface-body').innerHTML = snap.interfaces.length
    ? snap.interfaces
        .map(
          (i) => `<tr>
      <td>${i.isDefault ? '<span class="tag tag-accent">default</span>' : ''}</td><td>${escapeHtml(i.name)}</td><td>${escapeHtml(i.kind)}</td>
      <td>${escapeHtml(i.ip)}</td><td class="text-muted">${escapeHtml(i.mac)}</td>
      <td><span class="tag ${i.status === 'Up' ? 'tag-accent' : 'tag-neutral'}">${escapeHtml(i.status)}</span></td>
      <td>${escapeHtml(i.speed)}</td><td>${escapeHtml(i.rx)}</td><td>${escapeHtml(i.tx)}</td>
    </tr>`
        )
        .join('')
    : `<tr><td colspan="9" class="text-muted">No interfaces found</td></tr>`;

  document.getElementById('bond-body').innerHTML = snap.bonding.length
    ? snap.bonding
        .map(
          (b) =>
            `<tr><td>${escapeHtml(b.name)}</td><td>${escapeHtml(b.mode)}</td><td>${escapeHtml(b.active)}</td><td class="text-muted">${escapeHtml(b.standby)}</td></tr>`
        )
        .join('')
    : `<tr><td colspan="4" class="text-muted">No bonded interfaces configured on this machine.</td></tr>`;

  setText('lan-count', `LAN Devices · ${snap.lan.length} discovered`);
  document.getElementById('lan-body').innerHTML = snap.lan.length
    ? snap.lan
        .map(
          (d) =>
            `<tr><td>${escapeHtml(d.ip)}</td><td class="text-muted">${escapeHtml(d.mac)}</td><td>${escapeHtml(d.iface)}</td><td>${escapeHtml(d.state)}</td></tr>`
        )
        .join('')
    : `<tr><td colspan="4" class="text-muted">No devices in the ARP/NDP cache yet</td></tr>`;
}

// — VPN —
async function refreshVpn() {
  let profiles;
  try {
    profiles = await invoke('get_vpn');
  } catch (e) {
    console.error(e);
    return;
  }
  const active = profiles.find((p) => p.active);
  setText('vpn-status', active ? 'Connected' : 'Disconnected');
  const statusEl = document.getElementById('vpn-status');
  if (statusEl) statusEl.className = 'tag ' + (active ? 'tag-accent' : 'tag-neutral');
  setText('vpn-active', active ? active.name : '—');

  document.getElementById('vpn-body').innerHTML = profiles.length
    ? profiles
        .map(
          (p) => `<tr><td>${escapeHtml(p.name)}</td><td>${escapeHtml(p.vpnType)}</td>
      <td>${p.active ? '<span class="tag tag-accent">Active</span>' : '<span class="tag tag-neutral">Inactive</span>'}</td>
      <td><button type="button" class="btn btn-ghost vpn-action-btn" data-name="${escapeHtml(p.name)}" data-active="${p.active}">${p.active ? 'Disconnect' : 'Connect'}</button></td>
    </tr>`
        )
        .join('')
    : `<tr><td colspan="4" class="text-muted">No VPN profiles configured in NetworkManager</td></tr>`;
}

function initVpn() {
  document.getElementById('vpn-body').addEventListener('click', async (e) => {
    const btn = e.target.closest('.vpn-action-btn');
    if (!btn) return;
    const name = btn.dataset.name;
    const wasActive = btn.dataset.active === 'true';
    btn.disabled = true;
    btn.textContent = wasActive ? 'Disconnecting…' : 'Connecting…';
    const errorEl = document.getElementById('vpn-error');
    errorEl.textContent = '';
    try {
      await invoke(wasActive ? 'vpn_disconnect' : 'vpn_connect', { name });
    } catch (err) {
      errorEl.textContent = String(err);
    }
    await refreshVpn();
  });
}

// — Firewall (read-only scan, manual only — never auto-refreshed, since
// every scan needs a real password prompt via pkexec) —
function renderFirewallChains(chains) {
  const container = document.getElementById('firewall-chains');
  if (!chains.length) {
    container.innerHTML = `<div class="text-muted" style="padding:8px 4px">Ruleset is empty.</div>`;
    return;
  }
  container.innerHTML = chains
    .map((c) => {
      const rules = c.rules.length
        ? c.rules
            .map(
              (r) => `<div style="display:flex;justify-content:space-between;gap:10px;padding:6px 0;border-bottom:1px solid var(--color-divider);font-size:13px">
            <span>${escapeHtml(r.text)}</span>${r.handle ? `<span class="text-muted" style="font-size:11px;white-space:nowrap">#${escapeHtml(r.handle)}</span>` : ''}
          </div>`
            )
            .join('')
        : `<div class="text-muted" style="font-size:12px;padding:4px 0">No rules in this chain.</div>`;
      return `<div style="margin-top:10px">
        <div style="font-family:var(--font-heading);font-weight:600;font-size:14px">${escapeHtml(c.table)} / ${escapeHtml(c.name)}</div>
        ${c.hookInfo ? `<div class="text-muted" style="font-size:11px">${escapeHtml(c.hookInfo)}</div>` : ''}
        <div style="margin-top:4px">${rules}</div>
      </div>`;
    })
    .join('');
}
function initFirewall() {
  document.getElementById('firewall-scan-btn').addEventListener('click', async () => {
    const btn = document.getElementById('firewall-scan-btn');
    const status = document.getElementById('firewall-status');
    btn.disabled = true;
    status.textContent = 'Scanning — check for a password prompt…';
    try {
      const chains = await invoke('scan_firewall');
      status.textContent = `Scanned ${new Date().toLocaleTimeString(undefined, { hour12: false })}`;
      renderFirewallChains(chains);
    } catch (e) {
      status.textContent = 'Failed: ' + e;
    }
    btn.disabled = false;
  });
}

// — Per-process bandwidth (nethogs, manual scan, shared by Processes
// and Live Traffic's "Top Bandwidth Consumers") —
async function runNethogsScan(btnId, statusId, render) {
  const btn = document.getElementById(btnId);
  const status = document.getElementById(statusId);
  btn.disabled = true;
  status.textContent = 'Scanning — ~10s, check for a password prompt…';
  try {
    const rows = await invoke('scan_nethogs');
    status.textContent = `Scanned ${new Date().toLocaleTimeString(undefined, { hour12: false })}`;
    render(rows);
  } catch (e) {
    status.textContent = 'Failed: ' + e;
  }
  btn.disabled = false;
}
function renderNethogsTable(bodyId, cols) {
  return (rows) => {
    document.getElementById(bodyId).innerHTML = rows.length
      ? rows.map((r) => cols(r)).join('')
      : `<tr><td colspan="4" class="text-muted">Scan completed but found no active per-process traffic.</td></tr>`;
  };
}
function initNethogsScans() {
  document.getElementById('nethogs-scan-btn').addEventListener('click', () =>
    runNethogsScan(
      'nethogs-scan-btn',
      'nethogs-status',
      renderNethogsTable(
        'nethogs-body',
        (r) =>
          `<tr><td>${r.pid ? escapeHtml(r.pid) : '—'}</td><td>${escapeHtml(r.program)}</td><td>${escapeHtml(r.sent)}</td><td>${escapeHtml(r.received)}</td></tr>`
      )
    )
  );
  document.getElementById('lt-nethogs-scan-btn').addEventListener('click', () =>
    runNethogsScan(
      'lt-nethogs-scan-btn',
      'lt-nethogs-status',
      renderNethogsTable(
        'lt-top-body',
        (r) =>
          `<tr><td>${escapeHtml(r.program)}${r.pid ? ` <span class="text-muted">(${escapeHtml(r.pid)})</span>` : ''}</td><td>${escapeHtml(r.sent)}</td><td>${escapeHtml(r.received)}</td></tr>`
      )
    )
  );
}

// — Packet Inspector (tcpdump, manual capture) —
async function runPacketCapture() {
  const btn = document.getElementById('capture-btn');
  const status = document.getElementById('capture-status');
  btn.disabled = true;
  status.textContent = 'Capturing — check for a password prompt…';
  try {
    const rows = await invoke('run_packet_capture');
    status.textContent = `Captured ${rows.length} packets at ${new Date().toLocaleTimeString(undefined, { hour12: false })}`;
    document.getElementById('capture-body').innerHTML = rows
      .map(
        (r) =>
          `<tr><td>${escapeHtml(r.time)}</td><td>${escapeHtml(r.source)}</td><td>${escapeHtml(r.destination)}</td><td>${escapeHtml(r.proto)}</td><td>${escapeHtml(r.length)}</td><td>${escapeHtml(r.info)}</td></tr>`
      )
      .join('');
  } catch (e) {
    status.textContent = 'Failed: ' + e;
  }
  btn.disabled = false;
}
function initPacketCapture() {
  document.getElementById('capture-btn').addEventListener('click', runPacketCapture);
}

// — Port Scanner (plain TCP connect scan, no root) —
function initPortScan() {
  document.getElementById('portscan-btn').addEventListener('click', async () => {
    const target = document.getElementById('portscan-target').value.trim();
    if (!target) return;
    const btn = document.getElementById('portscan-btn');
    const status = document.getElementById('portscan-status');
    btn.disabled = true;
    status.textContent = `Scanning ${target}…`;
    try {
      const rows = await invoke('run_port_scan', { target });
      const open = rows.filter((r) => r.state === 'Open');
      status.textContent = `${open.length} of ${rows.length} common ports open`;
      document.getElementById('portscan-body').innerHTML = rows
        .map(
          (r) =>
            `<tr><td>${r.port}</td><td>${escapeHtml(r.service)}</td><td><span class="tag ${r.state === 'Open' ? 'tag-accent' : 'tag-neutral'}">${r.state}</span></td></tr>`
        )
        .join('');
    } catch (e) {
      status.textContent = 'Failed: ' + e;
    }
    btn.disabled = false;
  });
}

// — Reports — the only screen here with a real mutating action (writes
// a CSV file under the app's data dir); everything else on this page
// stays read-only.
function formatBytesTotal(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

let reportsData = [];
const reportSort = createSortState('modifiedTs', true, ['name', 'period']);
// Tracked outside renderReports() rather than as a per-button
// dataset flag -- this page re-polls every refresh-interval tick
// (PAGE_REFRESHERS.reports, as low as 1s), which rebuilds the whole
// table and would otherwise silently wipe an "armed" button's state
// out from under the user before their confirm click ever lands. This
// module-level variable survives the re-render, so the row stays
// showing "Confirm?" across polls until the click or the timeout.
let armedReportPath = null;
let armedReportTimeout = null;

function disarmReportDelete() {
  armedReportPath = null;
  clearTimeout(armedReportTimeout);
  armedReportTimeout = null;
}

// Delete used to not exist at all in this build -- netmon_core::
// reports::delete_report was written but never wired to a Tauri
// command or a UI button, so there was no way to remove a generated
// CSV short of deleting the file by hand. Now real, with the same
// "click to arm (button becomes 'Confirm?'), click again within 3s, or
// it silently disarms" pattern battery-monitor's equivalent screen
// uses -- a table row has no room for a two-button confirm/cancel
// layout per row.
function renderReports() {
  const tbody = document.getElementById('report-body');
  const total = reportsData.reduce((sum, r) => sum + r.sizeBytes, 0);
  document.getElementById('report-total').textContent = `${reportsData.length} report${reportsData.length === 1 ? '' : 's'} · ${formatBytesTotal(total)}`;
  updateSortHeaders('report-head', reportSort);
  if (reportsData.length === 0) {
    tbody.innerHTML = `<tr><td colspan="5" class="text-muted">No reports generated yet</td></tr>`;
    return;
  }
  // The armed report may have just been deleted (or no longer exists
  // for any other reason) -- don't leave a dangling armed-state
  // pointing at nothing.
  if (armedReportPath && !reportsData.some((r) => r.path === armedReportPath)) disarmReportDelete();
  const rows = applySort(reportsData, reportSort);
  tbody.innerHTML = rows
    .map((r) => {
      const armed = r.path === armedReportPath;
      return `<tr><td>${escapeHtml(r.name)}</td><td>${escapeHtml(r.period)}</td><td>${escapeHtml(r.date)}</td><td>${escapeHtml(r.size)}</td><td><button type="button" class="btn btn-ghost" data-delete-report="${escapeHtml(r.path)}" style="font-size:12px">${armed ? 'Confirm?' : 'Delete'}</button></td></tr>`;
    })
    .join('');
  tbody.querySelectorAll('[data-delete-report]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const path = btn.dataset.deleteReport;
      if (armedReportPath !== path) {
        armedReportPath = path;
        clearTimeout(armedReportTimeout);
        armedReportTimeout = setTimeout(() => {
          armedReportPath = null;
          renderReports();
        }, 3000);
        renderReports();
        return;
      }
      disarmReportDelete();
      await invoke('remove_report', { path });
      await refreshReports();
    });
  });
}

async function refreshReports() {
  try {
    reportsData = await invoke('get_reports');
  } catch (e) {
    console.error(e);
    return;
  }
  renderReports();
}

function initReportDeleteAll() {
  const btn = document.getElementById('report-delete-all-btn');
  btn.addEventListener('click', async () => {
    if (btn.dataset.armed !== 'true') {
      btn.dataset.armed = 'true';
      btn.textContent = 'Delete all — confirm?';
      setTimeout(() => {
        btn.dataset.armed = 'false';
        btn.textContent = 'Delete All';
      }, 3000);
      return;
    }
    btn.dataset.armed = 'false';
    btn.textContent = 'Delete All';
    for (const r of reportsData) {
      await invoke('remove_report', { path: r.path });
    }
    await refreshReports();
  });
}

function initReports() {
  initSortClicks('report-head', reportSort, renderReports);
  initReportDeleteAll();
  document.getElementById('report-generate-btn').addEventListener('click', async () => {
    const period = document.getElementById('report-period-seg').value;
    const statusEl = document.getElementById('report-status');
    statusEl.textContent = 'Generating…';
    try {
      await invoke('generate_report', { period });
      statusEl.textContent = 'Done';
      await refreshReports();
    } catch (e) {
      statusEl.textContent = 'Failed: ' + e;
    }
    setTimeout(() => {
      statusEl.textContent = '';
    }, 3000);
  });
}

// — Speed Test (DNS Resolver card only — everything else on this
// screen needs a real-time streaming/privileged backend not wired up) —
async function refreshSpeedtestDns() {
  let dns;
  try {
    dns = await invoke('get_dns');
  } catch (e) {
    console.error(e);
    return;
  }
  if (dns) {
    setText('dns-total', String(dns.totalQueries));
    setText('dns-cache-size', String(dns.cacheSize));
    setText('dns-hit-rate', dns.cacheHitRate);
  } else {
    setText('dns-total', '—');
    setText('dns-cache-size', '—');
    setText('dns-hit-rate', '—');
  }
}

async function runSpeedTest() {
  const btn = document.getElementById('speedtest-run-btn');
  const status = document.getElementById('speedtest-status');
  btn.disabled = true;
  status.textContent = 'Running — this takes a few seconds…';
  try {
    const result = await invoke('run_speed_test');
    setText('speedtest-ping', result.pingMs != null ? result.pingMs.toFixed(0) : '—');
    setText('speedtest-download', result.downloadMbps != null ? result.downloadMbps.toFixed(1) : '—');
    setText('speedtest-upload', result.uploadMbps != null ? result.uploadMbps.toFixed(1) : '—');
    status.textContent = result.error
      ? `Failed: ${result.error}`
      : "Idle — real transfer against Cloudflare's public endpoint, not simulated";
  } catch (e) {
    status.textContent = 'Failed: ' + e;
  }
  btn.disabled = false;
}

async function runPing() {
  const host = document.getElementById('ping-host').value.trim();
  if (!host) return;
  const btn = document.getElementById('ping-run-btn');
  const out = document.getElementById('ping-output');
  btn.disabled = true;
  out.textContent = 'Running…';
  try {
    out.textContent = await invoke('run_ping', { host });
  } catch (e) {
    out.textContent = 'Failed: ' + e;
  }
  btn.disabled = false;
}

async function runTraceroute() {
  const host = document.getElementById('trace-host').value.trim();
  if (!host) return;
  const btn = document.getElementById('trace-run-btn');
  const out = document.getElementById('trace-output');
  btn.disabled = true;
  out.textContent = 'Running — traceroute can take up to a minute…';
  try {
    out.textContent = await invoke('run_traceroute', { host });
  } catch (e) {
    out.textContent = 'Failed: ' + e;
  }
  btn.disabled = false;
}

function initSpeedtest() {
  document.getElementById('speedtest-run-btn').addEventListener('click', runSpeedTest);
  document.getElementById('ping-run-btn').addEventListener('click', runPing);
  document.getElementById('trace-run-btn').addEventListener('click', runTraceroute);
}

// — Alerts —
function renderAlertsList(alerts) {
  const filter = document.getElementById('alert-filter-seg').value;
  const rows = alerts.filter((a) => filter === 'all' || a.severity.toLowerCase() === filter);
  const list = document.getElementById('alert-list');
  if (rows.length === 0) {
    list.innerHTML = `<div class="text-muted" style="padding:12px 4px">No alerts yet — this session's traffic baseline hasn't shown any spikes.</div>`;
    return;
  }
  list.innerHTML = rows
    .map((a) => {
      const tagClass = a.severity === 'Critical' ? 'tag-accent' : 'tag-neutral';
      const dotColor = a.severity === 'Critical' ? 'var(--color-ul)' : 'var(--color-lat)';
      return `<div style="display:flex;gap:12px;align-items:flex-start;padding:12px 4px;border-bottom:1px solid var(--color-divider)">
        <span style="width:8px;height:8px;border-radius:50%;background:${dotColor};margin-top:6px;flex:none;opacity:${a.read ? 0.35 : 1}"></span>
        <div style="flex:1">
          <div style="display:flex;gap:8px;align-items:center"><span class="tag ${tagClass}">${escapeHtml(a.severity)}</span><span class="text-muted" style="font-size:12px">${escapeHtml(a.time)}</span></div>
          <div style="margin-top:4px;font-size:13px">${escapeHtml(a.message)}</div>
        </div>
      </div>`;
    })
    .join('');
}
async function refreshAlerts() {
  let snap;
  try {
    snap = await invoke('get_alerts');
  } catch (e) {
    console.error(e);
    return;
  }
  document.getElementById('alert-detect-toggle').checked = snap.detectionEnabled;
  renderAlertsList(snap.alerts);
}
function initAlerts() {
  document.getElementById('alert-detect-toggle').addEventListener('change', (e) => {
    invoke('set_anomaly_detection', { enabled: e.target.checked });
  });
  document.getElementById('alert-filter-seg').addEventListener('change', refreshAlerts);
  document.getElementById('alert-mark-all').addEventListener('click', async () => {
    await invoke('mark_all_alerts_read');
    refreshAlerts();
  });
}

// — Settings —
// Settings persist to a small JSON file (src-tauri's settings.json) —
// loaded once at startup (not on every page visit/2s tick, which would
// fight an in-progress slider drag) since nothing else in this
// single-client app changes them from outside.
let currentSettings = null;

function populateSettingsForm(s) {
  document.getElementById('settings-language').value = s.language;
  document.getElementById('refresh-interval-range').value = s.refreshIntervalSec;
  document.getElementById('refresh-interval-label').textContent = s.refreshIntervalSec;
  document.getElementById('notif-desktop-toggle').checked = s.notifDesktop;
  document.getElementById('notif-sound-toggle').checked = s.notifSound;
  document.getElementById('notif-tray-toggle').checked = s.notifTray;
  document.getElementById('cap-toggle').checked = s.bandwidthCapEnabled;
  document.getElementById('cap-gb-range').value = s.bandwidthCapGb;
  document.getElementById('cap-gb-label').textContent = s.bandwidthCapGb;
  document.getElementById('history-retention-range').value = s.historyRetentionDays;
  document.getElementById('history-retention-label').textContent = s.historyRetentionDays;
}

async function refreshHistoryStats() {
  try {
    const stats = await invoke('get_history_stats');
    document.getElementById('settings-db-path').textContent = stats.dbPath;
    document.getElementById('settings-row-count').textContent = stats.rowCount.toLocaleString();
  } catch (e) {
    console.error(e);
  }
}

function saveCurrentSettings() {
  if (!currentSettings) return;
  invoke('save_settings', { settings: currentSettings }).catch((e) => console.error(e));
}

// Real usage since the start of the current calendar month (get_data_usage
// sums the history db's cumulative counters, reset-resilient) — not just a
// saved cap number with nothing to compare it against.
async function refreshDataUsage() {
  let usage;
  try {
    usage = await invoke('get_data_usage');
  } catch (e) {
    console.error(e);
    return;
  }
  document.getElementById('cap-usage-bar').style.width = `${Math.min(100, usage.usedPercent)}%`;
  document.getElementById('cap-usage-bar').style.background =
    usage.capEnabled && usage.usedPercent >= 100 ? 'var(--color-ul)' : 'var(--color-accent-600)';
  const capNote = usage.capEnabled ? ` of ${usage.capGb} GB cap (${usage.usedPercent.toFixed(1)}%)` : ' this month';
  document.getElementById('cap-usage-text').textContent =
    `${usage.total} used${capNote} — ${usage.downloaded} down / ${usage.uploaded} up`;
}

// The 2s-poll interval id, so a persisted (or just-changed) refresh
// interval can actually replace it — otherwise "Refresh interval" would
// be a saved number with no effect on real polling cadence.
let pollIntervalHandle = null;
function applyRefreshInterval(seconds) {
  if (pollIntervalHandle) clearInterval(pollIntervalHandle);
  const ms = Math.max(1, Number(seconds) || 2) * 1000;
  pollIntervalHandle = setInterval(refreshCurrentPage, ms);
}

// The sidebar's profile card is single-user desktop software's only
// honest identity: the real logged-in OS username, not a generic
// "Local User" placeholder.
async function loadUsername() {
  let name;
  try {
    name = await invoke('get_username');
  } catch (e) {
    return; // not running inside Tauri — leave the "Local User" placeholder
  }
  document.getElementById('profile-name').textContent = name;
  document.getElementById('profile-menu-name').textContent = name;
  document.getElementById('settings-profile-name').textContent = name;
}

async function loadSettingsAndApply() {
  try {
    currentSettings = await invoke('get_settings');
  } catch (e) {
    console.error(e);
    currentSettings = null;
  }
  if (currentSettings) {
    populateSettingsForm(currentSettings);
    applyRefreshInterval(currentSettings.refreshIntervalSec);
  } else {
    applyRefreshInterval(2);
  }
  try {
    document.getElementById('autostart-toggle').checked = await invoke('get_autostart');
  } catch (e) {
    /* not running inside Tauri */
  }
  refreshDataUsage();
  refreshHistoryStats();
}

function initSettings() {
  document.getElementById('autostart-toggle').addEventListener('change', async (e) => {
    try {
      await invoke('set_autostart', { enabled: e.target.checked });
    } catch (err) {
      console.error(err);
      e.target.checked = !e.target.checked;
    }
  });

  document.getElementById('settings-language').addEventListener('change', (e) => {
    if (!currentSettings) return;
    currentSettings.language = e.target.value;
    saveCurrentSettings();
  });
  document.getElementById('refresh-interval-range').addEventListener('change', (e) => {
    if (!currentSettings) return;
    currentSettings.refreshIntervalSec = Number(e.target.value);
    saveCurrentSettings();
    applyRefreshInterval(currentSettings.refreshIntervalSec);
  });
  const wireToggle = (id, key) => {
    document.getElementById(id).addEventListener('change', (e) => {
      if (!currentSettings) return;
      currentSettings[key] = e.target.checked;
      saveCurrentSettings();
    });
  };
  wireToggle('notif-desktop-toggle', 'notifDesktop');
  wireToggle('notif-sound-toggle', 'notifSound');
  wireToggle('cap-toggle', 'bandwidthCapEnabled');

  // Tray icon is a real OS resource, not just a saved flag — created/
  // destroyed immediately on toggle, not only applied on next launch.
  document.getElementById('notif-tray-toggle').addEventListener('change', (e) => {
    if (!currentSettings) return;
    currentSettings.notifTray = e.target.checked;
    saveCurrentSettings();
    invoke('set_tray_enabled', { enabled: e.target.checked }).catch((err) => console.error(err));
  });
  document.getElementById('cap-gb-range').addEventListener('change', (e) => {
    if (!currentSettings) return;
    currentSettings.bandwidthCapGb = Number(e.target.value);
    saveCurrentSettings();
    refreshDataUsage();
  });

  document.getElementById('history-retention-range').addEventListener('change', (e) => {
    if (!currentSettings) return;
    currentSettings.historyRetentionDays = Number(e.target.value);
    saveCurrentSettings();
    // The backend just re-pruned against the new value -- refresh the
    // sample count so a lowered retention is visibly reflected right away.
    refreshHistoryStats();
  });

  document.getElementById('settings-clear-btn').addEventListener('click', () => {
    document.getElementById('settings-clear-normal').classList.add('hidden');
    document.getElementById('settings-clear-confirm').classList.remove('hidden');
  });
  document.getElementById('settings-clear-cancel-btn').addEventListener('click', () => {
    document.getElementById('settings-clear-confirm').classList.add('hidden');
    document.getElementById('settings-clear-normal').classList.remove('hidden');
  });
  document.getElementById('settings-clear-confirm-btn').addEventListener('click', async () => {
    await invoke('clear_history').catch((e) => console.error(e));
    document.getElementById('settings-clear-confirm').classList.add('hidden');
    document.getElementById('settings-clear-normal').classList.remove('hidden');
    refreshHistoryStats();
  });
}

// — Page dispatch — each screen's data is fetched lazily (on nav) and
// re-fetched every 2s only while it's the visible page, rather than
// continuously polling all 12 screens' backends regardless of what's
// on screen.
const PAGE_REFRESHERS = {
  dashboard: async (force = false) => {
    await Promise.all([
      refreshDashboard(),
      refreshTrafficChart(force),
      refreshLiveMetricsChart(),
      refreshLiveTraffic(),
    ]);
  },
  connections: async () => {
    await Promise.all([refreshConnections(), refreshGeoIpStatus()]);
  },
  processes: refreshProcesses,
  wifi: refreshWifi,
  interfaces: refreshInterfaces,
  vpn: refreshVpn,
  reports: refreshReports,
  speedtest: refreshSpeedtestDns,
  alerts: refreshAlerts,
  // No 'settings' entry: its form is populated once at startup by
  // loadSettingsAndApply(), not re-fetched on every visit/2s tick,
  // which would otherwise fight an in-progress slider drag.
};

async function refreshCurrentPage(force = false) {
  const activeBtn = document.querySelector('.nav-btn.active');
  const page = activeBtn ? activeBtn.dataset.page : 'dashboard';
  const fn = PAGE_REFRESHERS[page];
  if (fn) await fn(force);
  // Dashboard shows the backend's own last-updated timestamp instead —
  // see refreshDashboard().
  if (page !== 'dashboard') {
    setText('updated-at', 'Updated ' + new Date().toLocaleTimeString(undefined, { hour12: false }));
  }
}

window.addEventListener('DOMContentLoaded', () => {
  initNav();
  initTheme();
  initRefresh();
  initDropdowns();
  initDialogs();
  initRangeLabels();
  initDashboard();
  initConnections();
  initProcesses();
  initWifi();
  initReports();
  initVpn();
  initSpeedtest();
  initAlerts();
  initSettings();
  initQuickActionShortcuts();
  initFirewall();
  initNethogsScans();
  initPacketCapture();
  initPortScan();
  refreshCurrentPage();
  loadSettingsAndApply();
  loadUsername();
});
