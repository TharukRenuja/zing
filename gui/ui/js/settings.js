/* zing-gui settings logic */
var invoke = window.__TAURI__.core.invoke;

var currentTheme = localStorage.getItem('zing-theme') || 'dark';
var currentAccent = localStorage.getItem('zing-accent') || '#5b7fff';
var currentSidebarMode = localStorage.getItem('zing-sidebar-mode') || 'full';
var currentFontSize = localStorage.getItem('zing-font-size') || 'md';

function applyTheme(theme) {
  if (theme === 'dark') {
    document.documentElement.removeAttribute('data-theme');
  } else {
    document.documentElement.setAttribute('data-theme', theme);
  }
  currentTheme = theme;
  document.querySelectorAll('#theme-group .radio-option').forEach(function(btn) {
    btn.classList.toggle('active', btn.dataset.theme === theme);
  });
}

function applyAccent(color) {
  currentAccent = color;
  document.documentElement.style.setProperty('--accent', color);
  var r = parseInt(color.slice(1,3), 16);
  var g = parseInt(color.slice(3,5), 16);
  var b = parseInt(color.slice(5,7), 16);
  document.documentElement.style.setProperty('--accent-soft', 'rgba(' + r + ',' + g + ',' + b + ', 0.15)');
  document.documentElement.style.setProperty('--accent-hover', 'rgb(' + Math.min(255, r+20) + ',' + Math.min(255, g+20) + ',' + Math.min(255, b+20) + ')');
  document.documentElement.style.setProperty('--accent-active', 'rgb(' + Math.max(0, r-30) + ',' + Math.max(0, g-30) + ',' + Math.max(0, b-30) + ')');
  document.querySelectorAll('#accent-swatches .color-swatch').forEach(function(s) {
    s.classList.toggle('active', s.dataset.color === color);
  });
}

function applyFontSize(size) {
  currentFontSize = size;
  var sizes = { sm: '13px', md: '14px', lg: '16px' };
  document.body.style.fontSize = sizes[size] || '14px';
  document.querySelectorAll('#font-size-group .segment').forEach(function(btn) {
    btn.classList.toggle('active', btn.dataset.size === size);
  });
}

function applySidebarMode(mode) {
  currentSidebarMode = mode;
  document.querySelectorAll('#sidebar-mode-group .radio-option').forEach(function(btn) {
    btn.classList.toggle('active', btn.dataset.mode === mode);
  });
}

function closeWin() {
  invoke('close_current_window').catch(function(e) { console.error(e); });
}

var RATE_OPTIONS = [
  { value: '', label: 'Unlimited' },
  { value: '100KB', label: '100 KB/s' },
  { value: '500KB', label: '500 KB/s' },
  { value: '1MB', label: '1 MB/s' },
  { value: '2MB', label: '2 MB/s' },
  { value: '5MB', label: '5 MB/s' },
  { value: '10MB', label: '10 MB/s' },
  { value: '50MB', label: '50 MB/s' },
  { value: '100MB', label: '100 MB/s' }
];

function addSlotRow(time, rate) {
  var container = document.getElementById('bwlimit-slots');
  var row = document.createElement('div');
  row.style.cssText = 'display:flex;align-items:center;gap:6px;';
  var fromInput = document.createElement('input');
  fromInput.type = 'time';
  fromInput.className = 'field-input';
  fromInput.value = time || '00:00';
  fromInput.style.width = '120px';
  var sep = document.createElement('span');
  sep.textContent = '\u2014';
  sep.style.color = 'var(--text-weak)';
  var toInput = document.createElement('input');
  toInput.type = 'time';
  toInput.className = 'field-input';
  toInput.value = time || '23:59';
  toInput.style.width = '120px';
  var rateSelect = document.createElement('select');
  rateSelect.className = 'field-input';
  rateSelect.style.width = '120px';
  RATE_OPTIONS.forEach(function(opt) {
    var o = document.createElement('option');
    o.value = opt.value;
    o.textContent = opt.label;
    if (opt.value === (rate || '')) o.selected = true;
    rateSelect.appendChild(o);
  });
  var removeBtn = document.createElement('button');
  removeBtn.className = 'btn btn-ghost';
  removeBtn.innerHTML = '<svg class="btn-icon" viewBox="0 0 24 24"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>';
  removeBtn.title = 'Remove';
  removeBtn.addEventListener('click', function() { row.remove(); });
  row.appendChild(fromInput);
  row.appendChild(sep);
  row.appendChild(toInput);
  row.appendChild(rateSelect);
  row.appendChild(removeBtn);
  container.appendChild(row);
}


