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

// ── Appearance helpers ───────────────────────────────────────────

function applyTheme(theme) {
  if (theme === 'dark') {
    document.documentElement.removeAttribute('data-theme');
  } else {
    document.documentElement.setAttribute('data-theme', theme);
  }
}

function applyAccent(color) {
  document.documentElement.style.setProperty('--accent', color);
  var r = parseInt(color.slice(1,3), 16);
  var g = parseInt(color.slice(3,5), 16);
  var b = parseInt(color.slice(5,7), 16);
  document.documentElement.style.setProperty('--accent-soft', 'rgba(' + r + ',' + g + ',' + b + ', 0.15)');
  document.documentElement.style.setProperty('--accent-hover', 'rgb(' + Math.min(255, r+20) + ',' + Math.min(255, g+20) + ',' + Math.min(255, b+20) + ')');
  document.documentElement.style.setProperty('--accent-active', 'rgb(' + Math.max(0, r-30) + ',' + Math.max(0, g-30) + ',' + Math.max(0, b-30) + ')');
}

function applyFontSize(size) {
  var sizes = { sm: '13px', md: '14px', lg: '16px' };
  document.body.style.fontSize = sizes[size] || '14px';
}

function applySidebarMode(mode) {
  document.body.classList.toggle('sidebar-icons-only', mode === 'icons');
}

const CATEGORIES = [
  { label: 'All Downloads', matches: function() { return true; } },
  { label: 'Downloading', matches: function(t) { return !t.done && !t.paused && t.total_bytes > 0; } },
  { label: 'Complete', matches: function(t) { return t.status === 'Completed'; } },
  { label: 'Paused', matches: function(t) { return t.paused; } },
  { label: 'Queued', matches: function(t) { return t.total_bytes === 0 && !t.done && !t.paused; } },
  { label: 'Failed', matches: function(t) { return t.status.startsWith('Failed'); } },
  { label: 'Stopped', matches: function(t) { return t.status === 'Stopped'; } },
  { label: '---', matches: function() { return false; } },
  { label: 'Music', matches: function(t) { return /\.(mp3|flac|wav|aac|ogg|m4a|wma|opus)$/i.test(t.filename || t.url); } },
  { label: 'Video', matches: function(t) { return /\.(mp4|mkv|avi|mov|webm|flv|wmv|3gp|m4v)$/i.test(t.filename || t.url); } },
  { label: 'Images', matches: function(t) { return /\.(jpg|jpeg|png|gif|webp|svg|bmp|ico|tiff?)$/i.test(t.filename || t.url); } },
  { label: 'Documents', matches: function(t) { return /\.(pdf|doc|docx|xls|xlsx|ppt|pptx|txt|csv|epub)$/i.test(t.filename || t.url); } },
  { label: 'Compressed', matches: function(t) { return /\.(zip|rar|7z|tar|gz|bz2|xz|zst|tgz)$/i.test(t.filename || t.url); } },
  { label: 'Programs', matches: function(t) { return /\.(exe|msi|dmg|app|deb|rpm|appimage|pkg|snap)$/i.test(t.filename || t.url); } },
];

