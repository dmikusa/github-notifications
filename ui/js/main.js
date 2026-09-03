// js/main.js:
window.App = window.App || {};

(async () => {
  const main = document.getElementById('main');

  try {
    const state = await window.App.state.refresh();
    const auth = state.auth.authenticated ? 'configured' : 'not configured';
    main.textContent =
      `Connected. v${state.version} \u00b7 ${state.workspaces.length} workspace(s) ` +
      `\u00b7 auth: ${state.auth.provider} (${auth})` +
      (state.sync.last_sync ? ` \u00b7 last sync ${state.sync.last_sync}` : '');
  } catch (err) {
    main.textContent = `Failed to reach the daemon: ${err.message}`;
  }
})();