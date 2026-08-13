/* zing-gui add-download window logic */
var invoke = window.__TAURI__.core.invoke;

var addFilenameAuto = false;
var prevAddUrl = '';
var advOpen = false;

var BASE_H = 420;
var ADV_H = 640;
var CUSTOM_SPEED_H = 30;
var CUSTOM_UA_H = 30;

var UA_MAP = {
  'chrome': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
  'firefox': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:121.0) Gecko/20100101 Firefox/121.0',
  'safari': 'Mozilla/5.0 (Macintosh; Intel Mac OS X 14_2) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15',
  'edge': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0',
  'curl': 'curl/8.4.0',
};

function closeWin() {
  invoke('close_current_window').catch(function(e) { console.error(e); });
}

function calcHeight() {
  var h = BASE_H;
  if (advOpen) {
    h = ADV_H;
    if (document.getElementById('add-speed-limit').value === 'custom') h += CUSTOM_SPEED_H;
    if (document.getElementById('add-user-agent').value === 'custom') h += CUSTOM_UA_H;
  }
  return h;
}

function resizeWin() {
  invoke('resize_window', { label: 'add-download', width: 480, height: calcHeight() }).catch(function() {});
}

document.addEventListener('DOMContentLoaded', function() {
  var segs = document.getElementById('conn-segments');
  [0, 1, 2, 4, 8, 16, 32].forEach(function(v) {
    var btn = document.createElement('button');
    btn.className = 'segment';
    btn.textContent = v === 0 ? 'Auto' : v;
    btn.dataset.value = v;
    btn.addEventListener('click', function() {
      segs.querySelectorAll('.segment').forEach(function(b) { b.classList.remove('active'); });
      btn.classList.add('active');
    });
    if (v === 0) btn.classList.add('active');
    segs.appendChild(btn);
  });

  invoke('get_settings_dir').then(function(dlDir) {
    if (dlDir) {
      document.getElementById('add-dir').value = dlDir;
    } else {
      var fallback = window.__TAURI__ && window.__TAURI__.os ? window.__TAURI__.os.downloadDir() : '';
      if (fallback) document.getElementById('add-dir').value = fallback;
    }
  }).catch(function() {});

  document.getElementById('add-url').focus();

  document.getElementById('btn-browse-dir').addEventListener('click', function() {
    invoke('browse_folder').then(function(dir) {
      if (dir) document.getElementById('add-dir').value = dir;
    });
  });

  document.getElementById('add-url').addEventListener('input', function(e) {
    var url = e.target.value;
    if (prevAddUrl !== url) {
      prevAddUrl = url;
      var fn = document.getElementById('add-filename').value;
      if (!fn || addFilenameAuto) {
        var name = filenameFromUrl(url);
        if (name) { document.getElementById('add-filename').value = name; addFilenameAuto = true; }
      }
    }
  });

  document.getElementById('add-speed-limit').addEventListener('change', function() {
    var custom = document.getElementById('custom-speed-row');
    custom.style.display = this.value === 'custom' ? '' : 'none';
    resizeWin();
  });

  document.getElementById('add-user-agent').addEventListener('change', function() {
    var custom = document.getElementById('custom-ua-row');
    custom.style.display = this.value === 'custom' ? '' : 'none';
    resizeWin();
  });

  document.getElementById('adv-toggle').addEventListener('click', function() {
    var panel = document.getElementById('adv-panel');
    var arrow = document.getElementById('adv-arrow');
    advOpen = panel.style.display === 'none';
    panel.style.display = advOpen ? '' : 'none';
    arrow.textContent = advOpen ? '\u25be' : '\u25b8';
    resizeWin();
  });

  document.getElementById('btn-add-cancel').addEventListener('click', function() {
    this.classList.add('btn-press');
    setTimeout(closeWin, 80);
  });

  document.getElementById('btn-add-submit').addEventListener('click', function() {
    this.classList.add('btn-press');
    var self = this;
    setTimeout(function() { self.classList.remove('btn-press'); }, 150);
    submitAddUrl();
  });
});

function submitAddUrl() {
  var url = document.getElementById('add-url').value.trim();
  if (!url) return;
  var params = { url: url };
  var filename = document.getElementById('add-filename').value;
  if (filename) params.filename = filename;
  var dir = document.getElementById('add-dir').value;
  if (dir) params.dir = dir;
  var activeSeg = document.querySelector('.segment.active');
  var conns = activeSeg ? parseInt(activeSeg.dataset.value) : 0;
  if (conns > 0) params.connections = conns;

  var speedSel = document.getElementById('add-speed-limit').value;
  if (speedSel === 'custom') {
    var customVal = document.getElementById('add-speed-custom').value;
    if (customVal) params.max_download_rate = customVal;
  } else if (speedSel) {
    params.max_download_rate = speedSel;
  }

  var uaSel = document.getElementById('add-user-agent').value;
  if (uaSel === 'custom') {
    var customUa = document.getElementById('add-ua-custom').value;
    if (customUa) params.headers = ['User-Agent: ' + customUa];
  } else if (uaSel && UA_MAP[uaSel]) {
    params.headers = ['User-Agent: ' + UA_MAP[uaSel]];
  }

  var proxy = document.getElementById('add-proxy').value;
  if (proxy) params.proxy = proxy;
  var mirror = document.getElementById('add-mirror').value;
  if (mirror) params.mirror = [mirror];
  if (document.getElementById('add-insecure').checked) params.insecure = true;
  if (document.getElementById('add-overwrite').checked) params.allow_overwrite = true;

  invoke('add_uri', { params: params }).then(function() {
    closeWin();
  }).catch(function() {
    var urlField = document.getElementById('add-url');
    urlField.style.borderColor = 'var(--danger)';
    urlField.style.animation = 'shake 0.3s ease';
    setTimeout(function() {
      urlField.style.borderColor = '';
      urlField.style.animation = '';
    }, 2000);
  });
}

function filenameFromUrl(url) {
  try {
    var u = new URL(url);
    var name = u.pathname.split('/').pop();
    return name ? decodeURIComponent(name) : null;
  } catch (e) { return null; }
}