var CATEGORY_ICONS = [
  '<svg viewBox="0 0 24 24"><rect width="7" height="7" x="3" y="3" rx="1"/><rect width="7" height="7" x="14" y="3" rx="1"/><rect width="7" height="7" x="14" y="14" rx="1"/><rect width="7" height="7" x="3" y="14" rx="1"/></svg>',
  '<svg viewBox="0 0 24 24"><path d="M12 15V3"/><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/></svg>',
  '<svg viewBox="0 0 24 24"><path d="M21.801 10A10 10 0 1 1 17 3.335"/><path d="m9 11 3 3L22 4"/></svg>',
  '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><line x1="10" x2="10" y1="15" y2="9"/><line x1="14" x2="14" y1="15" y2="9"/></svg>',
  '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/></svg>',
  '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><line x1="12" x2="12" y1="8" y2="12"/><line x1="12" x2="12.01" y1="16" y2="16"/></svg>',
  '<svg viewBox="0 0 24 24"><path d="M4 6h16"/><path d="M4 12h16"/><path d="M4 18h16"/><path d="m8 6 8 12"/><path d="m16 6-8 12"/></svg>',
  '',
  '<svg viewBox="0 0 24 24"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>',
  '<svg viewBox="0 0 24 24"><path d="m22 8-6 4 6 4V8Z"/><rect width="14" height="12" x="2" y="6" rx="2" ry="2"/></svg>',
  '<svg viewBox="0 0 24 24"><rect width="18" height="18" x="3" y="3" rx="2" ry="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"/></svg>',
  '<svg viewBox="0 0 24 24"><path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z"/><polyline points="14 2 14 8 20 8"/></svg>',
  '<svg viewBox="0 0 24 24"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>',
  '<svg viewBox="0 0 24 24"><path d="M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2"/><path d="M7 11l5 5 5-5"/><path d="M12 4v12"/></svg>',
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
    if (cat.label === '---') return '<div class="cat-separator"></div>';
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
    var prevCount = tasks.length;
    tasks = result;
    applyFilter();
    renderCategories();
    renderTable();
    renderDetail();
    updateToolbar();
    updateInfoBar();
    // Check if all downloads complete (post-download action)
    if (tasks.length > 0 && prevCount > 0) {
      var allDone = tasks.every(function(t) { return t.done || t.paused; });
      var wasComplete = prevCount > 0 && tasks.every(function(t) { return t.done || t.paused; });
      if (allDone && !wasComplete) {
        invoke('execute_post_action').catch(function() {});
      }
    }
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
  // Apply saved appearance
  applySidebarMode(localStorage.getItem('zing-sidebar-mode') || 'full');

  // Drag and drop URL support
  var dragCounter = 0;
  document.addEventListener('dragenter', function(e) {
    e.preventDefault();
    dragCounter++;
    document.body.style.outline = '2px dashed var(--accent)';
    document.body.style.outlineOffset = '-4px';
  });

  document.addEventListener('dragleave', function(e) {
    e.preventDefault();
    dragCounter--;
    if (dragCounter <= 0) {
      dragCounter = 0;
      document.body.style.outline = '';
      document.body.style.outlineOffset = '';
    }
  });

  document.addEventListener('dragover', function(e) {
    e.preventDefault();
  });

  document.addEventListener('drop', function(e) {
    e.preventDefault();
    dragCounter = 0;
    document.body.style.outline = '';
    document.body.style.outlineOffset = '';

    var url = '';
    if (e.dataTransfer.urls && e.dataTransfer.urls.length > 0) {
      url = e.dataTransfer.urls[0];
    } else if (e.dataTransfer.getData('text/plain')) {
      url = e.dataTransfer.getData('text/plain').trim();
    }

    if (url && url.startsWith('http')) {
      openWin('add-download', 'add-download.html?url=' + encodeURIComponent(url), 480, 600);
    }
  });

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
    openWin('settings', 'settings.html', 780, 680);
  });

  // Listen for settings changes from the settings window (cross-window localStorage)
  window.addEventListener('storage', function(e) {
    if (!e.key || !e.key.startsWith('zing-')) return;
    if (e.key === 'zing-theme') applyTheme(e.newValue || 'dark');
    if (e.key === 'zing-accent') applyAccent(e.newValue || '#5b7fff');
    if (e.key === 'zing-font-size') applyFontSize(e.newValue || 'md');
    if (e.key === 'zing-sidebar-mode') applySidebarMode(e.newValue || 'full');
  });

  // Clipboard URL detection
  var toastEl = document.getElementById('clipboard-toast');
  var toastUrl = '';
  var toastTimeout = null;

  function showClipboardToast(url) {
    toastUrl = url;
    document.getElementById('clipboard-url').textContent = url;
    toastEl.style.display = '';
    if (toastTimeout) clearTimeout(toastTimeout);
    toastTimeout = setTimeout(function() { toastEl.style.display = 'none'; }, 8000);
  }

  document.getElementById('toast-download').addEventListener('click', function() {
    toastEl.style.display = 'none';
    if (toastTimeout) clearTimeout(toastTimeout);
    if (toastUrl) {
      openWin('add-download', 'add-download.html?url=' + encodeURIComponent(toastUrl), 480, 600);
    }
  });

  document.getElementById('toast-dismiss').addEventListener('click', function() {
    toastEl.style.display = 'none';
    if (toastTimeout) clearTimeout(toastTimeout);
  });

  window.__TAURI__.event.listen('clipboard-url', function(e) {
    showClipboardToast(e.payload);
  });

  // Start clipboard monitor if enabled
  invoke('get_config').then(function(cfg) {
    if (cfg.clipboard_monitor) invoke('start_clipboard_monitor');
  }).catch(function() {});

  poll();
  setInterval(poll, 700);
});
