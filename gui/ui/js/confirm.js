/* zing-gui confirm shell logic */
var invoke = window.__TAURI__.core.invoke;

var currentPending = [];
var existsCallback = null;

function closeWin() {
  invoke('close_current_window').catch(function(e) { console.error(e); });
}

function esc(s) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function splitFilename(name) {
  var dot = name.lastIndexOf('.');
  if (dot <= 0) return { base: name, ext: '' };
  return { base: name.substring(0, dot), ext: name.substring(dot) };
}

function showExistsPopup(filename, cb) {
  var parts = splitFilename(filename);
  document.getElementById('exists-filename').textContent = filename;
  document.getElementById('exists-rename-name').textContent = parts.base + '-1' + parts.ext;
  document.getElementById('exists-overlay').style.display = '';
  existsCallback = cb;
}

function hideExistsPopup() {
  document.getElementById('exists-overlay').style.display = 'none';
  existsCallback = null;
}

function poll() {
  invoke('pending_confirmations').then(function(pending) {
    currentPending = pending;
    var list = document.getElementById('pending-list');
    var empty = document.getElementById('empty-msg');
    if (pending.length === 0) {
      list.innerHTML = '';
      empty.style.display = '';
      return;
    }
    empty.style.display = 'none';
    list.innerHTML = pending.map(function(p) {
      return '<div class="confirm-card" data-id="' + p.pending_id + '">' +
        '<div class="confirm-filename">' + esc(p.filename) + '</div>' +
        '<div class="confirm-url">' + esc(p.url) + '</div>' +
        '<div class="confirm-dir">Save to: ' + esc(p.dir) + '</div>' +
        '<div class="confirm-actions">' +
          '<button class="btn btn-ghost" data-action="deny">Deny</button>' +
          '<button class="btn btn-primary" data-action="confirm">Confirm</button>' +
        '</div>' +
      '</div>';
    }).join('');

    list.querySelectorAll('.confirm-card').forEach(function(item) {
      var id = parseInt(item.dataset.id);
      item.querySelector('[data-action="confirm"]').addEventListener('click', function() {
        showExistsPopup(
          pending.find(function(p) { return p.pending_id === id; }).filename,
          function(overwrite, newName) {
            var params = { pendingId: id };
            if (overwrite) params.overwrite = true;
            if (newName) params.filename = newName;
            invoke('confirm_uri', params).then(function() { poll(); });
          }
        );
      });
      item.querySelector('[data-action="deny"]').addEventListener('click', function() {
        invoke('deny_uri', { pendingId: id }).then(function() { poll(); });
      });
    });
  }).catch(function(e) {
    console.error('poll error:', e);
  });
}

document.addEventListener('DOMContentLoaded', function() {
  document.getElementById('btn-exists-overwrite').addEventListener('click', function() {
    if (existsCallback) existsCallback(true, null);
    hideExistsPopup();
  });

  document.getElementById('btn-exists-rename').addEventListener('click', function() {
    if (existsCallback) {
      var item = currentPending.find(function(p) {
        return document.getElementById('exists-filename').textContent === p.filename;
      });
      if (item) {
        var parts = splitFilename(item.filename);
        var newName = parts.base + '-1' + parts.ext;
        existsCallback(false, newName);
      }
    }
    hideExistsPopup();
  });

  document.getElementById('btn-exists-cancel').addEventListener('click', function() {
    hideExistsPopup();
  });

  document.getElementById('exists-overlay').addEventListener('click', function(e) {
    if (e.target === this) hideExistsPopup();
  });

  poll();
  setInterval(poll, 2000);
});
