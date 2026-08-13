/* zing-gui main window logic */
const invoke = window.__TAURI__.core.invoke;

// ── State ───────────────────────────────────────────────────────

let tasks = [];
let filteredIds = [];
let selectedId = null;
let category = 0;
let suppressRowCb = false;
let lastClick = { index: -1, time: 0 };
let appVersion = '';

const CATEGORIES = [
  { label: 'All Downloads', matches: function() { return true; } },
  { label: 'Downloading', matches: function(t) { return !t.done && !t.paused && t.total_bytes > 0; } },
  { label: 'Complete', matches: function(t) { return t.status === 'Completed'; } },
  { label: 'Paused', matches: function(t) { return t.paused; } },
  { label: 'Queued', matches: function(t) { return t.total_bytes === 0 && !t.done && !t.paused; } },
  { label: 'Failed', matches: function(t) { return t.status.startsWith('Failed'); } },
  { label: 'Stopped', matches: function(t) { return t.status === 'Stopped'; } },
];

var CATEGORY_ICONS = [
  '<svg viewBox="0 0 24 24"><rect width="7" height="7" x="3" y="3" rx="1"/><rect width="7" height="7" x="14" y="3" rx="1"/><rect width="7" height="7" x="14" y="14" rx="1"/><rect width="7" height="7" x="3" y="14" rx="1"/></svg>',
  '<svg viewBox="0 0 24 24"><path d="M12 15V3"/><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/></svg>',
  '<svg viewBox="0 0 24 24"><path d="M21.801 10A10 10 0 1 1 17 3.335"/><path d="m9 11 3 3L22 4"/></svg>',
  '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><line x1="10" x2="10" y1="15" y2="9"/><line x1="14" x2="14" y1="15" y2="9"/></svg>',
  '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/></svg>',
  '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><line x1="12" x2="12" y1="8" y2="12"/><line x1="12" x2="12.01" y1="16" y2="16"/></svg>',
  '<svg viewBox="0 0 24 24"><path d="M2.586 16.726A2 2 0 0 1 2 15.312V8.688a2 2 0 0 1 .586-1.414l4.688-4.688A2 2 0 0 1 8.688 2h6.624a2 2 0 0 1 1.414.586l4.688 4.688A2 2 0 0 1 22 8.688v6.624a2 2 0 0 1-.586 1.414l-4.688 4.688a2 2 0 0 1-1.414.586H8.688a2 2 0 0 1-1.414-.586z"/></svg>',
];

// ── Formatting helpers ─────────────────────────────────────────

function formatBytes(n) {
  if (n === 0) return '\u2014';
  var units = ['B', 'KB', 'MB', 'GB', 'TB'];
  var v = n, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return v.toFixed(1) + ' ' + units[i];
}

function formatSpeed(s) {
  if (s === 0) return '\u2014';
  return formatBytes(Math.round(s)) + '/s';
}

function etaText(t) {
  if (t.done || t.paused || t.speed === 0 || t.total_bytes === 0) return '\u2014';
  var remaining = t.total_bytes - t.downloaded;
  var secs = Math.round(remaining / t.speed);
  if (secs < 60) return secs + 's';
  if (secs < 3600) return Math.floor(secs / 60) + 'm ' + (secs % 60) + 's';
  return Math.floor(secs / 3600) + 'h ' + Math.floor((secs % 3600) / 60) + 'm';
}

function statusInfo(t) {
  if (t.status === 'Completed') return { text: 'Complete', css: 'complete' };
  if (t.status.startsWith('Failed')) return { text: 'Failed', css: 'failed' };
  if (t.paused) return { text: 'Paused', css: 'paused' };
  if (t.status === 'Stopped') return { text: 'Stopped', css: 'stopped' };
  if (t.total_bytes === 0) return { text: 'Queued', css: 'queued' };
  return { text: 'Downloading', css: 'downloading' };
}

function progressPct(t) {
  if (t.total_bytes === 0) return 0;
  return Math.round((t.downloaded / t.total_bytes) * 100);
}

function statusColor(t) {
  if (t.status === 'Completed') return '#3ecf74';
  if (t.status.startsWith('Failed')) return '#f2555f';
  if (t.paused) return '#f0a83c';
  if (t.status === 'Stopped') return '#6c6d78';
  if (t.total_bytes === 0) return '#7ab0ea';
  return '#5b7fff';
}

function esc(s) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

// ── Window helpers ─────────────────────────────────────────────

function openWin(label, url, w, h) {
  invoke('open_window_cmd', {
    label: label,
    url: url,
    title: 'zing - ' + label,
    width: w || 520,
    height: h || 500,
  }).catch(function(e) { console.error('open window failed:', e); });
}