document.addEventListener('DOMContentLoaded', function() {
  // Custom number input +/- buttons
  document.querySelectorAll('.num-input button[data-step]').forEach(function(btn) {
    btn.addEventListener('click', function() {
      var input = document.getElementById(btn.dataset.target);
      if (!input) return;
      var step = parseFloat(btn.dataset.step) || 1;
      var min = input.hasAttribute('min') ? parseFloat(input.min) : -Infinity;
      var val = parseFloat(input.value) || 0;
      val = Math.max(min, val + step);
      input.value = val;
      input.dispatchEvent(new Event('change'));
    });
  });

  // Load backend config
  invoke('get_config').then(function(cfg) {
    if (cfg.download_dir) {
      document.getElementById('cfg-dir').value = cfg.download_dir;
    } else {
      // Auto-detect default download directory
      invoke('get_default_download_dir').then(function(dir) {
        document.getElementById('cfg-dir').value = dir;
      }).catch(function() {});
    }
    if (cfg.max_concurrent_downloads != null) document.getElementById('cfg-max-concurrent').value = cfg.max_concurrent_downloads;
    if (cfg.sched_max_concurrent != null) document.getElementById('cfg-sched-max-concurrent').value = cfg.sched_max_concurrent;
    if (cfg.default_connections) document.getElementById('cfg-connections').value = cfg.default_connections;
    if (cfg.connect_timeout) document.getElementById('cfg-connect-timeout').value = cfg.connect_timeout;
    if (cfg.max_transfer_time) document.getElementById('cfg-max-time').value = cfg.max_transfer_time;
    if (cfg.retry_count != null) document.getElementById('cfg-retry').value = cfg.retry_count;
    if (cfg.retry_wait_ms != null) document.getElementById('cfg-retry-wait').value = cfg.retry_wait_ms;
    if (cfg.default_proxy) document.getElementById('cfg-proxy').value = cfg.default_proxy;
    if (cfg.active_hours_from) document.getElementById('cfg-active-from').value = cfg.active_hours_from;
    if (cfg.active_hours_to) document.getElementById('cfg-active-to').value = cfg.active_hours_to;
    if (cfg.default_rate_limit) {
      var sel = document.getElementById('cfg-rate-limit');
      var found = false;
      for (var i = 0; i < sel.options.length; i++) {
        if (sel.options[i].value === cfg.default_rate_limit) { sel.selectedIndex = i; found = true; break; }
      }
      if (!found && cfg.default_rate_limit) {
        sel.value = 'custom';
        document.getElementById('cfg-rate-limit-custom').value = cfg.default_rate_limit;
        document.getElementById('cfg-rate-limit-custom').style.display = '';
      }
    }
    // General toggles
    setToggle('cfg-prompt-location', cfg.prompt_location);
    setToggle('cfg-update-check', cfg.update_check_interval_days !== 0);
    setToggle('cfg-clipboard-monitor', cfg.clipboard_monitor);
    // Bandwidth schedule slots
    if (cfg.bwlimit_schedule) {
      var slots = cfg.bwlimit_schedule.trim().split(/\s+/);
      slots.forEach(function(slot) {
        var parts = slot.split(',');
        if (parts.length === 2) addSlotRow(parts[0], parts[1]);
      });
    }
    // Toggles
    setToggle('cfg-end-game', cfg.end_game);
    setToggle('cfg-throttle-reprobe', cfg.throttle_reprobe);
    setToggle('cfg-auto-rename', cfg.auto_rename !== false);
    setToggle('cfg-overwrite', cfg.allow_overwrite);
    setToggle('cfg-content-disposition', cfg.content_disposition);
    setToggle('cfg-download-categories', cfg.download_categories !== false);
    if (cfg.post_download_action) {
      document.getElementById('cfg-post-action').value = cfg.post_download_action;
    }
  }).catch(function(e) { console.error(e); });

  // Version
  invoke('get_version').then(function(ver) {
    document.getElementById('about-version').textContent = ver ? 'v' + ver : '';
  }).catch(function(e) { console.error(e); });

  // Apply appearance
  applyTheme(currentTheme);
  applyAccent(currentAccent);
  applyFontSize(currentFontSize);
  applySidebarMode(currentSidebarMode);

  // Section nav
  document.querySelectorAll('.settings-nav-item').forEach(function(btn) {
    btn.addEventListener('click', function() {
      var section = btn.dataset.section;
      document.querySelectorAll('.settings-nav-item').forEach(function(b) { b.classList.remove('active'); });
      btn.classList.add('active');
      document.querySelectorAll('.settings-section').forEach(function(s) { s.classList.remove('active'); });
      document.getElementById('sec-' + section).classList.add('active');
    });
  });

  // Theme radio
  document.querySelectorAll('#theme-group .radio-option').forEach(function(btn) {
    btn.addEventListener('click', function() {
      applyTheme(btn.dataset.theme);
    });
  });

  // Accent swatches
  document.querySelectorAll('#accent-swatches .color-swatch').forEach(function(s) {
    s.addEventListener('click', function() {
      applyAccent(s.dataset.color);
    });
  });

  // Sidebar mode
  document.querySelectorAll('#sidebar-mode-group .radio-option').forEach(function(btn) {
    btn.addEventListener('click', function() {
      applySidebarMode(btn.dataset.mode);
    });
  });

  // Font size
  document.querySelectorAll('#font-size-group .segment').forEach(function(btn) {
    btn.addEventListener('click', function() {
      applyFontSize(btn.dataset.size);
    });
  });

  // Backend toggles
  document.querySelectorAll('.toggle[data-key]').forEach(function(toggle) {
    toggle.addEventListener('click', function() {
      toggle.classList.toggle('on');
    });
  });

  // Rate limit custom
  document.getElementById('cfg-rate-limit').addEventListener('change', function() {
    document.getElementById('cfg-rate-limit-custom').style.display = this.value === 'custom' ? '' : 'none';
  });

  // Add bandwidth slot
  document.getElementById('btn-add-slot').addEventListener('click', function() {
    addSlotRow('00:00', '');
  });

  // Browse
  document.getElementById('btn-browse').addEventListener('click', function() {
    invoke('browse_folder').then(function(dir) {
      if (dir) document.getElementById('cfg-dir').value = dir;
    });
  });

  // Update check
  document.getElementById('btn-check-update').addEventListener('click', function() {
    var btn = this;
    var status = document.getElementById('update-status');
    btn.disabled = true;
    btn.textContent = 'Checking...';
    status.style.display = '';
    status.textContent = 'Checking for updates...';
    status.style.color = 'var(--text-weak)';
    document.getElementById('update-confirm').style.display = 'none';
    invoke('update_check').then(function(out) {
      status.textContent = out;
      if (out && !out.includes('up to date')) {
        status.style.color = 'var(--accent)';
        document.getElementById('update-confirm').style.display = '';
      } else {
        status.style.color = 'var(--text-weak)';
      }
    }).catch(function(e) {
      status.textContent = 'Update check failed: ' + e;
      status.style.color = 'var(--danger)';
    }).finally(function() {
      btn.disabled = false;
      btn.innerHTML = '<svg class="btn-icon" viewBox="0 0 24 24"><path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16"/><path d="M16 16h5v5"/></svg> Check for Updates';
    });
  });

  // Restart button
  document.getElementById('btn-restart').addEventListener('click', function() {
    saveAll().then(function() {
      invoke('update_check').then(function() {
        window.__TAURI__.process.exit(0);
      }).catch(function() {
        window.__TAURI__.process.exit(0);
      });
    });
  });

  // Dismiss
  document.getElementById('btn-dismiss').addEventListener('click', function() {
    document.getElementById('update-confirm').style.display = 'none';
  });

  // Save
  document.getElementById('btn-save').addEventListener('click', function() {
    saveAll().then(function() { closeWin(); });
  });

  // Cancel
  document.getElementById('btn-cancel').addEventListener('click', function() {
    closeWin();
  });
});

