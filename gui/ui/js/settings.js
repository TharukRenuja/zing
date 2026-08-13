/* zing-gui settings logic */
var invoke = window.__TAURI__.core.invoke;

var currentTheme = localStorage.getItem('zing-theme') || 'dark';

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

function closeWin() {
  invoke('close_current_window').catch(function(e) { console.error(e); });
}

document.addEventListener('DOMContentLoaded', function() {
  invoke('get_settings_dir').then(function(dir) {
    document.getElementById('settings-dir').value = dir;
  }).catch(function(e) { console.error(e); });

  invoke('get_version').then(function(ver) {
    document.getElementById('settings-version').textContent = ver ? 'v' + ver : '';
  }).catch(function(e) { console.error(e); });

  applyTheme(currentTheme);

  document.querySelectorAll('#theme-group .radio-option').forEach(function(btn) {
    btn.addEventListener('click', function() {
      applyTheme(btn.dataset.theme);
      btn.classList.add('btn-press');
      setTimeout(function() { btn.classList.remove('btn-press'); }, 150);
    });
  });

  document.getElementById('btn-browse').addEventListener('click', function() {
    invoke('browse_folder').then(function(dir) {
      if (dir) document.getElementById('settings-dir').value = dir;
    });
  });

  document.getElementById('btn-save').addEventListener('click', function() {
    var dir = document.getElementById('settings-dir').value;
    invoke('save_settings_dir', { dir: dir }).then(function() {
      localStorage.setItem('zing-theme', currentTheme);
      closeWin();
    });
  });

  document.getElementById('btn-cancel').addEventListener('click', function() {
    closeWin();
  });
});