function animateBtn(el) {
  el.classList.add('btn-press');
  setTimeout(function() { el.classList.remove('btn-press'); }, 150);
}

// ── Rendering ──────────────────────────────────────────────────

function renderCategories() {
  var el = document.getElementById('categories');
  el.innerHTML = CATEGORIES.map(function(cat, i) {
    var count = tasks.filter(function(t) { return cat.matches(t); }).length;
    var active = i === category ? ' active' : '';
    return '<div class="cat-item' + active + '" data-idx="' + i + '">' +
      CATEGORY_ICONS[i] +
      '<span class="cat-label">' + cat.label + '</span>' +
      '<span class="cat-count">' + count + '</span>' +
    '</div>';
  }).join('');

  el.querySelectorAll('.cat-item').forEach(function(item) {
    item.addEventListener('click', function() {
      category = parseInt(item.dataset.idx);
      applyFilter();
      renderCategories();
      renderTable();
      updateToolbar();
    });
  });
}

function renderTable() {
  var tbody = document.getElementById('task-list');
  tbody.innerHTML = filteredIds.map(function(id, idx) {
    var t = tasks.find(function(x) { return x.id === id; });
    if (!t) return '';
    var si = statusInfo(t);
    var pct = progressPct(t);
    var sel = t.id === selectedId ? ' selected' : '';
    return '<tr data-id="' + t.id + '" data-idx="' + idx + '" class="' + sel + '">' +
      '<td class="col-name" title="' + esc(t.filename) + '">' + esc(t.filename) + '</td>' +
      '<td class="col-size">' + formatBytes(t.total_bytes) + '</td>' +
      '<td class="col-status"><span class="status-dot ' + si.css + '">' + si.text + '</span></td>' +
      '<td class="col-speed">' + ((t.paused || t.done || t.speed === 0) ? '\u2014' : formatSpeed(t.speed)) + '</td>' +
      '<td class="col-eta">' + etaText(t) + '</td>' +
      '<td class="col-conns">' + t.connections.length + '</td>' +
      '<td class="col-progress">' + pct + '%</td>' +
    '</tr>';
  }).join('');

  tbody.querySelectorAll('tr').forEach(function(tr) {
    tr.addEventListener('click', function() {
      var id = parseInt(tr.dataset.id);
      var idx = parseInt(tr.dataset.idx);
      if (suppressRowCb) return;
      var now = Date.now();
      var double = lastClick.index === idx && now - lastClick.time < 500;
      lastClick = { index: idx, time: now };
      selectedId = id;
      renderTable();
      updateToolbar();
      renderDetail();
      if (double) openProgress(id);
    });
  });
}

function renderDetail() {
  var t = tasks.find(function(x) { return x.id === selectedId; });
  var fn = document.getElementById('det-filename');
  var pill = document.getElementById('det-pill');
  var url = document.getElementById('det-url');

  if (!t) {
    fn.textContent = 'No task selected';
    pill.style.display = 'none';
    url.textContent = '';
    document.getElementById('det-size').textContent = '';
    document.getElementById('det-downloaded').textContent = '';
    document.getElementById('det-conns').textContent = '';
    document.getElementById('det-speed').textContent = '';
    document.getElementById('det-peak').textContent = '';
    document.getElementById('det-blocks').textContent = '';
    document.getElementById('det-error').textContent = '';
    drawBlockMap(0, 0);
    return;
  }

  var si = statusInfo(t);
  fn.textContent = t.filename;
  pill.textContent = si.text;
  pill.className = 'pill';
  pill.style.background = statusColor(t) + '18';
  pill.style.color = statusColor(t);
  pill.style.display = '';
  url.textContent = t.url;
  document.getElementById('det-size').textContent = 'Size ' + formatBytes(t.total_bytes);
  document.getElementById('det-downloaded').textContent = 'Downloaded ' + formatBytes(t.downloaded);
  document.getElementById('det-conns').textContent = 'Connections ' + t.connections.length;
  document.getElementById('det-speed').textContent = 'Speed ' + formatSpeed(t.speed);
  document.getElementById('det-peak').textContent = 'Peak ' + formatSpeed(t.peak_speed);
  document.getElementById('det-blocks').textContent = 'Blocks ' + t.completed_blocks + ' / ' + t.total_blocks;
  document.getElementById('det-error').textContent = t.error || '';
  drawBlockMap(t.completed_blocks, t.total_blocks);
}