function setToggle(id, val) {
  var el = document.getElementById(id);
  if (el && val === true) el.classList.add('on');
}

function saveAll() {
  // Save appearance to localStorage
  localStorage.setItem('zing-theme', currentTheme);
  localStorage.setItem('zing-accent', currentAccent);
  localStorage.setItem('zing-sidebar-mode', currentSidebarMode);
  localStorage.setItem('zing-font-size', currentFontSize);

  // Save backend config
  var saves = [];
  saves.push(invoke('save_settings_dir', { dir: document.getElementById('cfg-dir').value }));
  saves.push(invoke('set_config', { key: 'prompt_location', value: document.getElementById('cfg-prompt-location').classList.contains('on') }));
  saves.push(invoke('set_config', { key: 'update_check_interval_days', value: document.getElementById('cfg-update-check').classList.contains('on') ? 7 : 0 }));
  saves.push(invoke('set_config', { key: 'clipboard_monitor', value: document.getElementById('cfg-clipboard-monitor').classList.contains('on') }));
  saves.push(invoke('set_config', { key: 'max_concurrent_downloads', value: parseInt(document.getElementById('cfg-max-concurrent').value) || 0 }));
  saves.push(invoke('set_config', { key: 'sched_max_concurrent', value: parseInt(document.getElementById('cfg-sched-max-concurrent').value) || 3 }));
  saves.push(invoke('set_config', { key: 'default_connections', value: parseInt(document.getElementById('cfg-connections').value) || 16 }));
  saves.push(invoke('set_config', { key: 'connect_timeout', value: parseInt(document.getElementById('cfg-connect-timeout').value) || 30 }));
  saves.push(invoke('set_config', { key: 'max_transfer_time', value: parseInt(document.getElementById('cfg-max-time').value) || 300 }));
  saves.push(invoke('set_config', { key: 'retry_count', value: parseInt(document.getElementById('cfg-retry').value) || 5 }));
  saves.push(invoke('set_config', { key: 'retry_wait_ms', value: parseInt(document.getElementById('cfg-retry-wait').value) || 500 }));
  saves.push(invoke('set_config', { key: 'default_proxy', value: document.getElementById('cfg-proxy').value || null }));
  saves.push(invoke('set_config', { key: 'active_hours_from', value: document.getElementById('cfg-active-from').value || null }));
  saves.push(invoke('set_config', { key: 'active_hours_to', value: document.getElementById('cfg-active-to').value || null }));
  var rateSel = document.getElementById('cfg-rate-limit');
  var rateVal = rateSel.value === 'custom' ? document.getElementById('cfg-rate-limit-custom').value : rateSel.value;
  saves.push(invoke('set_config', { key: 'default_rate_limit', value: rateVal || null }));
  // Parse bandwidth schedule slots
  var slotRows = document.getElementById('bwlimit-slots').children;
  var scheduleParts = [];
  for (var i = 0; i < slotRows.length; i++) {
    var inputs = slotRows[i].querySelectorAll('input[type="time"]');
    var sel = slotRows[i].querySelector('select');
    if (inputs.length === 2 && sel) {
      var from = inputs[0].value;
      var rate = sel.value;
      if (rate) scheduleParts.push(from + ',' + rate);
    }
  }
  saves.push(invoke('set_config', { key: 'bwlimit_schedule', value: scheduleParts.length ? scheduleParts.join(' ') : null }));
  saves.push(invoke('set_config', { key: 'end_game', value: document.getElementById('cfg-end-game').classList.contains('on') }));
  saves.push(invoke('set_config', { key: 'throttle_reprobe', value: document.getElementById('cfg-throttle-reprobe').classList.contains('on') }));
  saves.push(invoke('set_config', { key: 'auto_rename', value: document.getElementById('cfg-auto-rename').classList.contains('on') }));
  saves.push(invoke('set_config', { key: 'allow_overwrite', value: document.getElementById('cfg-overwrite').classList.contains('on') }));
  saves.push(invoke('set_config', { key: 'content_disposition', value: document.getElementById('cfg-content-disposition').classList.contains('on') }));
  saves.push(invoke('set_config', { key: 'download_categories', value: document.getElementById('cfg-download-categories').classList.contains('on') }));
  saves.push(invoke('set_config', { key: 'post_download_action', value: document.getElementById('cfg-post-action').value || 'none' }));
  return Promise.all(saves)
    .then(function() { return invoke('notify_settings_changed'); })
    .catch(function(e) { console.error('Save error:', e); });
}
