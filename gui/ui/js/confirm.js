/* zing-gui confirm shell logic */
var invoke = window.__TAURI__.core.invoke;

function esc(s) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function truncateUrl(url, max) {
  max = max || 80;
  if (url.length <= max) return url;
  return url.substring(0, max - 3) + '...';
}

function poll() {
  invoke('pending_confirmations').then(function(pending) {
    var list = document.getElementById('pending-list');
    var empty = document.getElementById('empty-msg');

    if (pending.length === 0) {
      list.innerHTML = '';
      empty.style.display = '';
      // Auto-close: no pending items left
      setTimeout(function() {
        invoke('close_current_window').catch(function() {});
      }, 1500);
      return;
    }

    empty.style.display = 'none';
    list.innerHTML = pending.map(function(p) {
      return '<div class="confirm-card" data-id="' + p.pending_id + '">' +
        '<div class="confirm-filename">' + esc(p.filename || 'Unknown file') + '</div>' +
        '<div class="confirm-meta">' +
          '<div class="confirm-meta-row"><span class="label">URL</span><span class="value" title="' + esc(p.url) + '">' + esc(truncateUrl(p.url)) + '</span></div>' +
          '<div class="confirm-meta-row"><span class="label">Save to</span><span class="value">' + esc(p.dir || '~/Downloads') + '</span></div>' +
        '</div>' +
        '<div class="confirm-actions">' +
          '<button data-action="cancel">Cancel</button>' +
          '<button data-action="schedule" class="btn-schedule" disabled title="Coming soon">Schedule</button>' +
          '<button data-action="confirm" class="btn-confirm">Confirm</button>' +
        '</div>' +
      '</div>';
    }).join('');

    list.querySelectorAll('.confirm-card').forEach(function(card) {
      var id = parseInt(card.dataset.id);

      card.querySelector('[data-action="confirm"]').addEventListener('click', function() {
        invoke('confirm_uri', { pendingId: id }).then(function() { poll(); });
      });

      card.querySelector('[data-action="cancel"]').addEventListener('click', function() {
        invoke('deny_uri', { pendingId: id }).then(function() { poll(); });
      });
    });
  }).catch(function(e) {
    console.error('poll error:', e);
  });
}

document.addEventListener('DOMContentLoaded', function() {
  poll();
  setInterval(poll, 2000);
});