function drawBlockMap(completed, total) {
  var canvas = document.getElementById('blockmap');
  if (total === 0) { canvas.width = 0; canvas.height = 0; return; }
  var GAP = 2;
  var container = canvas.parentElement;
  var maxW = container.clientWidth || 198;
  var maxH = container.clientHeight || 160;
  if (maxW < 20) maxW = 198;
  if (maxH < 20) maxH = 160;
  var cols = Math.ceil(Math.sqrt(total));
  var rows = Math.ceil(total / cols);
  var sideByW = (maxW - (cols - 1) * GAP) / cols;
  var sideByH = (maxH - (rows - 1) * GAP) / rows;
  var SIDE = Math.floor(Math.min(sideByW, sideByH));
  if (SIDE < 2) SIDE = 2;
  var w = cols * SIDE + (cols - 1) * GAP;
  var h = rows * SIDE + (rows - 1) * GAP;
  canvas.width = w;
  canvas.height = h;
  var ctx = canvas.getContext('2d');
  var doneLeft = Math.min(completed, total);
  for (var r = 0; r < rows; r++) {
    for (var c = 0; c < cols; c++) {
      var idx = r * cols + c;
      if (idx >= total) break;
      var x = c * (SIDE + GAP);
      var y = r * (SIDE + GAP);
      ctx.fillStyle = doneLeft > 0 ? '#3ecf74' : '#34353c';
      if (doneLeft > 0) doneLeft--;
      ctx.fillRect(x, y, SIDE, SIDE);
    }
  }
}

function updateToolbar() {
  var sel = selectedId !== null;
  document.getElementById('btn-resume').disabled = !sel;
  document.getElementById('btn-pause').disabled = !sel;
  document.getElementById('btn-stop').disabled = !sel;
  document.getElementById('btn-remove').disabled = !sel;
}

function updateInfoBar() {
  var total = tasks.reduce(function(s, t) { return s + t.speed; }, 0);
  var active = tasks.filter(function(t) { return !t.done && !t.paused && t.total_bytes > 0; }).length;
  var completed = tasks.filter(function(t) { return t.status === 'Completed'; }).length;
  var queued = tasks.filter(function(t) { return t.total_bytes === 0 && !t.done && !t.paused; }).length;

  document.getElementById('info-version').textContent = appVersion ? 'zing v' + appVersion : 'zing';
  document.getElementById('info-active').textContent = active + ' active';
  document.getElementById('info-speed').textContent = formatSpeed(total);
  document.getElementById('info-complete').textContent = completed + ' completed';
  document.getElementById('info-queued').textContent = queued + ' queued';
}

function applyFilter() {
  var cat = CATEGORIES[category];
  filteredIds = tasks.filter(function(t) { return cat.matches(t); }).map(function(t) { return t.id; });
  if (selectedId !== null && filteredIds.indexOf(selectedId) === -1) {
    selectedId = null;
  }
}

// ── Polling ────────────────────────────────────────────────────

function poll() {
  invoke('list_tasks').then(function(result) {
    tasks = result;
    applyFilter();
    renderCategories();
    renderTable();
    renderDetail();
    updateToolbar();
    updateInfoBar();
  }).catch(function(e) {
    console.error('poll error:', e);
  });
}

// ── Progress window ────────────────────────────────────────────

function openProgress(id) {
  console.log('open progress for task', id);
}

// ── Init ───────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', function() {
  invoke('get_version').then(function(v) {
    appVersion = v;
  }).catch(function(e) {
    console.error('get_version failed:', e);
  });

  renderCategories();
  renderTable();
  updateToolbar();
  updateInfoBar();

  document.getElementById('btn-add').addEventListener('click', function() {
    animateBtn(this);
    openWin('add-download', 'add-download.html', 480, 600);
  });

  document.getElementById('btn-resume').addEventListener('click', function() {
    animateBtn(this);
    if (selectedId !== null) invoke('resume_task', { id: selectedId });
  });

  document.getElementById('btn-pause').addEventListener('click', function() {
    animateBtn(this);
    if (selectedId !== null) invoke('pause_task', { id: selectedId });
  });

  document.getElementById('btn-stop').addEventListener('click', function() {
    animateBtn(this);
    if (selectedId !== null) invoke('stop_task', { id: selectedId });
  });

  document.getElementById('btn-remove').addEventListener('click', function() {
    animateBtn(this);
    if (selectedId !== null) {
      invoke('remove_task', { id: selectedId }).then(function() {
        selectedId = null;
        renderTable();
        updateToolbar();
        renderDetail();
      });
    }
  });

  document.getElementById('btn-settings').addEventListener('click', function() {
    animateBtn(this);
    openWin('settings', 'settings.html', 520, 480);
  });

  poll();
  setInterval(poll, 700);
});
